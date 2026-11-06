//! Keyword search over chat text shared by `search_this_session` and `search_sessions`.
//! Words match by stem ("gardening" finds "garden") and common irregular forms ("ran"
//! finds "run"); results rank by BM25 and nearby matches merge into one excerpt.
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

/// Excerpt and result bounds, read from Agent Management's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Characters kept on each side of a match.
    pub context_chars: usize,
    /// Longest merged excerpt; matches beyond it start a new result.
    pub max_result_chars: usize,
    /// Results one session may contribute.
    pub per_session: usize,
    /// Results one search returns.
    pub total: usize,
}

impl Limits {
    /// Bounds from an ability settings object, each clamped to its setting's range.
    #[must_use]
    pub fn from_settings(settings: &Value) -> Self {
        let read = |key: &str, default: u64, min: u64, max: u64| {
            settings[key].as_u64().unwrap_or(default).clamp(min, max) as usize
        };
        Self {
            context_chars: read("search_context_chars", 500, 50, 2000),
            max_result_chars: read("search_max_result_chars", 2000, 200, 8000),
            per_session: read("search_results_per_session", 3, 1, 10),
            total: read("search_results_total", 8, 1, 20),
        }
    }
    /// These bounds as the model sees them.
    #[must_use]
    pub fn describe(&self) -> Value {
        json!({"context_chars":self.context_chars,"max_result_chars":self.max_result_chars,
            "results_per_session":self.per_session,"results_total":self.total})
    }
}

/// One searchable text: `group` is its session (for the per-session limit) and `meta`
/// the identifying fields copied into each of its results.
pub struct Doc<'a> {
    pub group: &'a str,
    pub meta: Value,
    pub text: &'a str,
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "did", "do", "does", "for",
    "from", "had", "has", "have", "he", "her", "his", "how", "i", "if", "in", "into", "is", "it",
    "its", "me", "my", "no", "not", "of", "on", "or", "our", "she", "so", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "this", "to", "us", "was", "we", "were",
    "what", "when", "where", "which", "who", "why", "will", "with", "you", "your",
];

/// Irregular forms mapped to the word their regular forms stem to.
const IRREGULAR: &[(&str, &str)] = &[
    ("ran", "run"),
    ("went", "go"),
    ("gone", "go"),
    ("was", "be"),
    ("were", "be"),
    ("been", "be"),
    ("ate", "eat"),
    ("eaten", "eat"),
    ("saw", "see"),
    ("seen", "see"),
    ("took", "take"),
    ("taken", "take"),
    ("gave", "give"),
    ("given", "give"),
    ("made", "make"),
    ("came", "come"),
    ("bought", "buy"),
    ("brought", "bring"),
    ("thought", "think"),
    ("taught", "teach"),
    ("caught", "catch"),
    ("found", "find"),
    ("told", "tell"),
    ("sold", "sell"),
    ("held", "hold"),
    ("kept", "keep"),
    ("left", "leave"),
    ("felt", "feel"),
    ("met", "meet"),
    ("paid", "pay"),
    ("said", "say"),
    ("sent", "send"),
    ("spent", "spend"),
    ("built", "build"),
    ("began", "begin"),
    ("begun", "begin"),
    ("knew", "know"),
    ("known", "know"),
    ("grew", "grow"),
    ("grown", "grow"),
    ("drew", "draw"),
    ("drawn", "draw"),
    ("flew", "fly"),
    ("flown", "fly"),
    ("wrote", "write"),
    ("written", "write"),
    ("drove", "drive"),
    ("driven", "drive"),
    ("rode", "ride"),
    ("ridden", "ride"),
    ("spoke", "speak"),
    ("spoken", "speak"),
    ("broke", "break"),
    ("broken", "break"),
    ("chose", "choose"),
    ("chosen", "choose"),
    ("froze", "freeze"),
    ("frozen", "freeze"),
    ("forgot", "forget"),
    ("forgotten", "forget"),
    ("got", "get"),
    ("gotten", "get"),
    ("sang", "sing"),
    ("sung", "sing"),
    ("swam", "swim"),
    ("swum", "swim"),
    ("drank", "drink"),
    ("drunk", "drink"),
    ("sat", "sit"),
    ("stood", "stand"),
    ("understood", "understand"),
    ("lost", "lose"),
    ("won", "win"),
    ("wore", "wear"),
    ("worn", "wear"),
    ("fell", "fall"),
    ("fallen", "fall"),
    ("hid", "hide"),
    ("hidden", "hide"),
    ("led", "lead"),
    ("fed", "feed"),
    ("slept", "sleep"),
    ("dealt", "deal"),
    ("meant", "mean"),
    ("heard", "hear"),
    ("better", "good"),
    ("best", "good"),
    ("worse", "bad"),
    ("worst", "bad"),
    ("children", "child"),
    ("men", "man"),
    ("women", "woman"),
    ("people", "person"),
    ("mice", "mouse"),
    ("geese", "goose"),
    ("feet", "foot"),
    ("teeth", "tooth"),
    ("analyses", "analysis"),
];

/// The comparable form of one lowercase word.
// shortcut: suffix rules plus an irregular table, not a full Snowball stemmer; upgrade if
// searches miss common word forms.
#[must_use]
pub fn stem(word: &str) -> String {
    let word = IRREGULAR
        .iter()
        .find(|(form, _)| *form == word)
        .map_or(word, |(_, base)| *base);
    let len = word.chars().count();
    if len <= 3 || !word.is_ascii() {
        return word.to_owned();
    }
    // Plurals and -ies/-ied first, so "waters" and "water" reach the same root.
    let mut root = if let Some(r) = word
        .strip_suffix("ies")
        .or_else(|| word.strip_suffix("ied"))
        .filter(|r| r.len() >= 2)
    {
        format!("{r}y")
    } else if let Some(r) = word.strip_suffix("es").filter(|r| {
        ["s", "x", "z", "ch", "sh"]
            .iter()
            .any(|end| r.ends_with(end))
    }) {
        r.to_owned()
    } else if let Some(r) = word
        .strip_suffix('s')
        .filter(|_| !word.ends_with("ss") && !word.ends_with("us"))
    {
        r.to_owned()
    } else {
        word.to_owned()
    };
    for suffix in ["ing", "ed", "er", "ly"] {
        if let Some(r) = root.strip_suffix(suffix).filter(|r| r.len() >= 3) {
            root = r.to_owned();
            let bytes = root.as_bytes();
            let n = bytes.len();
            if bytes[n - 1] == bytes[n - 2] && !b"aeioulsz".contains(&bytes[n - 1]) {
                root.pop();
            }
            break;
        }
    }
    if root.len() >= 4 && root.ends_with('e') {
        root.pop();
    }
    root
}

/// Words of `text` with their byte ranges, lowercased.
fn words(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (at, ch) in text.char_indices().chain([(text.len(), ' ')]) {
        match (ch.is_alphanumeric(), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                out.push((from, at, text[from..at].to_lowercase()));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// The stems a query searches for; empty when it has only stopwords.
#[must_use]
pub fn query_terms(query: &str) -> BTreeSet<String> {
    words(query)
        .into_iter()
        .filter(|(_, _, w)| !STOPWORDS.contains(&w.as_str()))
        .map(|(_, _, w)| stem(&w))
        .collect()
}

/// Text from byte `from` back `chars` characters.
fn back(text: &str, from: usize, chars: usize) -> usize {
    text[..from]
        .char_indices()
        .rev()
        .nth(chars.saturating_sub(1))
        .map_or(0, |(at, _)| at)
}
/// Text from byte `from` forward `chars` characters.
fn forward(text: &str, from: usize, chars: usize) -> usize {
    text[from..]
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(at, _)| from + at)
}

struct Hit<'a> {
    score: f64,
    order: usize,
    group: &'a str,
    result: Value,
}

/// Ranked, merged excerpts of `docs` for `terms`. `docs` come newest first; ties keep that
/// order. A merged excerpt counts once against both limits.
#[must_use]
pub fn search(terms: &BTreeSet<String>, docs: &[Doc<'_>], limits: &Limits) -> Vec<Value> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let parsed: Vec<Vec<(usize, usize, String)>> = docs
        .iter()
        .map(|doc| {
            words(doc.text)
                .into_iter()
                .map(|(from, to, w)| (from, to, stem(&w)))
                .collect()
        })
        .collect();
    let average =
        parsed.iter().map(Vec::len).sum::<usize>().max(1) as f64 / docs.len().max(1) as f64;
    let mut frequency: HashMap<&str, usize> = HashMap::new();
    for words in &parsed {
        let present: BTreeSet<&str> = words
            .iter()
            .map(|(_, _, s)| s.as_str())
            .filter(|s| terms.contains(*s))
            .collect();
        for term in present {
            *frequency.entry(term).or_default() += 1;
        }
    }
    let total = docs.len() as f64;
    let idf = |term: &str| {
        let df = frequency.get(term).copied().unwrap_or(0) as f64;
        (1.0 + (total - df + 0.5) / (df + 0.5)).ln()
    };
    let mut hits = Vec::new();
    for (order, (doc, words)) in docs.iter().zip(&parsed).enumerate() {
        let matches: Vec<&(usize, usize, String)> =
            words.iter().filter(|(_, _, s)| terms.contains(s)).collect();
        if matches.is_empty() {
            continue;
        }
        let norm = K1 * (1.0 - B + B * words.len() as f64 / average);
        let mut groups: Vec<Vec<&(usize, usize, String)>> = Vec::new();
        for m in matches {
            if let Some(group) = groups.last_mut() {
                let last = group[group.len() - 1];
                let gap = doc.text[last.1..m.0].chars().count();
                let span = doc.text[group[0].0..m.1].chars().count() + 2 * limits.context_chars;
                if gap <= 2 * limits.context_chars && span <= limits.max_result_chars {
                    group.push(m);
                    continue;
                }
            }
            groups.push(vec![m]);
        }
        for group in groups {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for (_, _, s) in &group {
                *counts.entry(s.as_str()).or_default() += 1;
            }
            let score: f64 = counts
                .iter()
                .map(|(term, tf)| {
                    let tf = *tf as f64;
                    idf(term) * tf * (K1 + 1.0) / (tf + norm)
                })
                .sum();
            let from = back(doc.text, group[0].0, limits.context_chars);
            let to = forward(doc.text, group[group.len() - 1].1, limits.context_chars);
            let body: String = doc.text[from..to]
                .chars()
                .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
                .collect();
            let excerpt = format!(
                "{}{body}{}",
                if from > 0 { "…" } else { "" },
                if to < doc.text.len() { "…" } else { "" }
            );
            let matched: BTreeSet<String> = group
                .iter()
                .map(|(a, b, _)| doc.text[*a..*b].to_lowercase())
                .collect();
            let mut result = doc.meta.clone();
            result["excerpt"] = json!(excerpt);
            result["matches"] = json!(group.len());
            result["matched_words"] = json!(matched);
            result["score"] = json!((score * 100.0).round() / 100.0);
            hits.push(Hit {
                score,
                order,
                group: doc.group,
                result,
            });
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.order.cmp(&b.order)));
    let mut per_group: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for hit in hits {
        let used = per_group.entry(hit.group).or_default();
        if *used >= limits.per_session {
            continue;
        }
        *used += 1;
        out.push(hit.result);
        if out.len() == limits.total {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(context: usize, max: usize, per_session: usize, total: usize) -> Limits {
        Limits {
            context_chars: context,
            max_result_chars: max,
            per_session,
            total,
        }
    }

    #[test]
    fn word_forms_share_a_stem() {
        for group in [
            &["garden", "gardens", "gardening", "gardened", "gardener"][..],
            &["run", "runs", "running", "ran"],
            &["cat", "cats"],
            &["party", "parties"],
            &["carry", "carried", "carries"],
            &["box", "boxes"],
            &["make", "making", "made", "makes"],
            &["stop", "stopped", "stopping"],
            &["mouse", "mice"],
            &["fall", "falling", "falls", "fell"],
        ] {
            let stems: BTreeSet<String> = group.iter().map(|w| stem(w)).collect();
            assert_eq!(stems.len(), 1, "{group:?} -> {stems:?}");
        }
        for group in [
            &["water", "waters"][..],
            &["family", "families"],
            &["class", "classes"],
        ] {
            let stems: BTreeSet<String> = group.iter().map(|w| stem(w)).collect();
            assert_eq!(stems.len(), 1, "{group:?} -> {stems:?}");
        }
        assert_eq!(stem("status"), "status");
    }

    #[test]
    fn queries_drop_stopwords() {
        assert_eq!(
            query_terms("What is the Gardening plan?"),
            ["garden", "plan"].iter().map(|s| (*s).to_owned()).collect()
        );
        assert!(query_terms("what is the").is_empty());
    }

    #[test]
    fn nearby_matches_merge_and_count_once() {
        let filler = "x ".repeat(100);
        let text = format!("{filler}cat one cat two cat {filler}{filler}{filler}cat end");
        let docs = [Doc {
            group: "s",
            meta: json!({"turn":1}),
            text: &text,
        }];
        let terms = query_terms("feline cat");
        let hits = search(&terms, &docs, &limits(20, 400, 3, 8));
        assert_eq!(hits.len(), 2, "{hits:?}");
        let counts: Vec<u64> = hits
            .iter()
            .map(|h| h["matches"].as_u64().unwrap())
            .collect();
        assert!(counts.contains(&3) && counts.contains(&1));
        let merged = hits.iter().find(|h| h["matches"] == 3).unwrap();
        let excerpt = merged["excerpt"].as_str().unwrap();
        assert!(excerpt.starts_with('…') && excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= "cat one cat two cat".len() + 40 + 2);
        assert_eq!(merged["turn"], 1);
        // One per session leaves only the better window.
        assert_eq!(search(&terms, &docs, &limits(20, 400, 1, 8)).len(), 1);
    }

    #[test]
    fn long_runs_split_at_the_maximum_length() {
        let text = "cat ".repeat(200);
        let docs = [Doc {
            group: "s",
            meta: json!({}),
            text: &text,
        }];
        let hits = search(&query_terms("cat"), &docs, &limits(10, 200, 10, 20));
        assert!(hits.len() > 1);
        assert!(hits
            .iter()
            .all(|h| h["excerpt"].as_str().unwrap().chars().count() <= 202));
    }

    #[test]
    fn ranking_prefers_rare_and_multiple_terms_and_limits_hold() {
        let texts = [
            "the garden needs water",
            "we were running late to the garden party",
            "garden",
            "gardening and composting tips",
            "nothing here",
        ];
        let docs: Vec<Doc<'_>> = texts
            .iter()
            .enumerate()
            .map(|(n, text)| Doc {
                group: if n < 2 { "a" } else { "b" },
                meta: json!({"n":n}),
                text,
            })
            .collect();
        let hits = search(
            &query_terms("composting gardens"),
            &docs,
            &limits(100, 2000, 3, 8),
        );
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0]["n"], 3);
        let capped = search(&query_terms("garden"), &docs, &limits(100, 2000, 1, 8));
        assert_eq!(capped.len(), 2);
        assert_eq!(
            search(&query_terms("garden"), &docs, &limits(100, 2000, 3, 2)).len(),
            2
        );
        let ran = search(&query_terms("ran"), &docs, &limits(100, 2000, 3, 8));
        assert_eq!(ran.len(), 1);
        assert_eq!(ran[0]["matched_words"], json!(["running"]));
    }

    #[test]
    fn settings_are_clamped() {
        let limits =
            Limits::from_settings(&json!({"search_context_chars":1,"search_results_total":99}));
        assert_eq!(limits.context_chars, 50);
        assert_eq!(limits.total, 20);
        assert_eq!(limits.max_result_chars, 2000);
        assert_eq!(limits.per_session, 3);
    }
}
