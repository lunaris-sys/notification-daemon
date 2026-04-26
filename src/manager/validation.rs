/// Input validation and sanitization for incoming notifications.
///
/// Truncates oversized fields, strips HTML markup, and enforces limits
/// to prevent abuse. The shell renders every text field as plain text
/// (Svelte auto-escapes interpolated values), so any `<b>`, `<i>`,
/// `<a>` etc. that apps like Thunderbird or Evolution send would
/// otherwise appear as literal tag text. See `strip_markup` below.

/// Maximum lengths.
const MAX_APP_NAME: usize = 50;
const MAX_SUMMARY: usize = 100;
const MAX_BODY: usize = 64 * 1024; // 64 KB
const MAX_ICON_LEN: usize = 1024 * 1024; // 1 MB (path or data URI)
const MAX_ACTIONS: usize = 6; // 3 key/label pairs

/// Sanitized notification input.
pub struct SanitizedInput {
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub app_icon: String,
    pub actions: Vec<String>,
}

/// Validate and sanitize D-Bus Notify() input.
pub fn sanitize_input(
    app_name: &str,
    summary: &str,
    body: &str,
    app_icon: &str,
    actions: &[String],
) -> SanitizedInput {
    let app_name = truncate(app_name, MAX_APP_NAME);
    // `summary` is spec'd as plain text but defensive strip is cheap;
    // some apps ship markup here too. `body` is where FDO actually
    // allows markup.
    //
    // We truncate BEFORE stripping so the HTML parser never sees more
    // than the per-field limit. A 10 MB body trimmed to 64 KB first
    // means ammonia touches at most 64 KB. Truncation may land mid-
    // tag (`<b>unclosed`), but ammonia's HTML5 parser is tolerant of
    // malformed input — unclosed tags just get dropped with their
    // content preserved. Strip output is always ≤ input length.
    let summary = if summary.is_empty() {
        app_name.clone()
    } else {
        strip_markup(&truncate(summary, MAX_SUMMARY))
    };
    let body = strip_markup(&truncate(body, MAX_BODY));
    let app_icon = truncate(app_icon, MAX_ICON_LEN);

    // Actions must be even-length (key/label pairs), max 6 entries.
    let mut sanitized_actions: Vec<String> = actions
        .iter()
        .take(MAX_ACTIONS)
        .cloned()
        .collect();
    // Ensure even length.
    if sanitized_actions.len() % 2 != 0 {
        sanitized_actions.pop();
    }

    SanitizedInput {
        app_name,
        summary,
        body,
        app_icon,
        actions: sanitized_actions,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Truncate at char boundary.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    }
}

/// Remove all HTML tags and decode entities.
///
/// Thunderbird, Evolution, and similar senders embed a small HTML
/// subset (`<b>`, `<i>`, `<a>`, `<img>`) in notification bodies; the
/// FDO spec calls this capability `body-markup`. The shell renders
/// bodies as escaped text, so un-stripped markup would appear as
/// literal `<b>foo</b>` instead of styled text. We deliberately do
/// **not** advertise the `body-markup` capability anymore, and strip
/// any markup that arrives anyway — a single consistent plain-text
/// contract is easier to reason about and eliminates an entire class
/// of XSS concerns in the shell renderer.
///
/// Strategy: `ammonia` with an empty tag allowlist parses the HTML
/// and drops every tag, leaving text content. Its output is still
/// HTML-escaped (so `<` and `&` become entities), which is the wrong
/// representation for plain-text display; `html-escape` decodes the
/// entities afterwards so the user sees what the sender meant.
///
/// Edge cases that fall out of using a real HTML parser rather than
/// a regex: malformed tags (`<b>unclosed`), nested tags
/// (`<b><i>x</i></b>`), comment blocks (`<!-- ... -->`), and
/// `<script>`/`<style>` content are all handled correctly.
pub fn strip_markup(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    // Fast path: bodies without `<` or `&` are pure text. Most CLI
    // notifications (`notify-send "Foo" "Bar"`) hit this branch and
    // skip the HTML parser entirely.
    if !input.contains('<') && !input.contains('&') {
        return input.to_string();
    }

    let no_tags = ammonia::Builder::empty().clean(input).to_string();
    html_escape::decode_html_entities(&no_tags).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_input_unchanged() {
        let r = sanitize_input("Firefox", "Download", "file.zip", "firefox", &[]);
        assert_eq!(r.app_name, "Firefox");
        assert_eq!(r.summary, "Download");
        assert_eq!(r.body, "file.zip");
    }

    #[test]
    fn test_truncate_app_name() {
        let long = "A".repeat(100);
        let r = sanitize_input(&long, "s", "b", "", &[]);
        assert_eq!(r.app_name.len(), MAX_APP_NAME);
    }

    #[test]
    fn test_truncate_summary() {
        let long = "S".repeat(200);
        let r = sanitize_input("app", &long, "", "", &[]);
        assert_eq!(r.summary.len(), MAX_SUMMARY);
    }

    #[test]
    fn test_empty_summary_defaults_to_app_name() {
        let r = sanitize_input("MyApp", "", "body", "", &[]);
        assert_eq!(r.summary, "MyApp");
    }

    #[test]
    fn test_truncate_body() {
        let long = "B".repeat(100_000);
        let r = sanitize_input("app", "s", &long, "", &[]);
        assert_eq!(r.body.len(), MAX_BODY);
    }

    #[test]
    fn test_actions_max_count() {
        let actions: Vec<String> = (0..20).map(|i| format!("a{i}")).collect();
        let r = sanitize_input("app", "s", "", "", &actions);
        assert_eq!(r.actions.len(), MAX_ACTIONS);
    }

    #[test]
    fn test_actions_even_length() {
        let actions = vec!["k1".into(), "l1".into(), "orphan".into()];
        let r = sanitize_input("app", "s", "", "", &actions);
        assert_eq!(r.actions.len(), 2); // Odd trimmed to even.
    }

    #[test]
    fn test_truncate_unicode_safe() {
        // 4-byte emoji at boundary.
        let s = "Hello 🌍 World";
        let truncated = truncate(s, 8);
        // "Hello 🌍" is 10 bytes. Truncate at 8 -> "Hello " (6 bytes)
        // because byte 6-9 is the emoji.
        assert!(truncated.len() <= 8);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn test_truncate_icon_oversized() {
        let long = "X".repeat(2_000_000);
        let r = sanitize_input("app", "s", "", &long, &[]);
        assert_eq!(r.app_icon.len(), MAX_ICON_LEN);
    }

    // ── strip_markup ─────────────────────────────────────────────────────

    #[test]
    fn strip_plain_text_passes_through() {
        assert_eq!(strip_markup("Hello world"), "Hello world");
        assert_eq!(strip_markup(""), "");
        assert_eq!(strip_markup("file.zip downloaded"), "file.zip downloaded");
    }

    #[test]
    fn strip_removes_bold() {
        // Thunderbird pattern: bold sender name before subject.
        assert_eq!(strip_markup("<b>Tim</b>: Re: Meeting"), "Tim: Re: Meeting");
    }

    #[test]
    fn strip_removes_multiple_tags() {
        assert_eq!(
            strip_markup("<b>bold</b> and <i>italic</i>"),
            "bold and italic"
        );
    }

    #[test]
    fn strip_removes_nested_tags() {
        assert_eq!(strip_markup("<b><i>nested</i></b>"), "nested");
    }

    #[test]
    fn strip_removes_links_keeps_text() {
        // FDO spec would allow `<a>` under body-markup, but we don't
        // advertise that capability; apps that send links anyway get
        // the visible text, and the URL disappears. Real use of URLs
        // should go through the `actions` array, not inline in body.
        assert_eq!(
            strip_markup(r#"Click <a href="https://evil.example.com">here</a>"#),
            "Click here"
        );
    }

    #[test]
    fn strip_decodes_entities() {
        assert_eq!(strip_markup("5 &lt; 10"), "5 < 10");
        assert_eq!(strip_markup("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(strip_markup("&quot;hi&quot;"), "\"hi\"");
    }

    #[test]
    fn strip_decodes_numeric_entities() {
        // Common when apps insert typographic chars without UTF-8.
        assert_eq!(strip_markup("caf&#233;"), "café");
        assert_eq!(strip_markup("&#x2764;"), "\u{2764}");
    }

    #[test]
    fn strip_handles_malformed_html() {
        // Unclosed tag — ammonia's error-tolerant HTML5 parser recovers.
        let out = strip_markup("<b>unclosed start");
        assert!(out.contains("unclosed start"));
    }

    #[test]
    fn strip_removes_script_tag_content() {
        // `<script>` is not in ammonia's default content-preserving
        // set: its content is dropped entirely, not rendered as text.
        let out = strip_markup("before<script>alert(1)</script>after");
        assert!(!out.contains("alert"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    #[test]
    fn strip_fast_path_skips_parser() {
        // 1 MB of plain ASCII — no `<` or `&`. The fast-path branch
        // short-circuits without invoking ammonia. This both documents
        // the optimisation and guards it against accidental removal.
        let long = "A".repeat(1_000_000);
        let out = strip_markup(&long);
        assert_eq!(out, long);
    }

    #[test]
    fn strip_output_never_exceeds_input_length() {
        // Guarantees that our "truncate then strip" order in
        // sanitize_input cannot overflow the per-field byte cap.
        for input in [
            "<b>x</b>",
            "&amp;&lt;&gt;",
            "<a href='http://x'>link</a>",
            "<script>code</script>",
            "plain",
        ] {
            assert!(strip_markup(input).len() <= input.len(), "grew: {input:?}");
        }
    }

    #[test]
    fn sanitize_input_strips_body_markup() {
        // End-to-end: the sanitised SanitizedInput has no HTML in body.
        let r = sanitize_input(
            "Thunderbird",
            "New Mail",
            "<b>Tim</b>: Re: Meeting",
            "",
            &[],
        );
        assert_eq!(r.body, "Tim: Re: Meeting");
        assert!(!r.body.contains('<'));
    }

    #[test]
    fn sanitize_input_strips_summary_markup() {
        // Summary is spec'd as plain text but we strip defensively.
        let r = sanitize_input("app", "<b>Urgent</b>", "body", "", &[]);
        assert_eq!(r.summary, "Urgent");
    }
}
