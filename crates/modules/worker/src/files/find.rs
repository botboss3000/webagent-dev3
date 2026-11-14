//! Plain-language code lookup, first stage: every named block under the search path is
//! scored on the query's words (BM25 over identifiers split at case changes and
//! punctuation). The host reranks the best of them with the matcher model, which this
//! isolated worker cannot reach.
use super::outline::Block;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// A lookup's scan and candidate budgets, sent by the host from the live
/// `find-ranking.json` as `find_limits`; the defaults match that file.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Most candidates one area hands to the host for reranking.
    pub per_area: usize,
    /// Candidates handed to the host in all; the matcher takes at most 1024 texts per request.
    pub total: usize,
    /// Files and folders one area's scan may visit.
    pub entries: i64,
    /// Seconds one area's scan may take.
    pub seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_area: 200,
            total: 400,
            entries: 10_000,
            seconds: 6,
        }
    }
}

impl Limits {
    /// The limits in a request's `find_limits`, bounded; the defaults when absent.
    pub fn from_value(value: Option<&Value>) -> Result<Self, &'static str> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let number = |key: &str, max: u64| {
            value[key]
                .as_u64()
                .filter(|n| (1..=max).contains(n))
                .ok_or("invalid find limits")
        };
        let per_area = usize::try_from(number("candidates_per_area", 1000)?).unwrap_or(1000);
        let total = usize::try_from(number("total_candidates", 1024)?).unwrap_or(1024);
        Ok(Self {
            per_area,
            total: total.max(per_area).min(1024),
            entries: i64::try_from(number("scan_entries", 100_000)?).unwrap_or(100_000),
            seconds: number("scan_seconds", 30)?,
        })
    }
}
/// Blocks kept from one walk; a larger tree reports a limited scan.
pub(super) const MAX_BLOCKS: usize = 60_000;
const DOC_LINES: usize = 4;
/// Furthest a `/* … */` comment's opening line is looked for above its closing line.
const COMMENT_BLOCK_LINES: usize = 20;
const BODY_LINES: usize = 5;
const TEXT_CHARS: usize = 600;
const GIST_CHARS: usize = 120;

/// One named block with the text it is scored on.
pub(super) struct Candidate {
    label: String,
    start: usize,
    end: usize,
    name: String,
    gist: String,
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
            while !markdown && doc.len() < DOC_LINES {
                let Some(start) = comment_above(&lines, at) else {
                    break;
                };
                doc.splice(0..0, lines[start..at].iter().map(|line| line.trim()));
                at = start;
            }
            doc.truncate(DOC_LINES);
            let body: Vec<&str> = lines
                .iter()
                .skip(block.start)
                .take(BODY_LINES.min(block.end.saturating_sub(block.start)))
                .map(|line| line.trim())
                .collect();
            let gist = gist(&doc, &body);
            let text: String = std::iter::once(format!("{label}:"))
                .chain(doc.into_iter().map(str::to_owned))
                .chain(std::iter::once(lines[header].trim().to_owned()))
                .chain(body.iter().map(|line| (*line).to_owned()))
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
                gist,
                words: tokens(&text),
                text,
            }
        })
        .collect()
}

/// One line telling what the block does: the comment above it, else its first non-empty
/// body line, cut to [`GIST_CHARS`].
fn gist(doc: &[&str], body: &[&str]) -> String {
    let comment = doc
        .iter()
        .map(|line| {
            line.trim_start_matches(['/', '*', '#', '-', '"', '!', ' '])
                .trim()
        })
        .find(|line| !line.is_empty());
    let line = comment
        .or_else(|| body.iter().copied().find(|line| !line.is_empty()))
        .unwrap_or("");
    let mut gist: String = line.chars().take(GIST_CHARS).collect();
    if line.chars().count() > GIST_CHARS {
        gist.push('…');
    }
    gist
}

/// First line of the comment that ends just above line `at`: one comment line, or a whole
/// `/* … */` block whose inner lines carry no `*`, as plain CSS comments are written.
fn comment_above(lines: &[&str], at: usize) -> Option<usize> {
    let above = lines.get(at.checked_sub(1)?)?;
    if above.trim_end().ends_with("*/") && !above.contains("/*") {
        return (at.saturating_sub(COMMENT_BLOCK_LINES)..at - 1)
            .rev()
            .find(|&line| lines[line].contains("/*"));
    }
    is_comment(above).then(|| at - 1)
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

/// The best blocks for `query` as JSON text for the host, best first: each area keeps up to
/// an equal share of the total (at most `per_area`), and slots a small area
/// leaves unused go to the next best blocks anywhere. Blocks sharing no word with the query
/// are left out.
pub(super) fn output(
    areas: &[(String, Vec<Candidate>)],
    query: &str,
    limited: bool,
    limits: Limits,
) -> String {
    let found: Vec<(&str, &Candidate)> = areas
        .iter()
        .flat_map(|(area, blocks)| blocks.iter().map(move |block| (area.as_str(), block)))
        .collect();
    let wanted: HashSet<String> = tokens(query).into_iter().collect();
    let total = found.len().max(1) as f64;
    let average = found.iter().map(|(_, c)| c.words.len()).sum::<usize>() as f64 / total;
    let mut documents: HashMap<&str, f64> = HashMap::new();
    for (_, candidate) in &found {
        let seen: HashSet<&str> = candidate.words.iter().map(String::as_str).collect();
        for word in seen.into_iter().filter(|word| wanted.contains(*word)) {
            *documents.entry(word).or_default() += 1.0;
        }
    }
    let mut scored: Vec<(f64, &str, &Candidate)> = found
        .iter()
        .filter_map(|&(area, candidate)| {
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
            (score > 0.0).then_some((score, area, candidate))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let share = (limits.total / areas.len().max(1)).clamp(1, limits.per_area);
    let mut taken: HashMap<&str, usize> = HashMap::new();
    let mut keep = vec![false; scored.len()];
    for (at, (_, area, _)) in scored.iter().enumerate() {
        let count = taken.entry(area).or_default();
        if *count < share {
            *count += 1;
            keep[at] = true;
        }
    }
    let mut room = limits
        .total
        .saturating_sub(keep.iter().filter(|kept| **kept).count());
    for (at, (_, area, _)) in scored.iter().enumerate() {
        let count = taken.entry(area).or_default();
        if room > 0 && !keep[at] && *count < limits.per_area {
            *count += 1;
            keep[at] = true;
            room -= 1;
        }
    }
    let candidates: Vec<Value> = scored
        .into_iter()
        .zip(keep)
        .filter(|(_, kept)| *kept)
        .map(|((score, area, c), _)| {
            json!({"path":c.label,"start":c.start,"end":c.end,"name":c.name,"gist":c.gist,
                "text":c.text,"score":score,"area":area})
        })
        .collect();
    json!({"find":true,"blocks":found.len(),"areas":areas.len(),"limited":limited,"candidates":candidates}).to_string()
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
        let areas = [("a.js".to_owned(), found)];
        let ranked: Value =
            serde_json::from_str(&output(&areas, "menu body open", false, Limits::default()))
                .unwrap();
        assert_eq!(ranked["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(ranked["candidates"][0]["name"], "function openMenu()");
        assert_eq!(ranked["candidates"][0]["start"], 2);
        assert_eq!(
            ranked["candidates"][0]["gist"],
            "Moves the menu to the body while open."
        );
    }

    #[test]
    fn every_area_keeps_its_candidates_when_one_area_has_many() {
        let block = |label: &str, at: usize| {
            let text = format!("// Opens the menu.\nfunction menu{at}() {{\n  open();\n}}\n");
            let lines: Vec<&str> = text.lines().collect();
            let blocks = super::super::outline::blocks(label, &text);
            candidates(label, &lines, &blocks)
        };
        let big: Vec<Candidate> = (0..1500).flat_map(|at| block("big.js", at)).collect();
        let small = block("small.js", 0);
        let areas = [("big".to_owned(), big), ("small".to_owned(), small)];
        let ranked: Value =
            serde_json::from_str(&output(&areas, "open menu", false, Limits::default())).unwrap();
        let listed = ranked["candidates"].as_array().unwrap();
        assert!(listed.iter().any(|c| c["area"] == "small"));
        assert_eq!(
            listed.iter().filter(|c| c["area"] == "big").count(),
            Limits::default().per_area
        );
        assert_eq!(ranked["areas"], 2);
    }

    #[test]
    fn a_plain_css_block_comment_is_the_blocks_doc() {
        let text = ".a { color: red; }\n/* Header controls scroll sideways\n   on narrow screens. */\n.header-actions {\n  overflow-x: auto;\n}\n";
        let lines: Vec<&str> = text.lines().collect();
        let blocks = super::super::outline::blocks("x.css", text);
        let found = candidates("x.css", &lines, &blocks);
        let header = found
            .iter()
            .find(|c| c.name.contains("header-actions"))
            .unwrap();
        assert_eq!(header.gist, "Header controls scroll sideways");
        assert!(header.text.contains("on narrow screens."));
    }
}
