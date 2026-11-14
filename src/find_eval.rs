//! `webagent-v3 --find-eval CHECKOUT FILE...` scores `file_search` `find` ranking against
//! request sets (`crates/modules/administration/eval/find-eval.jsonl`): one line per case
//! `{"id", "query", "path", "expect": {"path", "name"}}`, where `name` is a substring of the
//! block's header so cases survive line moves. It runs the worker's scan and the live blend
//! over CHECKOUT with the installed model, so it runs inside the built image, and prints each
//! case's rank with and without the matcher, then a summary per file.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use webagent_administration::find_ranking::Ranking;
use webagent_administration_host::tools::{find_order, find_text};

/// Word-score weights compared against the live `words_weight`; the eval reads the live
/// `find-ranking.json` like the tool.
const SWEEP: [f64; 5] = [0.5, 0.4, 0.3, 0.2, 0.1];

/// 1-based rank of the expected block in `order`, if present.
fn rank_of(candidates: &[Value], order: &[usize], expect: &Value) -> Option<usize> {
    let path = expect["path"].as_str().unwrap_or_default();
    let name = expect["name"].as_str().unwrap_or_default();
    order
        .iter()
        .position(|&at| {
            candidates[at]["path"] == path
                && candidates[at]["name"]
                    .as_str()
                    .is_some_and(|n| n.contains(name))
        })
        .map(|at| at + 1)
}

#[derive(Default)]
struct Totals {
    shown_blocks: usize,
    cases: usize,
    top1: usize,
    shown: usize,
    reciprocal: f64,
}

impl Totals {
    fn add(&mut self, rank: Option<usize>) {
        self.cases += 1;
        self.top1 += usize::from(rank == Some(1));
        self.shown += usize::from(rank.is_some_and(|r| r <= self.shown_blocks));
        self.reciprocal += rank.map_or(0.0, |r| 1.0 / r as f64);
    }

    fn summary(&self) -> Value {
        let share = |n: f64| (n / self.cases.max(1) as f64 * 1000.0).round() / 1000.0;
        json!({"top1": share(self.top1 as f64), "shown": share(self.shown as f64),
            "mrr": share(self.reciprocal)})
    }
}

pub fn evaluate(args: &[String]) -> Result<()> {
    let (checkout, files) = args
        .split_first()
        .context("Usage: --find-eval CHECKOUT FILE...")?;
    let ranking = Ranking::load();
    let totals = || Totals {
        shown_blocks: ranking.shown_blocks,
        ..Totals::default()
    };
    let mut ranker = webagent_inference::Ranker::new(webagent_inference::load_embedder()?);
    for file in files {
        let (mut blended, mut words) = (totals(), totals());
        let mut sweep: Vec<Totals> = SWEEP.iter().map(|_| totals()).collect();
        let text = std::fs::read_to_string(file).with_context(|| format!("Read {file}"))?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let case: Value = serde_json::from_str(line)?;
            let query = case["query"].as_str().context("Case query required")?;
            let path = case["path"].as_str().unwrap_or(".");
            let started = std::time::Instant::now();
            let found: Value = serde_json::from_str(
                &webagent_worker::files::find_lookup(
                    checkout.as_ref(),
                    path,
                    query,
                    Some(&ranking.worker_limits()),
                )
                .map_err(anyhow::Error::msg)?,
            )?;
            let candidates = found["candidates"].as_array().cloned().unwrap_or_default();
            let texts: Vec<Vec<String>> = candidates.iter().map(find_text).collect();
            let meaning = ranker.scores(query, &texts)?;
            let elapsed = started.elapsed().as_millis();
            let order = find_order(&candidates, Some(&meaning), ranking.words_weight);
            for (weight, totals) in SWEEP.iter().zip(&mut sweep) {
                totals.add(rank_of(
                    &candidates,
                    &find_order(&candidates, Some(&meaning), *weight),
                    &case["expect"],
                ));
            }
            let rank = rank_of(&candidates, &order, &case["expect"]);
            let word_rank = rank_of(
                &candidates,
                &find_order(&candidates, None, 1.0),
                &case["expect"],
            );
            let top: Vec<Value> = order
                .iter()
                .take(3)
                .map(|&at| {
                    json!(format!(
                        "{}:{}  {}",
                        candidates[at]["path"].as_str().unwrap_or(""),
                        candidates[at]["start"],
                        candidates[at]["name"].as_str().unwrap_or("")
                    ))
                })
                .collect();
            println!(
                "{}",
                json!({"id": case["id"], "rank": rank, "words_rank": word_rank, "top": top,
                    "blocks": found["blocks"], "areas": found["areas"],
                    "candidates": candidates.len(), "ms": elapsed, "limited": found["limited"]})
            );
            blended.add(rank);
            words.add(word_rank);
        }
        println!(
            "{}",
            json!({"file": file, "cases": blended.cases, "blended": blended.summary(),
                "words_only": words.summary(),
                "sweep": SWEEP.iter().zip(&sweep).map(|(weight, totals)| json!({"words_weight": weight, "scores": totals.summary()})).collect::<Vec<_>>()})
        );
    }
    Ok(())
}
