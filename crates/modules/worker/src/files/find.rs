//! Plain-language code lookup, first stage: every named block under the search path is
//! scored on the query's words (BM25 over identifiers split at case changes and
//! punctuation). The host reranks the best of them with the matcher model, which this
//! isolated worker cannot reach.
use super::outline::Block;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Blocks handed to the host for reranking.
pub(super) const CANDIDATES: usize = 200;
/// Blocks kept from one walk; a larger tree reports a limited scan.
pub(super) const MAX_BLOCKS: usize = 60_000;
const DOC_LINES: usize = 4;
const BODY_LINES: usize = 5;
const TEXT_CHARS: usize = 600;

/// One named block with the text it is scored on.
pub(super) struct Candidate {
    label: String,
    start: usize,
    end: usize,
    name: String,
    text: String,
    words: Vec<String>,
}

/// The blocks of one file, each described by the comment above it, its header and its
/// first body lines.
pub(super) fn candidates(label: &str, lines: &[&str], blocks: &[Block]) -> Vec<Candidate> {
    let markdown = label.ends_with(".md");
    blocks
        .iter()
        .map(|block| {
            let header = block.start - 1;
            let mut doc = Vec::new();
            let mut at = header;
            while !markdown && at > 0 && doc.len() < DOC_LINES && is_comment(lines[at - 1]) {
                at -= 1;
                doc.insert(0, lines[at].trim());
            }
            let body = lines
                .iter()
                .skip(block.start)
                .take(BODY_LINES.min(block.end.saturating_sub(block.start)))
                .map(|line| line.trim());
            let text: String = std::iter::once(format!("{label}:"))
                .chain(doc.into_iter().map(str::to_owned))
                .chain(std::iter::once(lines[header].trim().to_owned()))
                .chain(body.map(str::to_owned))
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(TEXT_CHARS)
                .collect();
            Candidate {
                label: label.to_owned(),
                start: block.start,
                end: block.end,
                name: block.name.clone(),
                words: tokens(&text),
                text,
            }
        })
        .collect()
}

fn is_comment(line: &str) -> bool {
    let line = line.trim_start();
    ["//", "/*", "*", "--", "\"\"\""]
        .iter()
        .any(|marker| line.starts_with(marker))
        || (line.starts_with('#') && !line.starts_with("#["))
}

/// Lowercase words of `text`: each identifier, plus its parts split at `-`, `_`, `$` and
/// lower-to-upper case changes, so `sessionDropdown` and `session-dropdown` share words.
pub(super) fn tokens(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for word in text
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '-')))
        .filter(|word| word.chars().next().is_some_and(char::is_alphabetic))
    {
        let mut parts = Vec::new();
        let mut part = String::new();
        let mut previous_lower = false;
        for c in word.chars() {
            if matches!(c, '-' | '_' | '$') || (c.is_uppercase() && previous_lower) {
                parts.push(std::mem::take(&mut part));
            }
            if !matches!(c, '-' | '_' | '$') {
                part.extend(c.to_lowercase());
            }
            previous_lower = c.is_lowercase();
        }
        parts.push(part);
        let split = parts.len() > 1;
        words.extend(parts.into_iter().filter(|part| part.chars().count() > 1));
        if split {
            words.push(word.to_lowercase());
        }
    }
    words
}

/// The best [`CANDIDATES`] blocks for `query` as JSON text for the host, best first;
/// blocks sharing no word with the query are left out.
pub(super) fn output(found: &[Candidate], query: &str, limited: bool) -> String {
    let wanted: HashSet<String> = tokens(query).into_iter().collect();
    let total = found.len().max(1) as f64;
    let average = found.iter().map(|c| c.words.len()).sum::<usize>() as f64 / total;
    let mut documents: HashMap<&str, f64> = HashMap::new();
    for candidate in found {
        let seen: HashSet<&str> = candidate.words.iter().map(String::as_str).collect();
        for word in seen.into_iter().filter(|word| wanted.contains(*word)) {
            *documents.entry(word).or_default() += 1.0;
        }
    }
    let mut scored: Vec<(f64, &Candidate)> = found
        .iter()
        .filter_map(|candidate| {
            let mut counts: HashMap<&str, f64> = HashMap::new();
            for word in &candidate.words {
                *counts.entry(word.as_str()).or_default() += 1.0;
            }
            let length = candidate.words.len() as f64 / average.max(1.0);
            let score: f64 = documents
                .iter()
                .map(|(word, with)| {
                    let count = counts.get(word).copied().unwrap_or(0.0);
                    let rarity = (1.0 + (total - with + 0.5) / (with + 0.5)).ln();
                    rarity * count * 2.2 / (count + 1.2 * (0.25 + 0.75 * length))
                })
                .sum();
            (score > 0.0).then_some((score, candidate))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let candidates: Vec<Value> = scored
        .into_iter()
        .take(CANDIDATES)
        .map(|(score, c)| {
            json!({"path":c.label,"start":c.start,"end":c.end,"name":c.name,"text":c.text,"score":score})
        })
        .collect();
    json!({"find":true,"blocks":found.len(),"limited":limited,"candidates":candidates}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_at_case_and_punctuation() {
        assert_eq!(
            tokens("sessionDropdown .session-dropdown-menu x"),
            [
                "session",
                "dropdown",
                "sessiondropdown",
                "session",
                "dropdown",
                "menu",
                "session-dropdown-menu"
            ]
        );
    }

    #[test]
    fn blocks_rank_by_shared_rare_words_with_their_comment() {
        let text = "// Moves the menu to the body while open.\nfunction openMenu() {\n  document.body.append(menu);\n}\nfunction other() {\n  return 1;\n}\n";
        let lines: Vec<&str> = text.lines().collect();
        let blocks = super::super::outline::blocks("a.js", text);
        let found = candidates("a.js", &lines, &blocks);
        assert!(
            found[0].text.contains("Moves the menu"),
            "{}",
            found[0].text
        );
        let ranked: Value = serde_json::from_str(&output(&found, "menu body open", false)).unwrap();
        assert_eq!(ranked["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(ranked["candidates"][0]["name"], "function openMenu()");
        assert_eq!(ranked["candidates"][0]["start"], 2);
    }
}
