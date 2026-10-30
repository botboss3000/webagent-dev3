//! Text prepared for server speech: code blocks, links and credential-like words are not read
//! aloud, and the result is bounded.

/// Most characters sent to speech synthesis.
pub const MAX_SPEECH_CHARS: usize = 1800;

/// Replace fenced code with a short notice, omit links, redact bearer tokens and key-like words,
/// collapse whitespace and truncate to [`MAX_SPEECH_CHARS`].
pub fn speech_text(text: &str) -> String {
    let mut result = String::new();
    for (index, part) in text.split("\u{60}\u{60}\u{60}").enumerate() {
        if index % 2 == 0 {
            result.push_str(part);
        } else {
            result.push_str(" Code omitted. ");
        }
    }
    let mut redact_next = false;
    let result = result
        .split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if redact_next {
                redact_next = false;
                return "[redacted]";
            }
            if lower == "bearer" {
                redact_next = true;
                return "[redacted]";
            }
            if lower.starts_with("http://") || lower.starts_with("https://") {
                return "link omitted";
            }
            if lower.starts_with("sk-") && word.len() > 12
                || lower.starts_with("api_key")
                || lower.starts_with("token=")
            {
                return "[redacted]";
            }
            word
        })
        .collect::<Vec<_>>()
        .join(" ");
    result.chars().take(MAX_SPEECH_CHARS).collect()
}
