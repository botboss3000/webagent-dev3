//! Rules that route a spoken utterance: confirmations, run controls, status questions, work
//! requests and quick questions. Every classifier takes text already passed through [`words`].

/// Lower-case `utterance`, keep letters, digits and apostrophes, and join the words with single
/// spaces, so "OK, stop." reads as "ok stop".
pub fn words(utterance: &str) -> String {
    utterance
        .to_lowercase()
        .replace('\u{2019}', "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A spoken confirmation.
pub fn yes(text: &str) -> bool {
    matches!(
        text,
        "yes"
            | "yeah"
            | "yep"
            | "sure"
            | "okay"
            | "ok"
            | "do it"
            | "go ahead"
            | "confirm"
            | "approve"
            | "approved"
    )
}

/// A spoken rejection.
pub fn no(text: &str) -> bool {
    matches!(
        text,
        "no" | "nope"
            | "nah"
            | "never mind"
            | "nevermind"
            | "reject"
            | "don't"
            | "do not"
            | "cancel"
    )
}

/// An explicit work request: an action verb, optionally after a polite prefix.
pub fn work(text: &str) -> bool {
    let text = text
        .strip_prefix("please ")
        .or_else(|| text.strip_prefix("can you "))
        .or_else(|| text.strip_prefix("could you "))
        .or_else(|| text.strip_prefix("would you "))
        .unwrap_or(text)
        .trim_start_matches("please ");
    [
        "build",
        "create",
        "implement",
        "fix",
        "write",
        "make",
        "add",
        "remove",
        "update",
        "run",
        "test",
        "deploy",
        "research",
        "find",
        "open",
        "send",
        "generate",
    ]
    .contains(&text.split_whitespace().next().unwrap_or(""))
}

/// A question by its leading word.
pub fn question(text: &str) -> bool {
    [
        "what", "why", "when", "where", "who", "how", "is", "are", "can", "could", "do", "does",
        "did", "explain",
    ]
    .contains(&text.split_whitespace().next().unwrap_or(""))
        || text.starts_with("tell me")
}

/// A spoken run control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Stop the active run.
    Cancel,
    /// Resume a paused run.
    Resume,
}

impl Control {
    /// The run command name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Resume => "resume",
        }
    }
}

/// A run control addressed at the current run, if `text` is one. Courtesy words around the
/// command ("ok, stop please") are ignored; anything else ("don't stop", "stop the music") is not
/// a control.
pub fn control(text: &str) -> Option<Control> {
    let mut words = text.split_whitespace().collect::<Vec<_>>();
    while let Some(first) = words.first() {
        if !matches!(*first, "please" | "ok" | "okay" | "hey" | "just" | "now") {
            break;
        }
        words.remove(0);
    }
    while let Some(last) = words.last() {
        if !matches!(*last, "please" | "now" | "thanks") {
            break;
        }
        words.pop();
    }
    let (verb, tail) = words.split_first()?;
    let tail = tail.join(" ");
    if [
        "",
        "it",
        "that",
        "everything",
        "run",
        "task",
        "work",
        "job",
        "agent",
        "the run",
        "the task",
        "the work",
        "the job",
        "the agent",
        "this",
        "this run",
        "this task",
        "what you're doing",
        "what you are doing",
    ]
    .contains(&tail.as_str())
    {
        match *verb {
            // A spoken pause is a stop; the next message carries the work forward.
            "stop" | "cancel" | "abort" | "halt" | "pause" => return Some(Control::Cancel),
            "resume" | "continue" => return Some(Control::Resume),
            _ => {}
        }
    }
    // "hold on" may stand alone; "wait" needs a duration so a hesitant "wait" is not a stop.
    let pause = match *verb {
        "hold" if tail == "on" => return Some(Control::Cancel),
        "hold" => tail.strip_prefix("on "),
        "wait" => Some(tail.as_str()),
        _ => None,
    };
    pause
        .filter(|rest| {
            matches!(
                *rest,
                "a second" | "a sec" | "a moment" | "a minute" | "one second" | "one moment"
            )
        })
        .map(|_| Control::Cancel)
}

/// A question about the current run's progress.
pub fn status_query(text: &str) -> bool {
    text.split_whitespace()
        .any(|w| matches!(w, "status" | "progress"))
        || matches!(
            text,
            "what's happening"
                | "what is happening"
                | "what are you doing"
                | "how is it going"
                | "are you done"
                | "is it done"
        )
}

/// Wording that changes existing work; with an active run it makes the utterance a work request.
pub fn modifies_work(text: &str) -> bool {
    text.split_whitespace().any(|w| {
        matches!(
            w,
            "instead"
                | "change"
                | "modify"
                | "revise"
                | "switch"
                | "replace"
                | "remove"
                | "rename"
                | "update"
                | "also"
                | "add"
                | "include"
                | "use"
        )
    })
}
