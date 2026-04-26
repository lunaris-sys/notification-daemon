/// Icon hint extraction for `org.freedesktop.Notifications`.
///
/// The FDO spec defines three ways an app can send an icon:
///
/// 1. `image-data` hint — raw pixel buffer
///    `(width, height, rowstride, has_alpha, bits_per_sample, channels, data)`.
///    Chromium, Firefox, Electron apps (Discord / Slack / VS Code)
///    use this for per-notification icons (tab favicons, message avatars).
/// 2. `image-path` hint — absolute filesystem path to an image.
///    Also commonly populated by chromium-based apps.
/// 3. `app_icon` positional argument — freedesktop icon name (e.g. `firefox`).
///    The oldest / weakest signal.
///
/// Priority per spec 1.2: `image-data` > `image-path` > `app_icon`.
/// We additionally fall back to the legacy `image_data` / `image_path`
/// (with underscore) names that older apps still ship.
///
/// The output is normalised to a single string that downstream code
/// (shell's `resolve_icon` in `notifications/client.rs`) already
/// understands: a `data:image/png;base64,…` URI for raw pixel hints,
/// an absolute path for file hints, or the bare icon name as fallback.

use std::collections::HashMap;
use std::io::Cursor;

use base64::Engine;
use image::{ImageBuffer, Rgba};
use zbus::zvariant::{OwnedValue, Structure, Value};

/// Maximum accepted `image-data` dimensions. A 512-pixel edge holds
/// any realistic notification icon (HiDPI panel cells top out around
/// 48-96 px), and rejecting larger images bounds our PNG-encode work
/// against a malicious or buggy sender.
const MAX_IMAGE_DIMENSION: i32 = 512;

/// Resolves the best-available icon from D-Bus hints + the positional
/// `app_icon` argument. The returned string is already in the shape the
/// shell's `notifications/client.rs::resolve_icon` expects:
///
/// - `"data:image/png;base64,..."` when the app sent a raw pixel buffer
/// - an absolute path like `"/usr/share/icons/.../firefox.png"` when
///   the app sent `image-path`
/// - the bare `app_icon` argument otherwise (`"firefox"` etc.)
///
/// Never panics on malformed hints — on any decode error the next
/// priority source is tried; the worst case is an empty string.
pub fn resolve_icon(hints: &HashMap<String, OwnedValue>, app_icon_arg: &str) -> String {
    // 1. image-data / image_data — raw pixels.
    if let Some(url) = hints
        .get("image-data")
        .or_else(|| hints.get("image_data"))
        .and_then(try_decode_image_data)
    {
        return url;
    }

    // 2. image-path / image_path — absolute file path.
    if let Some(path) = hints
        .get("image-path")
        .or_else(|| hints.get("image_path"))
        .and_then(extract_string)
    {
        if !path.is_empty() {
            return path;
        }
    }

    // 3. Positional app_icon argument.
    app_icon_arg.to_string()
}

/// Extract a `String` from an `OwnedValue` that wraps `Value::Str`.
fn extract_string(value: &OwnedValue) -> Option<String> {
    match &**value {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

/// Decode the FDO image-data struct and return a `data:image/png;base64,…` URI.
fn try_decode_image_data(value: &OwnedValue) -> Option<String> {
    let structure: &Structure = match &**value {
        Value::Structure(s) => s,
        _ => return None,
    };

    let fields = structure.fields();
    if fields.len() < 7 {
        return None;
    }

    let width = as_i32(&fields[0])?;
    let height = as_i32(&fields[1])?;
    let rowstride = as_i32(&fields[2])?;
    let has_alpha = as_bool(&fields[3])?;
    let bits_per_sample = as_i32(&fields[4])?;
    let channels = as_i32(&fields[5])?;
    let data = as_byte_array(&fields[6])?;

    // Validate.
    if width <= 0 || height <= 0 {
        return None;
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        tracing::warn!(
            width,
            height,
            "image-data hint rejected: exceeds {MAX_IMAGE_DIMENSION}px limit"
        );
        return None;
    }
    if bits_per_sample != 8 {
        // Spec allows non-8-bit but no real sender uses it. Keeping
        // the happy path tight avoids pulling in 16-bit codecs.
        return None;
    }
    if channels != 3 && channels != 4 {
        return None;
    }
    if (channels == 4) != has_alpha {
        // has_alpha and channels must agree (channels=3 → RGB, channels=4 → RGBA).
        return None;
    }

    let expected_min_stride = width.checked_mul(channels)?;
    if rowstride < expected_min_stride {
        return None;
    }

    let w = width as usize;
    let h = height as usize;
    let stride = rowstride as usize;
    let ch = channels as usize;

    if data.len() < stride.checked_mul(h.saturating_sub(1))? + w * ch {
        // Last-row short-count is allowed by the spec; anything less is malformed.
        return None;
    }

    // Pack into a dense RGBA buffer. We always materialise RGBA
    // because the image crate's PNG encoder emits smaller files for
    // the common case and we uniformly produce one output format.
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let row_start = row * stride;
        for col in 0..w {
            let pixel_start = row_start + col * ch;
            let r = data[pixel_start];
            let g = data[pixel_start + 1];
            let b = data[pixel_start + 2];
            let a = if has_alpha { data[pixel_start + 3] } else { 255 };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }

    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(w as u32, h as u32, rgba)?;

    let mut png_bytes: Vec<u8> = Vec::new();
    if buffer
        .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .is_err()
    {
        return None;
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Some(format!("data:image/png;base64,{b64}"))
}

fn as_i32(value: &Value<'_>) -> Option<i32> {
    match value {
        Value::I32(n) => Some(*n),
        _ => None,
    }
}

fn as_bool(value: &Value<'_>) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_byte_array(value: &Value<'_>) -> Option<Vec<u8>> {
    // zbus represents `ay` as `Array<u8>`; the concrete variant depends
    // on the incoming message. Accept both the dedicated bytearray and
    // the generic `Array<Value::U8>` form.
    match value {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr.iter() {
                match v {
                    Value::U8(b) => out.push(*b),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value as ZValue;

    fn build_rgba_hint(
        width: i32,
        height: i32,
        rowstride: i32,
        has_alpha: bool,
        channels: i32,
        data: Vec<u8>,
    ) -> OwnedValue {
        // FDO image-data struct: (iiibiiay)
        let structure: Structure<'static> = (
            width,
            height,
            rowstride,
            has_alpha,
            8i32,
            channels,
            data,
        )
            .into();
        ZValue::Structure(structure).try_into().unwrap()
    }

    #[test]
    fn fallback_to_app_icon_when_no_hints() {
        let hints: HashMap<String, OwnedValue> = HashMap::new();
        assert_eq!(resolve_icon(&hints, "firefox"), "firefox");
    }

    #[test]
    fn image_path_beats_app_icon() {
        let mut hints = HashMap::new();
        hints.insert(
            "image-path".into(),
            ZValue::Str("/usr/share/icons/firefox.png".into())
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            resolve_icon(&hints, "firefox"),
            "/usr/share/icons/firefox.png"
        );
    }

    #[test]
    fn legacy_image_path_underscore_still_works() {
        let mut hints = HashMap::new();
        hints.insert(
            "image_path".into(),
            ZValue::Str("/tmp/icon.png".into()).try_into().unwrap(),
        );
        assert_eq!(resolve_icon(&hints, "fallback"), "/tmp/icon.png");
    }

    #[test]
    fn image_data_rgba_produces_png_data_url() {
        // 2x2 RGBA image, packed rowstride (width * 4 = 8 bytes/row).
        let data = vec![
            // Row 0
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            // Row 1
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        let mut hints = HashMap::new();
        hints.insert("image-data".into(), build_rgba_hint(2, 2, 8, true, 4, data));

        let result = resolve_icon(&hints, "fallback");
        assert!(result.starts_with("data:image/png;base64,"));
        // PNG magic bytes in base64: "iVBORw0KGgo" (decode of the 8-byte
        // PNG header \x89PNG\r\n\x1a\n) — sanity-check the encoder ran.
        assert!(result.contains("iVBORw0KGgo"));
    }

    #[test]
    fn image_data_beats_image_path() {
        let data = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let mut hints = HashMap::new();
        hints.insert("image-data".into(), build_rgba_hint(2, 1, 8, true, 4, data));
        hints.insert(
            "image-path".into(),
            ZValue::Str("/path/ignored.png".into())
                .try_into()
                .unwrap(),
        );
        let result = resolve_icon(&hints, "fallback");
        assert!(result.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn image_data_rgb_without_alpha() {
        // 1x1 RGB image.
        let data = vec![200, 100, 50];
        let mut hints = HashMap::new();
        hints.insert("image-data".into(), build_rgba_hint(1, 1, 3, false, 3, data));
        let result = resolve_icon(&hints, "fallback");
        assert!(result.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn image_data_rowstride_padding_is_respected() {
        // 2x2 RGBA with rowstride 12 (4 extra padding bytes per row).
        let data = vec![
            // Row 0: 2 pixels (8 bytes) + 4 padding
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 0, 0,
            // Row 1: 2 pixels + 4 padding
            0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0,
        ];
        let mut hints = HashMap::new();
        hints.insert("image-data".into(), build_rgba_hint(2, 2, 12, true, 4, data));
        let result = resolve_icon(&hints, "fallback");
        assert!(result.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn image_data_falls_back_on_bad_dimensions() {
        // width=0 is invalid -> fall back to app_icon.
        let mut hints = HashMap::new();
        hints.insert("image-data".into(), build_rgba_hint(0, 2, 0, true, 4, vec![]));
        assert_eq!(resolve_icon(&hints, "firefox"), "firefox");
    }

    #[test]
    fn image_data_rejects_oversized_dimensions() {
        // 1024×1024 exceeds MAX_IMAGE_DIMENSION = 512. Expect fallback.
        let mut hints = HashMap::new();
        // rowstride must match; size validation runs before data.
        hints.insert(
            "image-data".into(),
            build_rgba_hint(1024, 1024, 4096, true, 4, vec![0u8; 1]),
        );
        assert_eq!(resolve_icon(&hints, "firefox"), "firefox");
    }

    #[test]
    fn image_data_channels_alpha_mismatch_rejected() {
        // channels=3 but has_alpha=true → malformed, fall back.
        let mut hints = HashMap::new();
        hints.insert(
            "image-data".into(),
            build_rgba_hint(1, 1, 3, true, 3, vec![1, 2, 3]),
        );
        assert_eq!(resolve_icon(&hints, "firefox"), "firefox");
    }

    #[test]
    fn empty_image_path_falls_through_to_app_icon() {
        let mut hints = HashMap::new();
        hints.insert(
            "image-path".into(),
            ZValue::Str("".into()).try_into().unwrap(),
        );
        assert_eq!(resolve_icon(&hints, "firefox"), "firefox");
    }
}
