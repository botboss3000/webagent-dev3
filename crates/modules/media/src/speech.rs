//! Text prepared for server speech: code blocks, links, Markdown markup and credential-like words
//! are not read aloud, and the result is bounded.
//! Also the Kokoro voice catalog and the per-agent voice output setting it validates.

/// The first group of sentences holds at most this many characters (unless one sentence is
/// longer), so playback starts after about one sentence. Each later group may be twice as long
/// as the one before, up to [`CHUNK_CHARS`], so it is synthesized before the one before it has
/// finished playing.
const FIRST_CHUNK_CHARS: usize = 120;
/// Longest group of sentences.
const CHUNK_CHARS: usize = 400;

/// Replace fenced code with a short notice, read Markdown as plain text (link text without its URL),
/// omit links, redact bearer tokens and key-like words, collapse whitespace and truncate to
/// `max_chars`. Each line ends a sentence, so headings and list items get a spoken pause instead
/// of running into the next line.
pub fn speech_text(text: &str, max_chars: usize) -> String {
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
        .map(|line| end_sentence(plain_line(line)))
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
    result.chars().take(max_chars).collect()
}

/// Prepared speech split into the groups of sentences that are synthesized and sent one at a
/// time: a sentence ends at `.`, `!`, `?`, `;` or `:` followed by whitespace.
pub fn speech_chunks(text: &str) -> Vec<String> {
    let mut sentences = vec![String::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let ended = sentences
            .last()
            .is_some_and(|s| s.ends_with(['.', '!', '?', ';', ':']));
        if c.is_whitespace() && ended {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            sentences.push(String::new());
        } else if let Some(current) = sentences.last_mut() {
            current.push(c);
        }
    }
    let (mut chunks, mut group, mut limit) = (Vec::new(), String::new(), FIRST_CHUNK_CHARS);
    for sentence in sentences {
        if !group.is_empty() && group.chars().count() + sentence.chars().count() > limit {
            chunks.push(std::mem::take(&mut group));
            limit = (limit * 2).min(CHUNK_CHARS);
        }
        group = format!("{group} {sentence}").trim().to_owned();
    }
    if !group.is_empty() {
        chunks.push(group);
    }
    chunks
}

/// A non-empty line without closing punctuation gets a period; speech pauses on punctuation.
fn end_sentence(line: String) -> String {
    let line = line.trim_end();
    match line.chars().last() {
        Some(last) if !".!?:;,\u{2026}".contains(last) => format!("{line}."),
        _ => line.to_owned(),
    }
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

/// English Kokoro voices offered for agent voice output, as (id, name). The id prefix is the
/// accent and gender: `af` American female, `am` American male, `bf`/`bm` British.
pub const VOICES: &[(&str, &str)] = &[
    ("af_alloy", "Alloy"),
    ("af_aoede", "Aoede"),
    ("af_bella", "Bella"),
    ("af_heart", "Heart"),
    ("af_jessica", "Jessica"),
    ("af_kore", "Kore"),
    ("af_nicole", "Nicole"),
    ("af_nova", "Nova"),
    ("af_river", "River"),
    ("af_sarah", "Sarah"),
    ("af_sky", "Sky"),
    ("am_adam", "Adam"),
    ("am_echo", "Echo"),
    ("am_eric", "Eric"),
    ("am_fenrir", "Fenrir"),
    ("am_liam", "Liam"),
    ("am_michael", "Michael"),
    ("am_onyx", "Onyx"),
    ("am_puck", "Puck"),
    ("am_santa", "Santa"),
    ("bf_alice", "Alice"),
    ("bf_emma", "Emma"),
    ("bf_isabella", "Isabella"),
    ("bf_lily", "Lily"),
    ("bm_daniel", "Daniel"),
    ("bm_fable", "Fable"),
    ("bm_george", "George"),
    ("bm_lewis", "Lewis"),
];

/// Voice used until an agent chooses one.
pub const DEFAULT_VOICE: &str = "af_heart";

/// The voice catalog for clients: id, name and accent/gender group.
pub fn voice_catalog() -> serde_json::Value {
    let group = |id: &str| match &id[..2] {
        "af" => "American female",
        "am" => "American male",
        "bf" => "British female",
        _ => "British male",
    };
    VOICES
        .iter()
        .map(|(id, name)| serde_json::json!({"id": id, "name": name, "group": group(id)}))
        .collect()
}

/// Spoken introduction used for a voice's sample.
pub fn sample_text(voice: &str) -> Option<String> {
    VOICES
        .iter()
        .find(|(id, _)| *id == voice)
        .map(|(_, name)| format!("Hi, I'm {name}. This is how I sound when I read a reply aloud."))
}

/// An agent's voice output setting, stored as `voice_output` in its encrypted metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceOutput {
    /// Whether chats with this agent offer the voice reply control.
    pub enabled: bool,
    /// One of [`VOICES`].
    pub voice: String,
}

/// A submitted or stored voice output setting that is not `{enabled: bool, voice: <known id>}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidVoiceOutput;

impl std::fmt::Display for InvalidVoiceOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Voice output needs enabled (true or false) and a listed voice")
    }
}

impl std::error::Error for InvalidVoiceOutput {}

impl Default for VoiceOutput {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: DEFAULT_VOICE.into(),
        }
    }
}

impl VoiceOutput {
    /// Parse a setting; null (never set) is off with the default voice. Unknown fields and
    /// voices are rejected.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, InvalidVoiceOutput> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let fields = value.as_object().ok_or(InvalidVoiceOutput)?;
        let enabled = fields.get("enabled").and_then(|v| v.as_bool());
        let voice = fields.get("voice").and_then(|v| v.as_str());
        match (enabled, voice) {
            (Some(enabled), Some(voice))
                if fields.len() == 2 && VOICES.iter().any(|(id, _)| *id == voice) =>
            {
                Ok(Self {
                    enabled,
                    voice: voice.into(),
                })
            }
            _ => Err(InvalidVoiceOutput),
        }
    }

    /// The stored and public form.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({"enabled": self.enabled, "voice": self.voice})
    }
}
