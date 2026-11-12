//! Choosing a setting for a request. The page a request is made on sets the layer (app
//! dashboard → app, an agent's dashboard → that agent, the user's own page → user), so only
//! that page's settings are candidates. Scores rank them; a card is pre-selected only when the
//! request carries strong evidence for the top setting's value and no close rival has the same.
//! Otherwise the answer is a short pick list, or `other_page` when the best match lives
//! elsewhere.
use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::Result;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};

use crate::values::{self, Reading};

/// Gates for bge-small cosine scores, tuned on the 120-request evaluation set (`eval/`) for no
/// wrong pre-selection: below `NONE` nothing matches unless the evidence is strong; a best
/// match off the page wins over the page's own below `PRE` or `MARGIN` behind; on-page rivals
/// within `MARGIN` of the top are close.
const NONE: f32 = 0.65;
const PRE: f32 = 0.75;
const MARGIN: f32 = 0.10;
/// Settings a pick list offers at most.
const PICKS: usize = 4;

const STOP: &str = "a an the to of for and or in on at my me i it is be should can please set \
    make change turn with by from as that this so let have when its are was will all";

static SPLIT: LazyLock<Regex> = LazyLock::new(|| regex(r"\s*(?:,|\band\b)\s*"));
static APP_WIDE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b(everyone|all users|for all|all agents|everybody)\b"));
static WORD: LazyLock<Regex> = LazyLock::new(|| regex("[a-z0-9]+"));

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

/// One descriptor, as the matcher reads it.
#[derive(Debug, Clone)]
pub struct Setting {
    pub id: String,
    pub description: String,
    pub kind: String,
    pub layers: Vec<String>,
    pub unit: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub choices: Vec<String>,
    pub parameterized: bool,
    /// The description and example requests, each embedded on its own.
    pub texts: Vec<String>,
}

impl Setting {
    /// Read a validated catalog descriptor.
    pub fn from_descriptor(descriptor: &Value) -> Self {
        let text = |key: &str| descriptor[key].as_str().unwrap_or_default().to_owned();
        let list = |key: &str| -> Vec<String> {
            descriptor[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let description = text("description");
        Self {
            id: text("id"),
            texts: std::iter::once(description.clone())
                .chain(list("examples"))
                .collect(),
            description,
            kind: text("type"),
            layers: list("layers"),
            unit: text("unit"),
            min: descriptor["min"].as_f64(),
            max: descriptor["max"].as_f64(),
            choices: list("enum"),
            parameterized: descriptor.get("parameters").is_some(),
        }
    }
}

/// One request: its text, the page it was made on (`app`, `agent:<id>` or `user`), the agent
/// ids it may name, and tighter maxima the caller derived from live values (`{id: number}`).
pub struct Request<'a> {
    pub text: &'a str,
    pub page: &'a str,
    pub agents: &'a [String],
    pub limits: &'a Map<String, Value>,
}

/// The request's agent names ("the shared default agent"), when it names any agents.
struct Names(Option<Regex>);

impl Names {
    fn new(agents: &[String]) -> Self {
        let names: Vec<String> = agents
            .iter()
            .map(|agent| regex::escape(&agent.replace('_', " ")))
            .collect();
        Self((!names.is_empty()).then(|| {
            RegexBuilder::new(&format!(r"\b(?:the |my )?({}) agent\b", names.join("|")))
                .case_insensitive(true)
                .build()
                .expect("escaped names")
        }))
    }
}

/// The settings and a scorer giving each setting's similarity to a text.
pub struct Matcher<S> {
    settings: Vec<Setting>,
    words: Vec<Value>,
    score: S,
    memo: HashMap<String, Vec<f32>>,
}

/// Content words of a request: lowercase, stop words dropped, simple plurals singular.
pub fn words(text: &str) -> Vec<String> {
    let stop: Vec<&str> = STOP.split_whitespace().collect();
    WORD.find_iter(&text.to_lowercase())
        .map(|w| w.as_str())
        .filter(|w| !stop.contains(w))
        .map(|w| {
            if w.len() > 4 && w.ends_with("ies") {
                format!("{}y", &w[..w.len() - 3])
            } else if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
                w[..w.len() - 1].to_owned()
            } else {
                w.to_owned()
            }
        })
        .collect()
}

impl<S: FnMut(&str) -> Result<Vec<f32>>> Matcher<S> {
    /// `score(text)` returns one score per setting, in order.
    pub fn new(settings: Vec<Setting>, score: S) -> Self {
        let all = values::value_words();
        let words = settings
            .iter()
            .map(|s| all.get(&s.id).cloned().unwrap_or(Value::Null))
            .collect();
        Self {
            settings,
            words,
            score,
            memo: HashMap::new(),
        }
    }

    pub fn settings(&self) -> &[Setting] {
        &self.settings
    }

    /// The answer for one request: `{"outcome": "card"|"pick"|"multiple"|"other_page"|
    /// "out_of_range"|"no_match", ...}`.
    pub fn decide(&mut self, request: &Request) -> Result<Value> {
        self.memo.clear();
        let names = Names::new(request.agents);
        self.decide_part(request, &names, request.text, true)
    }

    fn decide_part(
        &mut self,
        r: &Request,
        names: &Names,
        text: &str,
        split: bool,
    ) -> Result<Value> {
        let (order, scores) = self.ranked(names, text)?;
        let best = order[0];
        if scores[best] < NONE && !(scores[best] > 0.0 && self.evidence(r, text, best) == 2) {
            return Ok(json!({"outcome": "no_match"}));
        }
        if split {
            let parts: Vec<&str> = SPLIT
                .split(text)
                .filter(|part| !words(part).is_empty())
                .collect();
            if let [first, second] = parts[..] {
                // Two clauses whose own top settings differ and each accept a value: never one
                // card for the whole request.
                let mut tops = Vec::new();
                for part in [first, second] {
                    let (part_order, part_scores) = self.ranked(names, part)?;
                    let top = part_order
                        .into_iter()
                        .find(|i| self.off_page(r, names, part, *i).is_none());
                    tops.push(
                        top.filter(|i| part_scores[*i] >= NONE && self.evidence(r, part, *i) > 0),
                    );
                }
                if let [Some(a), Some(b)] = tops[..] {
                    if a != b {
                        let id = self.settings[a].id.clone();
                        let mut answer = self.decide_part(r, names, first, false)?;
                        if answer["setting"] != json!(id) {
                            answer = json!({"outcome": "pick", "candidates": [id]});
                        }
                        let other = &self.settings[b].id;
                        return Ok(json!({"outcome": "multiple", "first": answer, "other": other}));
                    }
                }
            }
        }
        let on: Vec<usize> = order
            .iter()
            .copied()
            .filter(|i| self.off_page(r, names, text, *i).is_none())
            .collect();
        if on.first() != Some(&best)
            && (on.is_empty()
                || scores[on[0]] < PRE
                || (scores[best] - scores[on[0]]) / scores[best] >= MARGIN)
        {
            return Ok(json!({
                "outcome": "other_page",
                "setting": self.settings[best].id,
                "reason": self.off_page(r, names, text, best),
            }));
        }
        let Some(&top) = on.first() else {
            return Ok(json!({"outcome": "no_match"}));
        };
        let close = on[1..]
            .iter()
            .take(2)
            .filter(|i| scores[**i] >= scores[top] * (1.0 - MARGIN));
        let blocked = self.evidence(r, text, top) < 2
            || close.into_iter().any(|i| self.evidence(r, text, *i) >= 2);
        if blocked {
            let candidates: Vec<&str> = on
                .iter()
                .take(PICKS)
                .map(|i| self.settings[*i].id.as_str())
                .collect();
            return Ok(json!({"outcome": "pick", "candidates": candidates}));
        }
        let setting = &self.settings[top];
        let pick =
            |needs: &str| json!({"outcome": "pick", "candidates": [setting.id], "needs": needs});
        let params = match values::parameters(text, setting) {
            Ok(params) => params,
            Err(needs) => return Ok(pick(&needs)),
        };
        let (key, value) = match values::read(text, setting, &self.words[top], r.agents) {
            Reading::Value(value) => ("value", value),
            Reading::Change(change) => ("change", change),
            Reading::Needs(detail) => return Ok(pick(&detail)),
            Reading::Nothing => return Ok(pick("value")),
        };
        if key == "value" {
            let limit = r.limits.get(&setting.id).and_then(Value::as_f64);
            if let Some(limit) = values::out_of_range(setting, &value, limit) {
                return Ok(
                    json!({"outcome": "out_of_range", "setting": setting.id, "limit": limit}),
                );
            }
        }
        let mut card = json!({"outcome": "card", "setting": setting.id, "layer": layer(r.page)});
        card[key] = value;
        if !params.is_empty() {
            card["params"] = Value::Object(params);
        }
        Ok(card)
    }

    /// Settings by score, best first, for `text` with agent names removed (they name whose
    /// setting, not which).
    fn ranked(&mut self, names: &Names, text: &str) -> Result<(Vec<usize>, Vec<f32>)> {
        let stripped = match &names.0 {
            Some(pattern) => pattern.replace_all(text, " ").into_owned(),
            None => text.to_owned(),
        };
        let query = if words(&stripped).is_empty() {
            text.to_owned()
        } else {
            stripped
        };
        let scores = match self.memo.get(&query) {
            Some(scores) => scores.clone(),
            None => {
                let scores = (self.score)(&query)?;
                self.memo.insert(query, scores.clone());
                scores
            }
        };
        let mut order: Vec<usize> = (0..scores.len()).collect();
        order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
        anyhow::ensure!(!order.is_empty(), "No settings to match");
        Ok((order, scores))
    }

    fn evidence(&self, r: &Request, text: &str, i: usize) -> u8 {
        values::evidence(text, &self.settings[i], &self.words[i], r.agents)
    }

    /// Why setting `i` cannot be changed from this page for `text`, or `None`.
    fn off_page(&self, r: &Request, names: &Names, text: &str, i: usize) -> Option<&'static str> {
        let setting = &self.settings[i];
        let t = text.to_lowercase();
        let here = layer(r.page);
        if !setting.layers.iter().any(|l| l == here) {
            return Some("not on this page");
        }
        if here != "app" && APP_WIDE.is_match(&t) {
            return Some("app-wide wording off the app dashboard");
        }
        if let Some(pattern) = &names.0 {
            if let Some(named) = pattern.captures(&t) {
                let agent = format!("agent:{}", named[1].replace(' ', "_"));
                if setting.layers.iter().any(|l| l == "agent") && r.page != agent {
                    return Some("names another agent");
                }
            }
            // "Make the research agent summarize later": the agent is who, not a value.
            let subject = pattern.find_iter(&t).any(|name| {
                let rest = &t[name.end()..];
                rest.starts_with(char::is_whitespace)
                    && rest
                        .trim_start()
                        .starts_with(|c: char| c.is_alphanumeric() || c == '_')
            });
            if setting.kind == "agent_id" && subject {
                return Some("the named agent is the subject, not a value");
            }
        }
        None
    }
}

/// The layer a page writes: `app`, `agent` or `user`.
pub fn layer(page: &str) -> &str {
    page.split(':').next().unwrap_or(page)
}
