//! Reading a value from a request for one setting: numbers with units, enum values, on/off,
//! quoted text, relative changes ("bigger", "by 3", "double"), each setting's own value words
//! (`value-words.json`), and how strong that evidence is.
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::matcher::Setting;

/// Settings' own wording that their type alone does not give: enum synonyms ("close" →
/// restricted), phrases that need another detail first, and direction words ("by accident" →
/// smaller). Phrases are tried in file order; the first one found in the request wins.
pub const VALUE_WORDS: &str = include_str!("../value-words.json");

/// One request's reading for a setting.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// A new value.
    Value(Value),
    /// A change to the current value: `{"op": "add"|"multiply", "by": N}` or `{"op":
    /// "increase"|"decrease"}`.
    Change(Value),
    /// The request needs this detail before a value can be proposed (for example a voice).
    Needs(String),
    /// Nothing in the request reads as a value for this setting.
    Nothing,
}

const NUMBER_WORDS: [(&str, u32); 12] = [
    ("zero", 0),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("twenty", 20),
];
const UP: &str = "bigger|more|wider|larger|longer|later|higher|increase|raise|grow";
const DOWN: &str = "smaller|less|fewer|narrower|shorter|earlier|lower|decrease|reduce|shrink";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

static EVERY: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b(every|each|per) (second|minute|hour)\b"));
static NUMBER_WORD: LazyLock<Regex> = LazyLock::new(|| {
    let words: Vec<&str> = NUMBER_WORDS.iter().map(|(word, _)| *word).collect();
    regex(&format!(r"\b({})\b", words.join("|")))
});
static QUANTITY: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(\d+(?:\.\d+)?)\s*(k\b)?\s*(%|percent|ms|milliseconds?|seconds?|secs?|s\b|minutes?|mins?|hours?|h\b|mb|gb|px|pixels?|tokens?)?",
    )
});
static OFF: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"\b(off|disable|don'?t|do not|stop|no longer|never|get rid|hide|remove|turn off)\b")
});
static QUOTED: LazyLock<Regex> = LazyLock::new(|| regex(r#"['"‘’“”](.+?)['"‘’“”]"#));
static QUOTE: LazyLock<Regex> = LazyLock::new(|| regex(r#"['"‘’“”]"#));
static NAMES: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        regex(r"\bto ([A-Z][\w-]*)\s*$"),
        regex(r"\b(?i:call) (?:me|\w+(?:[- ]\w+)? people) ([A-Z]\w*)"),
        regex(r"\b(?:me|is) ([A-Z]\w*)\s*$"),
    ]
});
static DOUBLE: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(double|twice)\b"));
static HALVE: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(halve|by half|half as)\b"));
static ONE_MORE: LazyLock<Regex> = LazyLock::new(|| regex(r"\bone more\b"));
static AFTER_UP: LazyLock<Regex> = LazyLock::new(|| regex(&format!(r"^\s*(?:\w+\s)?({UP})\b")));
static AFTER_DOWN: LazyLock<Regex> = LazyLock::new(|| regex(&format!(r"^\s*(?:\w+\s)?({DOWN})\b")));
static HALF_SCREEN: LazyLock<Regex> = LazyLock::new(|| regex(r"\bhalf (my|the) screen\b"));
static INCREASE: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"\b({UP}|too (small|narrow|slow|short))\b")));
static DECREASE: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"\b({DOWN}|too (big|large|wide|long))\b")));
static TIERS: LazyLock<[(&str, Regex); 3]> = LazyLock::new(|| {
    [
        ("basic", regex(r"\b(basic|base|regular|member)\b")),
        (
            "anonymous",
            regex(r"\b(anonymous|signed[- ]out|guest|without logging)\b"),
        ),
        ("operator", regex(r"\b(operator|admin)\b")),
    ]
});

/// A number found in a request, in its own unit (`""` for none).
#[derive(Debug, Clone, PartialEq)]
pub struct Quantity {
    pub number: f64,
    pub unit: String,
    end: usize,
}

/// The request lowercased, with "every minute" read as "every 1 minute" and number words before
/// another word as digits, and the numbers in it.
pub fn quantities(text: &str) -> (String, Vec<Quantity>) {
    let lower = EVERY
        .replace_all(&text.to_lowercase(), "$1 1 $2")
        .into_owned();
    let mut digits = String::with_capacity(lower.len());
    let mut last = 0;
    for found in NUMBER_WORD.find_iter(&lower) {
        // Only a number word followed by another word ("five pixels", "one more").
        let mut rest = lower[found.end()..].chars();
        if rest.next() == Some(' ') && rest.next().is_some_and(word_char) {
            let value = NUMBER_WORDS
                .iter()
                .find(|(word, _)| *word == found.as_str())
                .map_or(0, |(_, value)| *value);
            digits.push_str(&lower[last..found.start()]);
            digits.push_str(&value.to_string());
            last = found.end();
        }
    }
    digits.push_str(&lower[last..]);
    let numbers = QUANTITY
        .captures_iter(&digits)
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            let number =
                caps[1].parse::<f64>().ok()? * if caps.get(2).is_some() { 1000.0 } else { 1.0 };
            let unit = caps.get(3).map_or("", |unit| unit.as_str());
            let unit = if matches!(unit, "s" | "ms" | "%") {
                unit
            } else {
                unit.trim_end_matches('s')
            };
            Some(Quantity {
                number,
                unit: unit.to_owned(),
                end: whole.end(),
            })
        })
        .collect();
    (digits, numbers)
}

/// A unit's family and its size in that family's base unit.
fn scale(unit: &str) -> Option<(&'static str, f64)> {
    Some(match unit {
        "ms" | "millisecond" => ("time", 0.001),
        "s" | "sec" | "second" => ("time", 1.0),
        "min" | "minute" => ("time", 60.0),
        "h" | "hour" => ("time", 3600.0),
        "mb" => ("size", 1.0),
        "gb" => ("size", 1024.0),
        "px" | "pixel" => ("px", 1.0),
        "%" | "percent" => ("pct", 1.0),
        "token" => ("tok", 1.0),
        _ => return None,
    })
}

/// A setting's unit's family and size ("MB", "tokens", "min").
fn setting_scale(unit: &str) -> Option<(&'static str, f64)> {
    let unit = unit.to_lowercase();
    scale(&unit).or_else(|| scale(unit.trim_end_matches('s')))
}

/// `number` in `unit` converted to the setting's unit when both are of one family.
fn convert(number: f64, unit: &str, setting: &Setting) -> f64 {
    let Some((family, size)) = scale(unit) else {
        return number;
    };
    let (target, target_size) = setting_scale(&setting.unit).unwrap_or((family, 1.0));
    if family == target {
        number * size / target_size
    } else {
        number
    }
}

/// A whole number as an integer, anything else as a float.
pub fn clean(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        json!(number as i64)
    } else {
        json!(number)
    }
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `word` appears in `text` with word boundaries on both sides.
pub fn has_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let edge = |next: Option<char>, inner: Option<char>| {
            next.is_none_or(|c| !word_char(c)) || !inner.is_some_and(word_char)
        };
        edge(before, word.chars().next()) && edge(after, word.chars().next_back())
    })
}

/// The value words of every setting: `{id: {phrases: [[phrase, value]], increase: [...],
/// decrease: [...], inverse: [...]}}`.
pub fn value_words() -> Map<String, Value> {
    serde_json::from_str::<Value>(VALUE_WORDS)
        .ok()
        .and_then(|words| words.get("settings")?.as_object().cloned())
        .unwrap_or_default()
}

fn word_list<'a>(words: &'a Value, key: &str) -> impl Iterator<Item = &'a str> {
    words[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn phrases(words: &Value) -> impl Iterator<Item = (&str, &Value)> {
    words["phrases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| Some((pair.get(0)?.as_str()?, pair.get(1)?)))
}

/// Read a value for `setting` from `text`; `agents` are the agent ids a request may name.
pub fn read(text: &str, setting: &Setting, words: &Value, agents: &[String]) -> Reading {
    let (t, numbers) = quantities(text);
    for (phrase, value) in phrases(words) {
        if t.contains(phrase) {
            return match value.get("needs").and_then(Value::as_str) {
                Some(detail) => Reading::Needs(detail.to_owned()),
                None => Reading::Value(value.clone()),
            };
        }
    }
    match setting.kind.as_str() {
        "enum" => {
            return setting
                .choices
                .iter()
                .find(|choice| has_word(&t, choice))
                .map_or(Reading::Nothing, |choice| Reading::Value(json!(choice)))
        }
        "boolean" | "object" => {
            let on = !OFF.is_match(&t);
            return Reading::Value(if setting.kind == "object" {
                json!({ "enabled": on })
            } else {
                json!(on)
            });
        }
        "string" => {
            return std::iter::once(&*QUOTED)
                .chain(NAMES.iter())
                .find_map(|pattern| pattern.captures(text))
                .map_or(Reading::Nothing, |caps| Reading::Value(json!(&caps[1])))
        }
        "agent_id" => {
            return agents
                .iter()
                .find(|agent| has_word(&t, agent) || has_word(&t, &agent.replace('_', " ")))
                .map_or(Reading::Nothing, |agent| Reading::Value(json!(agent)))
        }
        _ => {}
    }
    let inverse = word_list(words, "inverse").any(|word| t.contains(word));
    let multiply = |by: f64| Reading::Change(json!({"op": "multiply", "by": clean(by)}));
    if DOUBLE.is_match(&t) {
        return multiply(if inverse { 0.5 } else { 2.0 });
    }
    if HALVE.is_match(&t) {
        return multiply(if inverse { 2.0 } else { 0.5 });
    }
    if ONE_MORE.is_match(&t) {
        return Reading::Change(json!({"op": "add", "by": 1}));
    }
    for quantity in &numbers {
        let after: String = t[quantity.end..].chars().take(20).collect();
        let up = AFTER_UP.is_match(&after);
        if up || AFTER_DOWN.is_match(&after) {
            let by = if setting.kind == "length" {
                quantity.number
            } else {
                convert(quantity.number, &quantity.unit, setting)
            };
            return Reading::Change(json!({"op": "add", "by": clean(if up { by } else { -by })}));
        }
    }
    if setting.kind == "length" {
        if HALF_SCREEN.is_match(&t) {
            return Reading::Value(json!({"width": 50, "unit": "%"}));
        }
        for quantity in &numbers {
            let unit = match quantity.unit.as_str() {
                "%" | "percent" => "%",
                "px" | "pixel" => "px",
                _ => continue,
            };
            return Reading::Value(json!({"width": clean(quantity.number), "unit": unit}));
        }
    } else if let Some(quantity) = numbers.first() {
        return Reading::Value(clean(convert(quantity.number, &quantity.unit, setting)));
    }
    if word_list(words, "increase").any(|word| t.contains(word)) || INCREASE.is_match(&t) {
        return Reading::Change(json!({"op": "increase"}));
    }
    if word_list(words, "decrease").any(|word| t.contains(word)) || DECREASE.is_match(&t) {
        return Reading::Change(json!({"op": "decrease"}));
    }
    Reading::Nothing
}

/// The parameters a request names for a parameterized setting (the account level), or the
/// parameter it still needs.
pub fn parameters(text: &str, setting: &Setting) -> Result<Map<String, Value>, String> {
    if !setting.parameterized {
        return Ok(Map::new());
    }
    let t = text.to_lowercase();
    TIERS
        .iter()
        .find(|(_, pattern)| pattern.is_match(&t))
        .map(|(tier, _)| Map::from_iter([("tier".to_owned(), json!(tier))]))
        .ok_or_else(|| "tier".to_owned())
}

/// Why a proposed number is outside the setting's bounds, or `None`. `limit` is a tighter
/// maximum the caller derived from related live values.
pub fn out_of_range(setting: &Setting, value: &Value, limit: Option<f64>) -> Option<String> {
    if !matches!(setting.kind.as_str(), "number" | "integer" | "length") {
        return None;
    }
    let number = value.get("width").unwrap_or(value).as_f64()?;
    let shown = |n: f64| clean(n).to_string();
    if value.get("unit").and_then(Value::as_str) == Some("%") && number > 100.0 {
        return Some("max 100%".to_owned());
    }
    match (setting.max, setting.min) {
        (Some(max), _) if number > max => Some(format!("max {}", shown(max))),
        (_, Some(min)) if number < min => Some(format!("min {}", shown(min))),
        _ => limit
            .filter(|limit| number > *limit)
            .map(|limit| format!("max {}", shown(limit))),
    }
}

/// How strongly `text` names a value for `setting`: 2 for the setting's own value words, one of
/// its enum values, a unit of its family or quoted text; 1 for a value read only generically (a
/// bare number, on/off, "bigger"); 0 for none.
pub fn evidence(text: &str, setting: &Setting, words: &Value, agents: &[String]) -> u8 {
    if read(text, setting, words, agents) == Reading::Nothing {
        return 0;
    }
    let (t, numbers) = quantities(text);
    let own = phrases(words).any(|(phrase, _)| t.contains(phrase))
        || word_list(words, "increase")
            .chain(word_list(words, "decrease"))
            .any(|word| t.contains(word));
    let enum_value = setting.kind == "enum" && setting.choices.iter().any(|c| has_word(&t, c));
    let quoted = setting.kind == "string" && QUOTE.is_match(text);
    let unit = if setting.unit.is_empty() && setting.kind == "length" {
        "%"
    } else {
        &setting.unit
    };
    let family = setting_scale(unit).map(|(family, _)| family);
    let same_family = family.is_some_and(|family| {
        numbers
            .iter()
            .any(|q| scale(&q.unit).is_some_and(|(f, _)| f == family))
    });
    let length_unit = setting.kind == "length"
        && numbers
            .iter()
            .any(|q| matches!(q.unit.as_str(), "px" | "pixel" | "%" | "percent"));
    if own || enum_value || quoted || same_family || length_unit {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_words_and_units() {
        let (t, numbers) = quantities("Refresh every minute, five pixels and 1.5k tokens");
        assert_eq!(t, "refresh every 1 minute, 5 pixels and 1.5k tokens");
        let read: Vec<(f64, &str)> = numbers
            .iter()
            .map(|q| (q.number, q.unit.as_str()))
            .collect();
        assert_eq!(read, [(1.0, "minute"), (5.0, "pixel"), (1500.0, "token")]);
    }

    #[test]
    fn words_need_boundaries() {
        assert!(has_word("set it to left please", "left"));
        assert!(!has_word("leftover", "left"));
        assert!(has_word("operator_reviewable.", "operator_reviewable"));
        assert!(!has_word("anything", ""));
    }
}
