//! The matcher process. It loads the descriptor catalog from the checkout and the model, embeds
//! every setting's description and examples once, then answers until its input closes.
//!
//! Requests arrive one JSON object per line on stdin: `{"id": ID, "text": TEXT, "page":
//! "app"|"agent:<id>"|"user", "agents": [AGENT_ID...], "limits": {SETTING_ID: MAX}}`. Replies are
//! JSON lines on stdout: `{"ready": true, "settings": N}` once loaded, then `{"id": ID, "result":
//! ANSWER}` (see [`Matcher::decide`]) or `{"id": ID, "error": REASON}`. Nothing is stored and no
//! network is used. A changed catalog is read when the process next starts.
//!
//! `{"id": ID, "op": "rank", "query": TEXT, "items": [[TEXT...]...]}` scores any other catalog
//! (tools, skills) by each item's closest text: `{"id": ID, "result": {"scores": [N...]}}`.
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::embedder::{cosine, Embedder, MODEL_DIR, QUERY_PREFIX, RUNTIME};
use crate::matcher::{Matcher, Request, Setting};

/// Longer requests are refused rather than truncated.
const MAX_TEXT_CHARS: usize = 500;
/// Bounds on one `rank` request.
const MAX_ITEMS: usize = 1024;
const MAX_ITEM_TEXTS: usize = 16;
const MAX_ITEM_TEXT_CHARS: usize = 2000;
/// Cached text embeddings; the cache restarts when full.
const MAX_CACHED: usize = 8192;

/// Scores items by their closest text to a query, embedding each text once.
pub struct Ranker {
    embedder: Rc<RefCell<Embedder>>,
    cache: HashMap<String, Vec<f32>>,
}

impl Ranker {
    pub fn new(embedder: Embedder) -> Self {
        Self {
            embedder: Rc::new(RefCell::new(embedder)),
            cache: HashMap::new(),
        }
    }

    /// Each item's best cosine between `query` and its texts (-1 for an item with none).
    pub fn scores(&mut self, query: &str, items: &[Vec<String>]) -> Result<Vec<f32>> {
        let query = self
            .embedder
            .borrow_mut()
            .embed(&format!("{QUERY_PREFIX}{query}"))?;
        let mut scores = Vec::with_capacity(items.len());
        for texts in items {
            let mut best = -1.0f32;
            for text in texts {
                if !self.cache.contains_key(text) {
                    if self.cache.len() >= MAX_CACHED {
                        self.cache.clear();
                    }
                    let vector = self.embedder.borrow_mut().embed(text)?;
                    self.cache.insert(text.clone(), vector);
                }
                best = best.max(cosine(&self.cache[text], &query));
            }
            scores.push(best);
        }
        Ok(scores)
    }
}

/// The catalog's settings, from the checkout at `root`.
pub fn settings(root: &Path) -> Result<Vec<Setting>> {
    Ok(webagent_configuration::catalog::load(root)?
        .iter()
        .map(|s| Setting::from_descriptor(&s.descriptor))
        .collect())
}

/// The installed model, shared by the setting matcher and [`Ranker`].
pub fn load_embedder() -> Result<Embedder> {
    Embedder::load(Path::new(RUNTIME), Path::new(MODEL_DIR))
}

/// A matcher over the live catalog scored by the installed model, and a ranker on the same model.
fn load() -> Result<(Matcher<impl FnMut(&str) -> Result<Vec<f32>>>, Ranker)> {
    let settings = settings(Path::new(webagent_configuration::live::CHECKOUT))?;
    let ranker = Ranker::new(load_embedder()?);
    let embedder = ranker.embedder.clone();
    let vectors = settings
        .iter()
        .map(|s| {
            s.texts
                .iter()
                .map(|t| embedder.borrow_mut().embed(t))
                .collect()
        })
        .collect::<Result<Vec<Vec<Vec<f32>>>>>()?;
    // A setting scores its closest text: the description or one of its examples.
    let score = move |text: &str| -> Result<Vec<f32>> {
        let query = embedder
            .borrow_mut()
            .embed(&format!("{QUERY_PREFIX}{text}"))?;
        Ok(vectors
            .iter()
            .map(|texts| texts.iter().map(|v| cosine(v, &query)).fold(-1.0, f32::max))
            .collect())
    };
    Ok((Matcher::new(settings, score), ranker))
}

/// Answer requests from stdin until it closes.
pub fn serve() -> Result<()> {
    let (mut matcher, mut ranker) = load()?;
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{}",
        json!({"ready": true, "settings": matcher.settings().len()})
    )?;
    out.flush()?;
    for line in io::stdin().lock().lines() {
        let Ok(request) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let answered = if request["op"] == "rank" {
            rank(&mut ranker, &request)
        } else {
            answer(&mut matcher, &request)
        };
        let reply = match answered {
            Ok(result) => json!({"id": request["id"], "result": result}),
            Err(error) => json!({"id": request["id"], "error": error.to_string()}),
        };
        writeln!(out, "{reply}")?;
        out.flush()?;
    }
    Ok(())
}

fn rank(ranker: &mut Ranker, request: &Value) -> Result<Value> {
    let query = request["query"].as_str().context("Rank query required")?;
    if query.trim().is_empty() || query.chars().count() > MAX_TEXT_CHARS {
        bail!("Rank query must be 1 to {MAX_TEXT_CHARS} characters");
    }
    let items: Vec<Vec<String>> =
        serde_json::from_value(request["items"].clone()).context("Rank items required")?;
    if items.len() > MAX_ITEMS
        || items.iter().any(|texts| {
            texts.len() > MAX_ITEM_TEXTS
                || texts
                    .iter()
                    .any(|t| t.chars().count() > MAX_ITEM_TEXT_CHARS)
        })
    {
        bail!("Rank request exceeds its bounds");
    }
    Ok(json!({"scores": ranker.scores(query, &items)?}))
}

fn answer<S: FnMut(&str) -> Result<Vec<f32>>>(
    matcher: &mut Matcher<S>,
    request: &Value,
) -> Result<Value> {
    let text = request["text"].as_str().context("Request text required")?;
    if text.chars().count() > MAX_TEXT_CHARS {
        bail!("Request longer than {MAX_TEXT_CHARS} characters");
    }
    let page = request["page"].as_str().context("Request page required")?;
    if !matches!(page, "app" | "user") && !page.starts_with("agent:") {
        bail!("Unknown page {page}");
    }
    let agents: Vec<String> = request["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str().map(str::to_owned))
        .collect();
    let limits = request["limits"].as_object().cloned().unwrap_or_default();
    matcher.decide(&Request {
        text,
        page,
        agents: &agents,
        limits: &limits,
    })
}

/// Score request sets (`eval/*.jsonl`): one line per case, then a summary line per file.
/// `eval/context.json` gives the agents and limits the sets were written against.
pub fn evaluate(files: &[String]) -> Result<()> {
    let (mut matcher, _) = load()?;
    for file in files {
        let path = Path::new(file);
        let context: Value = serde_json::from_str(
            &std::fs::read_to_string(path.with_file_name("context.json"))
                .context("Evaluation context unavailable")?,
        )?;
        let mut request = context.clone();
        let mut rows = Vec::new();
        for line in std::fs::read_to_string(path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let case: Value = serde_json::from_str(line)?;
            request["text"] = case["request"].clone();
            request["page"] = case["page"].clone();
            let got = answer(&mut matcher, &request)?;
            let (right, wrong) = grade(&case["expect"], &got);
            println!(
                "{}",
                json!({"id": case["id"], "got": got, "right": right, "wrong": wrong})
            );
            rows.push((case, got, right, wrong));
        }
        let single: Vec<_> = rows
            .iter()
            .filter(|(case, ..)| case["expect"].get("outcome").is_none())
            .collect();
        let first_in_pick = single
            .iter()
            .filter(|(case, got, right, _)| {
                *right
                    || (got["outcome"] == "pick"
                        && got["candidates"][0] == case["expect"]["setting"])
            })
            .count();
        let share = |n: usize, of: usize| (n as f64 / of.max(1) as f64 * 1000.0).round() / 1000.0;
        println!(
            "{}",
            json!({
                "file": file,
                "single": single.len(),
                "fully_right": share(single.iter().filter(|r| r.2).count(), single.len()),
                "first_in_pick": share(first_in_pick, single.len()),
                "all_right": share(rows.iter().filter(|r| r.2).count(), rows.len()),
                "wrong": rows.iter().filter(|r| r.3).map(|r| &r.0["id"]).collect::<Vec<_>>(),
            })
        );
    }
    Ok(())
}

/// Whether `got` is the expected answer, and whether it pre-selected a wrong card.
fn grade(expect: &Value, got: &Value) -> (bool, bool) {
    let card = got["outcome"] == "card";
    match expect.get("outcome").and_then(Value::as_str) {
        None => {
            let right = card && same_card(got, expect);
            (right, card && !right)
        }
        Some("multiple") => {
            let multiple = got["outcome"] == "multiple";
            let first = &got["first"];
            let right =
                multiple && same_card(first, &expect["first"]) && got["other"] == expect["other"];
            let wrong = card
                || (multiple && first["outcome"] == "card" && !same_card(first, &expect["first"]));
            (right, wrong)
        }
        Some(outcome @ ("out_of_range" | "other_page")) => (
            got["outcome"] == outcome && got["setting"] == expect["setting"],
            card,
        ),
        Some("pick") => {
            let offered = got["candidates"].as_array().into_iter().flatten();
            let wanted = expect["candidates"].as_array().cloned().unwrap_or_default();
            let right = got["outcome"] == "pick" && offered.into_iter().any(|c| wanted.contains(c));
            (right, card)
        }
        Some(_) => (got["outcome"] == "no_match", card),
    }
}

fn same_card(card: &Value, expect: &Value) -> bool {
    let params = |v: &Value| {
        v.get("params")
            .cloned()
            .unwrap_or(Value::Object(Map::new()))
    };
    card["setting"] == expect["setting"]
        && card["layer"] == expect["layer"]
        && params(card) == params(expect)
        && match expect.get("value") {
            Some(value) => card.get("value") == Some(value),
            None => card.get("change") == expect.get("change"),
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::{self, Reading};

    fn checkout() -> Option<std::path::PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        root.join(webagent_configuration::catalog::INDEX)
            .is_file()
            .then_some(root)
    }

    /// Every single-setting request in the tuned evaluation set reads to its expected value
    /// and parameters once the right setting is chosen (retrieval aside).
    #[test]
    fn eval_set_values_read_right() -> Result<()> {
        let Some(root) = checkout() else {
            return Ok(());
        };
        let settings = settings(&root)?;
        let words = values::value_words();
        let agents = ["research".to_owned(), "shared_default".to_owned()];
        let cases = include_str!("../eval/eval-set.jsonl");
        let mut wrong = Vec::new();
        for case in cases.lines().map(serde_json::from_str::<Value>) {
            let case = case?;
            let expect = &case["expect"];
            let Some(setting) = settings.iter().find(|s| expect["setting"] == s.id.as_str()) else {
                continue;
            };
            if expect.get("outcome").is_some() {
                continue;
            }
            let text = case["request"].as_str().unwrap_or_default();
            let setting_words = words.get(&setting.id).cloned().unwrap_or(Value::Null);
            let read = values::read(text, setting, &setting_words, &agents);
            let params = values::parameters(text, setting).unwrap_or_default();
            let want = match expect.get("value") {
                Some(value) => Reading::Value(value.clone()),
                None => Reading::Change(expect["change"].clone()),
            };
            let want_params = expect["params"].as_object().cloned().unwrap_or_default();
            if read != want || params != want_params {
                wrong.push(format!("{}: {read:?} {params:?}", case["id"]));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        Ok(())
    }

    #[test]
    fn value_words_name_catalog_settings() -> Result<()> {
        let Some(root) = checkout() else {
            return Ok(());
        };
        let ids: Vec<String> = settings(&root)?.into_iter().map(|s| s.id).collect();
        let words = values::value_words();
        let unknown: Vec<&String> = words.keys().filter(|id| !ids.contains(id)).collect();
        assert!(unknown.is_empty(), "{unknown:?}");
        Ok(())
    }

    #[test]
    fn grading_matches_the_prototype() {
        let expect = json!({"setting": "a", "layer": "app", "value": 3});
        assert_eq!(
            grade(
                &expect,
                &json!({"outcome": "card", "setting": "a", "layer": "app", "value": 3})
            ),
            (true, false)
        );
        assert_eq!(
            grade(
                &expect,
                &json!({"outcome": "card", "setting": "b", "layer": "app", "value": 3})
            ),
            (false, true)
        );
        assert_eq!(
            grade(&expect, &json!({"outcome": "pick", "candidates": ["a"]})),
            (false, false)
        );
    }
}
