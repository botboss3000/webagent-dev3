//! present_chat_component: declarative chat panels and the server-side check of their answers.
use super::{authorize, short_title, text};
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::digest;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentArgs {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    placement: Option<String>,
    #[serde(default = "empty_object")]
    data: Value,
    #[serde(default)]
    component_id: Option<String>,
    expected_revision: u64,
    /// Closes the panel the user no longer needs to answer.
    #[serde(default)]
    dismiss: bool,
}
/// Hard limit for panel prompts and hints; the tool skill asks for 200 or fewer. The margin
/// only protects against runaway text.
const PANEL_TEXT: usize = 400;
fn empty_object() -> Value {
    json!({})
}
pub(super) fn component(args: ComponentArgs, fallback_id: &str) -> Result<(String, Value, u64)> {
    let kind = args.kind.as_str();
    if kind == "approval" {
        // A model-drawn approval could be mistaken for a real permission control.
        bail!("Approvals come only from the runtime; ask with a choice panel instead");
    }
    if !matches!(kind, "todo_list" | "status" | "choice" | "form") {
        bail!("Unsupported chat component type");
    }
    let id = args.component_id.unwrap_or_else(|| fallback_id.to_owned());
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("Invalid component ID");
    }
    let placement = args.placement.unwrap_or_else(|| "inline".to_owned());
    if !matches!(placement.as_str(), "inline" | "hover" | "sticky") {
        bail!("Invalid component placement");
    }
    let fallback = match kind {
        "todo_list" => "Checklist",
        "status" => "Overview",
        "choice" => "Choice",
        "approval" => "Approval",
        _ => "Form",
    };
    let title = if matches!(kind, "choice" | "form") && !args.dismiss {
        // The title prefixes the user's answer, so it must name the panel's topic.
        let title = text(&args.title, 80)?;
        if title.is_empty() || title.contains('\n') {
            bail!("Choice and form panels need a one-line title naming their topic");
        }
        title
    } else {
        short_title(&args.title, fallback)?
    };
    if args.dismiss {
        let spec = json!({"id":id,"type":kind,"title":title,"placement":placement,
            "data":{},"lifecycle":{"dismissed":true},"authority":"display_only"});
        return Ok((id, spec, args.expected_revision));
    }
    let data = args
        .data
        .as_object()
        .ok_or_else(|| anyhow!("Component data must be an object"))?;
    let cleaned = match kind {
        "todo_list" => {
            let items = data
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Checklist items required"))?;
            if items.is_empty() || items.len() > 100 {
                bail!("Expected 1..100 checklist items");
            }
            let mut clean = Vec::new();
            for (n, item) in items.iter().enumerate() {
                clean.push(json!({"id":format!("item-{}",n+1),
                    "label":text(item["label"].as_str().ok_or_else(||anyhow!("Checklist label required"))?,240)?,
                    "done":item["done"].as_bool().unwrap_or(false)}));
            }
            json!({"items":clean})
        }
        "status" => {
            let sections = data
                .get("sections")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Status sections required"))?;
            if sections.is_empty() || sections.len() > 20 {
                bail!("Expected 1..20 status sections");
            }
            let mut clean = Vec::new();
            for section in sections {
                clean.push(json!({"label":text(section["label"].as_str().ok_or_else(||anyhow!("Status label required"))?,80)?,
                    "value":text(section["value"].as_str().unwrap_or(""),1200)?,
                    "status":section["status"].as_str().filter(|s|matches!(*s,"good"|"warning"|"danger"))}));
            }
            json!({"sections":clean})
        }
        "choice" => {
            let options = data
                .get("options")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Choice options required"))?;
            if options.is_empty() || options.len() > 8 {
                bail!("Expected 1..8 choice options");
            }
            let mut clean = Vec::new();
            for (n, option) in options.iter().enumerate() {
                // A clicked label is sent as the user's message, so it must read as one.
                let label = text(
                    option["label"]
                        .as_str()
                        .or_else(|| option.as_str())
                        .ok_or_else(|| anyhow!("Choice label required"))?,
                    80,
                )?;
                if label.is_empty() || label.contains('\n') {
                    bail!("Choice labels must be one non-empty line");
                }
                clean.push(json!({"id":format!("option-{}",n+1),"label":label}));
            }
            let prompt = data
                .get("prompt")
                .or_else(|| data.get("question"))
                .and_then(Value::as_str)
                .unwrap_or("");
            json!({"prompt":text(prompt,PANEL_TEXT)?,"options":clean})
        }
        "form" => {
            let fields = data
                .get("fields")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Form fields required"))?;
            if fields.is_empty() || fields.len() > 20 {
                bail!("Expected 1..20 form fields");
            }
            let mut clean = Vec::new();
            for field in fields {
                let name = field["name"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Field name required"))?;
                if name.is_empty()
                    || name.len() > 40
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!("Invalid field name");
                }
                let field_type = field["type"].as_str().unwrap_or("text");
                if !matches!(
                    field_type,
                    "text" | "email" | "number" | "select" | "multi" | "radio" | "textarea"
                ) {
                    bail!("Unsupported field type");
                }
                let picks = matches!(field_type, "multi" | "radio");
                let mut choices = Vec::new();
                if picks {
                    let options = field["options"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if options.is_empty() || options.len() > 8 {
                        bail!("Multi and radio fields need 1..8 options");
                    }
                    for option in options {
                        let label = text(
                            option
                                .as_str()
                                .ok_or_else(|| anyhow!("Multi and radio options must be text"))?,
                            80,
                        )?;
                        if label.is_empty() || label.contains('\n') {
                            bail!("Multi and radio options must be one non-empty line");
                        }
                        choices.push(label);
                    }
                } else if field_type == "select" {
                    for option in field["options"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .take(20)
                    {
                        choices.push(text(
                            option
                                .as_str()
                                .ok_or_else(|| anyhow!("Select options must be text"))?,
                            80,
                        )?);
                    }
                    if choices.is_empty() {
                        bail!("Select fields need options");
                    }
                }
                // `other` adds a "Something else…" row to a pick list; `attach` adds a + button
                // that stages files on the answer message; `comment` (on unless false) adds an
                // optional comment box under a pick list. All are ignored on other types.
                let flag = |key: &str| field[key].as_bool().unwrap_or(false);
                let comment = picks && field["comment"].as_bool() != Some(false);
                let attaches = matches!(field_type, "text" | "textarea");
                clean.push(
                    json!({"name":name,"label":text(field["label"].as_str().unwrap_or(name),120)?,
                    "type":field_type,"required":flag("required"),"options":choices,
                    "hint":text(field["hint"].as_str().unwrap_or(""),PANEL_TEXT)?,
                    "other":picks && flag("other"),"attach":attaches && flag("attach"),
                    "comment":comment}),
                );
            }
            // A paged form shows one field per page with a step counter, Back and Skip.
            let paged = data.get("paged").and_then(Value::as_bool).unwrap_or(false);
            json!({"prompt":text(data.get("prompt").and_then(Value::as_str).unwrap_or(""),PANEL_TEXT)?,
                "fields":clean,"paged":paged})
        }
        _ => unreachable!(),
    };
    // Choices and forms answer as the user's own message through normal admission; the panel
    // itself carries no authority.
    let authority = if matches!(kind, "choice" | "form") {
        "user_reply"
    } else {
        "display_only"
    };
    let spec = json!({"id":id,"type":kind,"title":title,"placement":placement,
        "data":cleaned,"lifecycle":{},"authority":authority});
    if serde_json::to_vec(&spec)?.len() > 24 * 1024 {
        bail!("Component exceeds 24 KiB");
    }
    Ok((id, spec, args.expected_revision))
}
/// One answer as the agent reads it: list items joined by commas, an empty one "skipped".
fn shown(value: &Value) -> String {
    match value {
        Value::Array(items) if !items.is_empty() => items
            .iter()
            .map(|item| item.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", "),
        Value::String(text) if !text.is_empty() => text.clone(),
        _ => "skipped".to_owned(),
    }
}

/// Checks the user's `answer` to reply panel `spec` and builds the message the agent
/// receives: "<title>: <value>" for one uncommented answer, else the title and one
/// "<label>: <value>" line per field, each comment on a "<label> — comment: <text>" line.
/// A choice answer is `{option_id}`; a form answer is `{values, comments}` keyed by field.
pub fn answer_text(spec: &Value, answer: &Value) -> Result<String> {
    if spec["authority"] != "user_reply" {
        bail!("Only choice and form panels take answers");
    }
    let title = spec["title"].as_str().unwrap_or("Answer");
    if spec["type"] == "choice" {
        let option = spec["data"]["options"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|option| answer["option_id"].is_string() && option["id"] == answer["option_id"])
            .ok_or_else(|| anyhow!("Unknown panel option"))?;
        return Ok(format!(
            "{title}: {}",
            option["label"].as_str().unwrap_or("")
        ));
    }
    let fields = spec["data"]["fields"]
        .as_array()
        .ok_or_else(|| anyhow!("Panel has no fields"))?;
    let values = answer["values"]
        .as_object()
        .ok_or_else(|| anyhow!("Form answer values required"))?;
    let comments = match &answer["comments"] {
        Value::Null => serde_json::Map::new(),
        Value::Object(map) => map.clone(),
        _ => bail!("Form answer comments must be an object"),
    };
    let known = |name: &String| fields.iter().any(|field| field["name"] == name.as_str());
    if !values.keys().all(known) || !comments.keys().all(known) {
        bail!("Answer names a field the panel does not have");
    }
    let mut lines = Vec::new();
    for field in fields {
        let name = field["name"].as_str().unwrap_or_default();
        let label = field["label"].as_str().unwrap_or(name);
        let kind = field["type"].as_str().unwrap_or("text");
        let options: Vec<&str> = field["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let other = field["other"] == true;
        let value = values.get(name).cloned().unwrap_or(Value::Null);
        let picked: Vec<&str> = match (&value, kind) {
            (Value::Null, _) => Vec::new(),
            (Value::Array(items), "multi") => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .ok_or_else(|| anyhow!("Invalid answer for {label}"))
                })
                .collect::<Result<_>>()?,
            (Value::String(text), "multi") if text.is_empty() => Vec::new(),
            (Value::String(text), _) if kind != "multi" => {
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text.as_str()]
                }
            }
            _ => bail!("Invalid answer for {label}"),
        };
        if field["required"] == true && picked.iter().all(|text| text.trim().is_empty()) {
            bail!("{label} is required");
        }
        for text in &picked {
            let limit = if kind == "textarea" { 8000 } else { 2000 };
            if text.chars().count() > limit {
                bail!("Answer for {label} is too long");
            }
            match kind {
                "radio" | "multi" if !other && !options.contains(text) => {
                    bail!("Unknown option for {label}")
                }
                "select" if !options.contains(text) => bail!("Unknown option for {label}"),
                "number" if text.trim().parse::<f64>().is_err() => {
                    bail!("{label} must be a number")
                }
                "email" if !text.contains('@') => bail!("{label} must be an email address"),
                _ => {}
            }
        }
        if picked.len() > 9 || (kind != "multi" && picked.len() > 1) {
            bail!("Too many answers for {label}");
        }
        lines.push(format!("{label}: {}", shown(&value)));
        if let Some(comment) = comments.get(name) {
            let comment = comment
                .as_str()
                .ok_or_else(|| anyhow!("Comments must be text"))?;
            if field["comment"] != true || comment.chars().count() > 2000 {
                bail!("Invalid comment for {label}");
            }
            if !comment.trim().is_empty() {
                lines.push(format!("{label} — comment: {comment}"));
            }
        }
    }
    if lines.len() == 1 && fields.len() == 1 {
        let value = values.get(fields[0]["name"].as_str().unwrap_or_default());
        return Ok(format!("{title}: {}", shown(value.unwrap_or(&Value::Null))));
    }
    Ok(std::iter::once(title.to_owned())
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n"))
}
pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    sid: &str,
    rid: &str,
    pending_id: &str,
    name: &str,
    generation: &str,
    args: &Value,
) -> Result<Value> {
    let request: ComponentArgs = serde_json::from_value(args.clone())?;
    let fallback = format!("component-{}", &digest(&json!(pending_id))[..24]);
    let (id, spec, revision) = component(request, &fallback)?;
    if authorize(host, sid, rid, pending_id, name).await? != generation {
        bail!("Task run generation changed");
    }
    let saved = host
        .write_artifact(call, "component", &id, revision, &spec)
        .await?;
    Ok(json!({"status":"success","output":saved}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paged_forms_keep_radio_other_and_attach_only_where_they_apply() {
        let form = |fields: Value| {
            component(
                serde_json::from_value(json!({"type":"form","expected_revision":0,
                    "title":"Panel setup","data":{"paged":true,"fields":fields}}))
                .unwrap(),
                "c",
            )
        };
        let (_, spec, _) = form(json!([
            {"name":"layout","type":"radio","options":["A","B"],"other":true,"attach":true,
             "required":true,"hint":"Pick one"},
            {"name":"notes","type":"textarea","attach":true,"other":true},
            {"name":"when","type":"select","options":["Now"],"attach":true},
            {"name":"tools","type":"multi","options":["X"],"comment":false}
        ]))
        .unwrap();
        let fields = &spec["data"]["fields"];
        assert_eq!(spec["data"]["paged"], true);
        assert_eq!(fields[0]["type"], "radio");
        assert_eq!(fields[0]["options"][1], "B");
        assert_eq!(fields[0]["hint"], "Pick one");
        assert_eq!(
            (&fields[0]["other"], &fields[0]["attach"]),
            (&json!(true), &json!(false))
        );
        assert_eq!(
            (&fields[1]["other"], &fields[1]["attach"]),
            (&json!(false), &json!(true))
        );
        assert_eq!(fields[2]["attach"], false);
        // Pick lists carry a comment box unless it is turned off; other types never do.
        assert_eq!(
            (
                &fields[0]["comment"],
                &fields[1]["comment"],
                &fields[3]["comment"]
            ),
            (&json!(true), &json!(false), &json!(false))
        );
        assert!(form(json!([{"name":"layout","type":"radio","options":[]}])).is_err());
        assert!(form(json!([{"name":"layout","type":"radio","options":["A\nB"]}])).is_err());
        assert!(form(json!([{"name":"x","type":"html"}])).is_err());
    }

    /// Answers are checked against the stored panel and become the text the old browser
    /// built, so the agent reads the same message as before.
    #[test]
    fn answers_are_validated_and_rendered_on_the_server() {
        let (_, choice, _) = component(
            serde_json::from_value(json!({"type":"choice","expected_revision":0,
                "title":"For Bob","data":{"options":["Yes","No"]}}))
            .unwrap(),
            "c",
        )
        .unwrap();
        assert_eq!(
            answer_text(&choice, &json!({"option_id":"option-2"})).unwrap(),
            "For Bob: No"
        );
        assert!(answer_text(&choice, &json!({"option_id":"option-9"})).is_err());
        let (_, form, _) = component(
            serde_json::from_value(json!({"type":"form","expected_revision":0,"title":"Setup",
                "data":{"fields":[
                    {"name":"pick","label":"Pick","type":"multi","options":["A","B"]},
                    {"name":"size","label":"Size","type":"number","required":true}]}}))
            .unwrap(),
            "c",
        )
        .unwrap();
        let answer = json!({"values":{"pick":["A","B"],"size":"3"},"comments":{"pick":"both"}});
        assert_eq!(
            answer_text(&form, &answer).unwrap(),
            "Setup\nPick: A, B\nPick — comment: both\nSize: 3"
        );
        for bad in [
            json!({"values":{"pick":["C"],"size":"3"}}),
            json!({"values":{"pick":[]}}),
            json!({"values":{"size":"three"}}),
            json!({"values":{"size":"3","extra":"x"}}),
            json!({"values":{"size":"3"},"comments":{"size":"no comment box"}}),
        ] {
            assert!(answer_text(&form, &bad).is_err(), "{bad}");
        }
        let (_, status, _) = component(
            serde_json::from_value(json!({"type":"status","expected_revision":0,
                "data":{"sections":[{"label":"A","value":"B"}]}}))
            .unwrap(),
            "c",
        )
        .unwrap();
        assert!(answer_text(&status, &json!({"option_id":"option-1"})).is_err());
    }
}
