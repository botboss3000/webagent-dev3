//! `webagent-v3 --tool-match-eval FILE...` scores `list_tools`/`list_skills` ranking against
//! request sets (`crates/modules/composer/eval/tool-match-eval.jsonl`): one line per case
//! `{"id", "kind": "tool"|"skill", "query", "expect": [ID...]}`, then a summary per kind. It uses
//! the installed model and the same rules as the live tools, over the whole catalog (no access
//! filtering), so it runs where the model is installed: inside the built image.
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use webagent_abilities::tool_match;

struct Candidate {
    id: String,
    texts: Vec<String>,
    keywords: Vec<String>,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// Every tool and every bundled skill, with the texts and keywords the live tools rank by.
fn candidates() -> Result<(Vec<Candidate>, Vec<Candidate>)> {
    let root = std::env::var("V3_ABILITIES_ROOT")
        .unwrap_or_else(|_| "/opt/v3/plugins/abilities".to_owned());
    let catalog = crate::abilities::Catalog::load(&PathBuf::from(root))?;
    let tools = catalog
        .all_schemas()
        .iter()
        .chain(crate::mcp::all_schemas().iter())
        .filter_map(|schema| {
            let name = schema["name"].as_str()?;
            let (examples, keywords) = tool_match::hints(name);
            let description = schema["description"].as_str().unwrap_or_default();
            Some(Candidate {
                id: name.to_owned(),
                texts: tool_match::texts(name, description, &examples),
                keywords,
            })
        })
        .collect();
    let ids = catalog.ids();
    let state = json!({"active": ids, "suppressed": []});
    let mut skills = Vec::new();
    for id in &ids {
        for skill in catalog
            .bundled_skill_list(id, &json!({}), &state, |_| true, |_| true)
            .unwrap_or_default()
        {
            let skill_id = skill["skill_id"].as_str().unwrap_or_default().to_owned();
            let summary = skill["summary"].as_str().unwrap_or_default();
            skills.push(Candidate {
                texts: tool_match::texts(&skill_id, summary, &strings(&skill["examples"])),
                keywords: strings(&skill["keywords"]),
                id: skill_id,
            });
        }
    }
    Ok((tools, skills))
}

pub fn evaluate(files: &[String]) -> Result<()> {
    let (tools, skills) = candidates()?;
    let mut ranker = webagent_composer::Ranker::new(webagent_composer::load_embedder()?);
    let items = |list: &[Candidate]| list.iter().map(|c| c.texts.clone()).collect::<Vec<_>>();
    let (tool_items, skill_items) = (items(&tools), items(&skills));
    for file in files {
        let mut totals: std::collections::BTreeMap<String, (usize, usize, usize)> =
            Default::default();
        let text = std::fs::read_to_string(file).with_context(|| format!("Read {file}"))?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let case: Value = serde_json::from_str(line)?;
            let kind = case["kind"].as_str().unwrap_or("tool");
            let query = case["query"].as_str().context("Case query required")?;
            let (list, items) = if kind == "skill" {
                (&skills, &skill_items)
            } else {
                (&tools, &tool_items)
            };
            let scores = ranker.scores(query, items)?;
            let pairs: Vec<(&str, &[String])> = list
                .iter()
                .map(|c| (c.id.as_str(), &c.keywords[..]))
                .collect();
            let best = if kind == "skill" {
                tool_match::SKILL_BEST
            } else {
                tool_match::BEST
            };
            let ranked = tool_match::rank(query, &pairs, &scores, best);
            let top: Vec<&str> = ranked
                .iter()
                .take(3)
                .map(|r| list[r.index].id.as_str())
                .collect();
            let expect = strings(&case["expect"]);
            let right = top.iter().any(|id| expect.iter().any(|e| e == id));
            let wrong_best = ranked
                .first()
                .is_some_and(|r| r.best && !expect.iter().any(|e| *e == list[r.index].id));
            let scored: Vec<Value> = ranked
                .iter()
                .take(5)
                .map(|r| json!([list[r.index].id, (r.score * 100.0).round() / 100.0, r.best]))
                .collect();
            println!(
                "{}",
                json!({"id": case["id"], "kind": kind, "query": query, "top": scored,
                    "right": right, "wrong_best": wrong_best})
            );
            let entry = totals.entry(kind.to_owned()).or_default();
            entry.0 += 1;
            entry.1 += usize::from(right);
            entry.2 += usize::from(wrong_best);
        }
        for (kind, (cases, right, wrong)) in totals {
            let share = |n: usize| (n as f64 / cases.max(1) as f64 * 1000.0).round() / 1000.0;
            println!(
                "{}",
                json!({"file": file, "kind": kind, "cases": cases,
                    "top3_recall": share(right), "wrong_best_rate": share(wrong)})
            );
        }
    }
    Ok(())
}
