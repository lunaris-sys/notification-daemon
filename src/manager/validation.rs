/// Input validation and sanitization for incoming notifications.
///
/// Truncates oversized fields and enforces limits to prevent abuse.

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
    let summary = if summary.is_empty() {
        app_name.clone()
    } else {
        truncate(summary, MAX_SUMMARY)
    };
    let body = truncate(body, MAX_BODY);
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
}
