//! Private pure-function definitions; execution is a bounded interpreter in the isolated worker.
use crate::*;
use serde::Deserialize;
pub fn owns(name: &str) -> bool {
    matches!(name, "create_tool" | "run_soft_ability")
}
/// Tools whose schemas live in tool folders; `run_soft_ability` is built-in Base.
pub const TOOLS: &[&str] = &["create_tool"];
pub fn schemas() -> Vec<Value> {
    let mut schemas = crate::tools::folder_schemas(TOOLS);
    schemas.push(json!({"type":"function","name":"run_soft_ability",
        "description":"List private saved functions when name is omitted, or call one by name with JSON arguments. The current run's normal tool policy still applies.",
        "parameters":{"type":"object","properties":{"name":{"type":"string","maxLength":64},"arguments":{"type":"object"}},"required":[],"additionalProperties":false}}));
    schemas
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    name: Option<String>,
    description: Option<String>,
    code: Option<String>,
    expected_revision: Option<u64>,
    arguments: Option<Value>,
}
fn identifier(raw: &str) -> Result<&str> {
    if raw.is_empty()
        || raw.len() > 64
        || !raw.as_bytes()[0].is_ascii_alphabetic()
        || !raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(anyhow!("Function name must be a plain identifier"));
    }
    Ok(raw)
}
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = super::core_authority::check(app, sid, rid, generation, pending, name, args, &cancel)
        .await?;
    if shared_chat::actor(app) != app.store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private functions"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    let Some(raw) = q.name.as_deref() else {
        if name != "run_soft_ability" {
            return Err(anyhow!("Function name required"));
        }
        let items=app.store.list("core_tool:").await?.into_iter().map(|v|json!({"name":v["id"],"revision":v["revision"],"description":v["artifact"]["description"]})).take(100).collect::<Vec<_>>();
        return Ok(json!({"status":"success","output":{"functions":items}}));
    };
    let id = identifier(raw)?;
    let output = if name == "create_tool" {
        let code = q
            .code
            .as_deref()
            .ok_or_else(|| anyhow!("Function source required"))?;
        let description = q
            .description
            .as_deref()
            .filter(|v| !v.trim().is_empty() && v.len() <= 1000)
            .ok_or_else(|| anyhow!("Bounded description required"))?;
        if code.len() > 16384 {
            return Err(anyhow!("Function source exceeds bounds"));
        }
        let validation = tools::execute_trusted(
            "core_function",
            &json!({"operation":"validate","code":code,"name":id}),
            sid,
            &app.tool_socket,
            None,
            cancel.clone(),
        )
        .await?;
        if validation["status"] != "success" {
            return Ok(validation);
        }
        super::core_authority::check(app, sid, rid, generation, pending, name, args, &cancel)
            .await?;
        let prior = app.store.list("core_tool:").await?;
        if !prior.iter().any(|v| v["id"] == id) && prior.len() >= 100 {
            return Err(anyhow!("Private function quota reached"));
        }
        app.store
            .base_write_artifact(
                sid,
                rid,
                pending,
                run["generation"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Run generation missing"))?,
                "core_tool",
                id,
                q.expected_revision
                    .ok_or_else(|| anyhow!("Function revision required"))?,
                &json!({"description":description,"code":code}),
            )
            .await?
    } else {
        let definition = app
            .store
            .get(&format!("core_tool:{id}"))
            .await?
            .ok_or_else(|| anyhow!("Private function not found"))?;
        let result=tools::execute_trusted("core_function",&json!({"operation":"invoke","code":definition["artifact"]["code"],"name":id,"arguments":q.arguments.unwrap_or(json!({}))}),sid,&app.tool_socket,None,cancel.clone()).await?;
        super::core_authority::check(app, sid, rid, generation, pending, name, args, &cancel)
            .await?;
        return Ok(result);
    };
    Ok(json!({"status":"success","output":{"name":id,"revision":output["revision"],"saved":true}}))
}
