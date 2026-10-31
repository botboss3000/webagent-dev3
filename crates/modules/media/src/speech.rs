//! Text prepared for server speech: code blocks, links, Markdown markup and credential-like words
//! are not read aloud, and the result is bounded.

/// Most characters sent to speech synthesis.
pub const MAX_SPEECH_CHARS: usize = 1800;

/// Replace fenced code with a short notice, read Markdown as plain text (link text without its URL),
/// omit links, redact bearer tokens and key-like words, collapse whitespace and truncate to
/// [`MAX_SPEECH_CHARS`].
pub fn speech_text(text: &str) -> String {
    let mut result = String::new();
    for (index, part) in text.split("\u{60}\u{60}\u{60}").enumerate() {
        if index % 2 == 0 {
            result.push_str(part);
        } else {
            result.push_str(" Code omitted. ");
        }
    }
    let result = result
        .lines()
        .map(plain_line)
        .collect::<Vec<_>>()
        .join("\n");
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

/// One Markdown line as spoken text: no heading, quote or bullet marker, no bold or inline-code
/// marks, `[text](url)` as its text and `<url>` as "link omitted".
fn plain_line(line: &str) -> String {
    let line = line
        .trim_start()
        .trim_start_matches(['#', '>'])
        .trim_start();
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .replace("**", "")
        .replace("__", "")
        .replace('\u{60}', "");
    let mut out = String::new();
    let mut rest = line.as_str();
    while let Some(start) = rest.find(['[', '<']) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        if let Some(inner) = tail.strip_prefix('<') {
            let lower = inner.to_ascii_lowercase();
            let url = lower.starts_with("http://") || lower.starts_with("https://");
            if let Some(end) = inner.find('>').filter(|_| url) {
                out.push_str("link omitted");
                rest = &inner[end + 1..];
                continue;
            }
        } else if let Some((text, after)) = tail[1..].split_once("](") {
            if let Some(end) = after.find(')').filter(|_| !text.contains(['[', ']'])) {
                out.push_str(text);
                rest = &after[end + 1..];
                continue;
            }
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}
