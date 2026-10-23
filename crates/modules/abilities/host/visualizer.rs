//! Caller-owned mini apps render only in opaque sandbox frames; private vault data is never injected.
use crate::*;
use axum::{response::Html, Extension};
use serde::Deserialize;
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "render_visual"
            | "create_genui"
            | "edit_genui"
            | "list_genui"
            | "get_genui"
            | "delete_genui"
            | "rename_genui"
            | "get_genui_data"
            | "set_genui_data"
            | "get_genui_logs"
            | "refresh_genui"
            | "get_genui_reference"
    )
}
pub fn is_read(name: &str) -> bool {
    matches!(
        name,
        "list_genui" | "get_genui" | "get_genui_data" | "get_genui_logs" | "get_genui_reference"
    )
}
pub fn schemas() -> Vec<Value> {
    let id = json!({"type":"string","format":"uuid"});
    let rev = json!({"type":"integer","minimum":1});
    [
 ("render_visual","Save and display a private self-contained HTML/JavaScript mini app in this conversation. Inline code runs in an opaque sandbox without platform cookies, API access, forms, popups or external subresources. HTML is at most 32 KiB and optional JSON data at most 16 KiB. Use get_genui_reference for the data/theme contract.",json!({"name":{"type":"string","maxLength":120},"html":{"type":"string","maxLength":32768},"data":{}}),vec!["name","html"]),
 ("create_genui","Save a private self-contained mini app without displaying it. Use refresh_genui to display a saved app in this conversation.",json!({"name":{"type":"string","maxLength":120},"html":{"type":"string","maxLength":32768},"data":{}}),vec!["name","html"]),
 ("edit_genui","Edit one private mini app using its exact revision. Supply replacement html or exactly one old_text/new_text replacement. Displays the updated app in this conversation.",json!({"genui_id":id,"expected_revision":rev,"html":{"type":"string","maxLength":32768},"old_text":{"type":"string","maxLength":32768},"new_text":{"type":"string","maxLength":32768}}),vec!["genui_id","expected_revision"]),
 ("list_genui","List caller-private mini app names and revisions without source or data.",json!({}),vec![]),
 ("get_genui","Read one caller-private mini app's bounded HTML, data and revision.",json!({"genui_id":id}),vec!["genui_id"]),
 ("delete_genui","Delete the active private mini app using its exact revision. Conversation history and prior attachments are independent retained records.",json!({"genui_id":id,"expected_revision":rev}),vec!["genui_id","expected_revision"]),
 ("rename_genui","Rename a caller-private mini app using its exact revision.",json!({"genui_id":id,"expected_revision":rev,"name":{"type":"string","maxLength":120}}),vec!["genui_id","expected_revision","name"]),
 ("get_genui_data","Read a mini app's private JSON data snapshot. This is stored data, not a network fetch or a credential lookup.",json!({"genui_id":id}),vec!["genui_id"]),
 ("set_genui_data","Replace a mini app's private JSON data snapshot, at most 16 KiB, using its exact revision; refreshes its display in this conversation.",json!({"genui_id":id,"expected_revision":rev,"data":{}}),vec!["genui_id","expected_revision","data"]),
 ("get_genui_logs","Read bounded mini app edit activity and the last isolated browser capture's diagnostics. This does not claim continuous telemetry from a user's browser.",json!({"genui_id":id}),vec!["genui_id"]),
 ("refresh_genui","Display the current stored app and data in this conversation using its exact revision. Does not fetch external data or execute a backend script.",json!({"genui_id":id,"expected_revision":rev}),vec!["genui_id","expected_revision"]),
 ("get_genui_reference","Read the mini app HTML, data, theme and isolation contract with a small interactive example.",json!({}),vec![])
 ].into_iter().map(|(name,description,properties,required)|json!({"type":"function","name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    genui_id: Option<String>,
    expected_revision: Option<u64>,
    name: Option<String>,
    html: Option<String>,
    data: Option<Value>,
    old_text: Option<String>,
    new_text: Option<String>,
}
pub fn stable_id(value: &Value) -> String {
    let h = digest(value);
    let mut b = [0u8; 16];
    for (i, x) in b.iter_mut().enumerate() {
        *x = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap();
    }
    b[6] = (b[6] & 15) | 80;
    b[8] = (b[8] & 63) | 128;
    Uuid::from_bytes(b).to_string()
}
pub fn metadata(record: &Value) -> Value {
    json!({"genui_id":record["id"],"revision":record["revision"],"name":record["artifact"]["name"],"view_url":format!("/api/v3/core/visuals/{}/view",record["id"].as_str().unwrap_or("")),"deleted":record["artifact"]["deleted"]==true})
}
fn name(raw: Option<String>) -> Result<String> {
    raw.filter(|v| !v.trim().is_empty() && v.len() <= 120 && !v.chars().any(char::is_control))
        .ok_or_else(|| anyhow!("Mini app name must contain 1..120 bytes"))
}
fn html(raw: Option<String>) -> Result<String> {
    raw.filter(|v| !v.trim().is_empty() && v.len() <= 32768 && !v.contains('\0'))
        .ok_or_else(|| anyhow!("Self-contained HTML must contain 1..32768 bytes"))
}
fn data(value: Value) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() > 16384 {
        return Err(anyhow!("Mini app data exceeds 16 KiB"));
    }
    Ok(value)
}
pub async fn record(app: &App, id: &str) -> Result<Value> {
    Uuid::parse_str(id)?;
    app.store
        .get(&format!("core_visual:{id}"))
        .await?
        .filter(|v| v["artifact"]["deleted"] != true)
        .ok_or_else(|| anyhow!("Private mini app not found"))
}
pub fn reference() -> Value {
    json!({"format":"self-contained HTML with inline CSS and JavaScript","max_html_bytes":32768,"max_data_bytes":16384,"data":"JSON.parse(document.getElementById('core-genui-data').textContent)","theme_variables":["--wa-bg","--wa-card","--wa-text","--wa-muted","--wa-border","--wa-accent"],"isolation":"Opaque sandbox; no platform credential access, forms, popups, external scripts, network fetches, nested frames or external images. Use inline CSS/JS and data/blob images. Vault credentials are never included in the document.","example":"<style>body{background:var(--wa-bg);color:var(--wa-text)}button{padding:.7rem;touch-action:manipulation}</style><h1>Counter</h1><button id=increment type=button>Count: 0</button><script>let count=0;document.getElementById('increment').onclick=e=>{e.currentTarget.textContent='Count: '+(++count)}</script>"})
}
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    tool: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = super::core_authority::check(app, sid, rid, generation, pending, tool, args, &cancel)
        .await?;
    if shared_chat::actor(app) != app.store.current_user_id() {
        return Err(anyhow!("Use an owner-private conversation for mini apps"));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    if tool == "get_genui_reference" {
        return Ok(json!({"status":"success","output":reference()}));
    }
    if tool == "list_genui" {
        return Ok(
            json!({"status":"success","output":{"items":app.store.list("core_visual:").await?.iter().filter(|v|v["artifact"]["deleted"]!=true).map(metadata).take(100).collect::<Vec<_>>()}}),
        );
    }
    let creating = matches!(tool, "render_visual" | "create_genui");
    let id = if creating {
        stable_id(&json!([shared_chat::actor(app), sid, rid, pending]))
    } else {
        q.genui_id
            .clone()
            .ok_or_else(|| anyhow!("Mini app ID required"))?
    };
    Uuid::parse_str(&id)?;
    let signature = digest(&json!([tool, args]));
    if !is_read(tool) {
        if let Some(receipt) = app
            .store
            .get(&format!("receipt:base:{sid}:{rid}:{pending}"))
            .await?
        {
            let output = &receipt["output"];
            if output["kind"] != "core_visual"
                || output["id"] != id
                || output["artifact"]["invocation_digest"] != signature
            {
                return Err(anyhow!("Mini app invocation identity reused"));
            }
            return Ok(
                json!({"status":"success","output":metadata(&json!({"id":id,"revision":output["revision"],"artifact":output["artifact"]}))}),
            );
        }
    }
    let prior = if creating {
        None
    } else {
        Some(record(app, &id).await?)
    };
    if let Some(prior) = &prior {
        match tool {
            "get_genui" => {
                return Ok(
                    json!({"status":"success","output":{"metadata":metadata(prior),"html":prior["artifact"]["html"],"data":prior["artifact"]["data"],"source":"untrusted_user_mini_app"}}),
                )
            }
            "get_genui_data" => {
                return Ok(
                    json!({"status":"success","output":{"genui_id":id,"revision":prior["revision"],"data":prior["artifact"]["data"],"source":"private_data_snapshot"}}),
                )
            }
            "get_genui_logs" => {
                let capture = app
                    .store
                    .module_state(&format!("core_capture:{id}"))
                    .await?;
                return Ok(
                    json!({"status":"success","output":{"activity":prior["artifact"]["activity"],"last_capture":capture["state"],"authority":"untrusted_app_diagnostics"}}),
                );
            }
            _ => {}
        }
    }
    let expected = if creating {
        0
    } else {
        q.expected_revision
            .ok_or_else(|| anyhow!("Mini app revision required"))?
    };
    if prior.as_ref().is_some_and(|v| v["revision"] != expected) {
        return Err(anyhow!(
            "Mini app revision changed; read the current version before editing"
        ));
    }
    let mut artifact = if let Some(prior) = prior {
        prior["artifact"].clone()
    } else {
        if app
            .store
            .list("core_visual:")
            .await?
            .iter()
            .filter(|v| v["artifact"]["deleted"] != true)
            .count()
            >= 100
        {
            return Err(anyhow!("Mini app quota reached"));
        }
        json!({"name":name(q.name.clone())?,"html":html(q.html.clone())?,"data":data(q.data.clone().unwrap_or(json!({})))?,"activity":[],"deleted":false})
    };
    match tool {
        "render_visual" | "create_genui" => {}
        "edit_genui" => {
            let replacement = match (q.html, q.old_text, q.new_text) {
                (Some(value), None, None) => value,
                (None, Some(old), Some(new)) => {
                    if old.is_empty() {
                        return Err(anyhow!("Replacement text cannot be empty"));
                    }
                    let source = artifact["html"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Mini app source unavailable"))?;
                    if source.matches(&old).count() != 1 {
                        return Err(anyhow!("Edit must match exactly one source span"));
                    }
                    source.replacen(&old, &new, 1)
                }
                _ => {
                    return Err(anyhow!(
                        "Supply replacement HTML or one old_text/new_text pair"
                    ))
                }
            };
            artifact["html"] = json!(html(Some(replacement))?);
        }
        "rename_genui" => artifact["name"] = json!(name(q.name)?),
        "set_genui_data" => {
            artifact["data"] = data(q.data.ok_or_else(|| anyhow!("Data required"))?)?
        }
        "refresh_genui" => {}
        "delete_genui" => artifact = json!({"name":artifact["name"],"deleted":true,"activity":[]}),
        _ => return Err(anyhow!("Unknown mini app mutation")),
    }
    artifact["publish"] = json!(tool != "create_genui");
    artifact["invocation_digest"] = json!(signature);
    let activity = artifact["activity"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("Invalid mini app activity"))?;
    activity.push(json!({"operation":tool,"revision":expected+1,"run_id":rid}));
    if activity.len() > 20 {
        activity.remove(0);
    }
    super::core_authority::check(app, sid, rid, generation, pending, tool, args, &cancel).await?;
    let output = app
        .store
        .base_write_artifact(
            sid,
            rid,
            pending,
            run["generation"]
                .as_str()
                .ok_or_else(|| anyhow!("Run generation missing"))?,
            "core_visual",
            &id,
            expected,
            &artifact,
        )
        .await?;
    Ok(
        json!({"status":"success","output":metadata(&json!({"id":id,"revision":output["revision"],"artifact":artifact}))}),
    )
}
fn escaped_json(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(value)?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}
pub fn document(record: &Value, theme: &str) -> Result<String> {
    let html = record["artifact"]["html"]
        .as_str()
        .ok_or_else(|| anyhow!("Mini app source missing"))?;
    if html.len() > 32768 {
        return Err(anyhow!("Mini app source exceeds bounds"));
    }
    let source = escaped_json(&json!(html))?;
    let portable_csp = CSP.split("; frame-ancestors").next().unwrap();
    let data = escaped_json(&record["artifact"]["data"])?;
    let light = theme == "light";
    let style = if light {
        "--wa-bg:#f8fafc;--wa-card:#fff;--wa-text:#172033;--wa-muted:#64748b;--wa-border:#cbd5e1;--wa-accent:#2563eb;color-scheme:light"
    } else {
        "--wa-bg:#10121a;--wa-card:#191d2a;--wa-text:#e5e7eb;--wa-muted:#94a3b8;--wa-border:#334155;--wa-accent:#7aa2f7;color-scheme:dark"
    };
    Ok(format!("<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{portable_csp}\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><style>:root{{{style}}}body{{background:var(--wa-bg);color:var(--wa-text);font:14px system-ui,sans-serif;margin:16px}}button,[role=button]{{touch-action:manipulation;-webkit-tap-highlight-color:transparent}}button:focus-visible,[role=button]:focus-visible{{outline:2px solid var(--wa-accent);outline-offset:2px}}</style><script type=\"application/json\" id=\"core-genui-data\">{data}</script><script>addEventListener('message',event=>{{if(event.source!==parent||event.data?.type!=='webagent:visual-theme')return;for(const [key,value]of Object.entries(event.data.colors||{{}}))if(['bg','card','text','muted','border','accent'].includes(key)&&typeof value==='string'&&CSS.supports('color',value))document.documentElement.style.setProperty('--wa-'+key,value);if(['light','dark'].includes(event.data.theme))document.documentElement.style.colorScheme=event.data.theme;}});</script><script type=\"application/json\" id=\"core-genui-source\">{source}</script><script>for(const name of ['RTCPeerConnection','webkitRTCPeerConnection','WebTransport','Worker','SharedWorker']){{Object.defineProperty(globalThis,name,{{value:undefined,writable:false,configurable:false}});}}document.write(JSON.parse(document.getElementById('core-genui-source').textContent));</script>"))
}
const CSP:&str="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; media-src data: blob:; font-src data:; connect-src 'none'; object-src 'none'; frame-src 'none'; worker-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; sandbox allow-scripts";
pub async fn view(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let owner = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Caller required"))?;
    let mut caller = app.clone();
    caller.store = app.store.for_user(owner).await?;
    let record = record(&caller, &id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Mini app not found".into()))?;
    let body = document(
        &record,
        query.get("theme").map(String::as_str).unwrap_or("dark"),
    )?;
    let mut response = Html(body).into_response();
    response
        .headers_mut()
        .insert("x-dns-prefetch-control", HeaderValue::from_static("off"));
    response
        .headers_mut()
        .insert("content-security-policy", HeaderValue::from_static(CSP));
    response.headers_mut().insert("permissions-policy",HeaderValue::from_static("camera=(),microphone=(),geolocation=(),display-capture=(),clipboard-read=(),clipboard-write=(),payment=(),usb=()"));
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_data_cannot_close_its_json_element_or_grant_same_origin() {
        let record = json!({"artifact":{"html":"<h1>Mini app</h1>","data":{"value":"</script><script>bad()</script>"}}});
        let body = document(&record, "dark").unwrap();
        assert!(body.contains("\\u003c/script\\u003e"));
        assert!(!body.contains("<script>bad()"));
        let source = body
            .split("id=\"core-genui-source\">")
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(source).unwrap(),
            record["artifact"]["html"]
        );
        assert!(CSP.contains("sandbox allow-scripts"));
        assert!(!CSP.contains("allow-same-origin"));
        assert!(CSP.contains("connect-src 'none'"));
    }
    #[tokio::test]
    async fn mini_app_views_cannot_read_another_users_record() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let a = app
            .store
            .create_user_account("visual-first", "fixture-password-hash-placeholder")
            .await
            .unwrap();
        let b = app
            .store
            .create_user_account("visual-second", "fixture-password-hash-placeholder")
            .await
            .unwrap();
        let id = uuid();
        let store = app
            .store
            .for_user(a["user_id"].as_str().unwrap())
            .await
            .unwrap();
        store
            .put(
                &format!("core_visual:{id}"),
                &json!({"id":id,"revision":1,"artifact":{"html":"<h1>Private</h1>","data":{}}}),
            )
            .await
            .unwrap();
        assert!(view(
            State(app.clone()),
            Extension(json!({"_user_id":a["user_id"]})),
            Path(id.clone()),
            Query(HashMap::new())
        )
        .await
        .is_ok());
        assert!(view(
            State(app),
            Extension(json!({"_user_id":b["user_id"]})),
            Path(id),
            Query(HashMap::new())
        )
        .await
        .is_err());
    }
}
