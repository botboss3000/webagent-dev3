//! Model discovery normalization over listings the host has already fetched with
//! the caller's authorized credential. It reads nothing itself: provider bytes,
//! public metadata catalogs and credentials stay in the host.
//! An edit here ships as a module release of the model-discovery service alone.
//! Public capability metadata is descriptive, never an entitlement or execution grant.
use serde_json::{json, Value};
use webagent_models::discovery::ModelDiscovery;
use webagent_models::provider::ProviderId;

pub struct Discovery;

impl ModelDiscovery for Discovery {
    async fn provider_models(&self, provider: ProviderId, rows: &[Value], models_dev: &Value, openrouter: &Value) -> anyhow::Result<Vec<Value>> {
        Ok(provider_models(provider, rows, models_dev, openrouter))
    }
    async fn subscription_models(&self, catalog: &Value) -> anyhow::Result<Value> {
        subscription_model_list(catalog)
    }
}

/// At most 256 unique valid model ids, in listing order, enriched from the public catalogs.
fn provider_models(provider: ProviderId, rows: &[Value], models_dev: &Value, openrouter: &Value) -> Vec<Value> {
    let mut ids = std::collections::BTreeSet::new();
    rows.iter().filter_map(|row| {
        let id = row["id"].as_str()?;
        if !webagent_models::valid_model(id) || !ids.insert(id.to_owned()) { return None; }
        Some(normalize(id, row, &lookup(id, provider, models_dev, openrouter)))
    }).take(256).collect()
}

/// Project visible subscription models in server order through the shared capability normalizer.
fn subscription_model_list(value: &Value) -> anyhow::Result<Value> {
    let models = value["models"].as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid subscription model catalog"))?;
    let visible: Vec<Value> = models.iter().filter_map(|model| {
        if model["visibility"] != "list" { return None; }
        let id = model["slug"].as_str()?;
        if id.is_empty() || id.len() > 200 || !id.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        let name = model["display_name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id);
        // Subscription chat models retain the v2 Vision fallback when upstream omits modalities.
        let mut row = model.clone();
        row["name"] = json!(name);
        let fallback = json!({"input_modalities":["text","image"],"output_modalities":["text"]});
        Some(normalize(id,&row,&fallback))
    }).collect();
    Ok(json!({"models":visible}))
}

fn modalities(row: &Value, direction: &str) -> Vec<String> {
    let key = format!("{direction}_modalities");
    let value = row.get("architecture").and_then(|a| a.get(&key))
        .or_else(|| row.get(&key)).or_else(|| row.get("modalities").and_then(|m| m.get(direction)));
    if let Some(items) = value.and_then(Value::as_array) {
        return items.iter().filter_map(Value::as_str).map(str::to_lowercase).collect();
    }
    if let Some(m) = row["architecture"]["modality"].as_str().or_else(|| row["modality"].as_str()) {
        let (input, output) = m.split_once("->").unwrap_or((m, ""));
        return (if direction == "input" { input } else { output }).split('+').map(str::to_lowercase).collect();
    }
    Vec::new()
}

fn normalize(id: &str, row: &Value, fallback: &Value) -> Value {
    let mut input = modalities(row, "input");
    let mut output = modalities(row, "output");
    // Provider metadata wins; the public catalog fills missing directions only.
    if input.is_empty() { input = modalities(fallback, "input"); }
    if output.is_empty() { output = modalities(fallback, "output"); }
    let has = |items: &[String], name: &str| items.iter().any(|m| m == name);
    let methods = row["supportedGenerationMethods"].as_array();
    let method = |name: &str| methods.is_some_and(|items| items.iter().any(|m| m.as_str() == Some(name)));
    let lower = id.to_lowercase();
    let image_hint = ["dall-e", "dalle", "gpt-image", "flux", "sdxl", "stable-diffusion", "stable-image", "sd3", "imagen", "playground", "diffus"].iter().any(|h| lower.contains(h));
    let known = !input.is_empty() || !output.is_empty() || methods.is_some();
    let name = row["display_name"].as_str().or_else(|| row["displayName"].as_str()).or_else(|| row["name"].as_str()).unwrap_or(id);
    let mut result = json!({"id":id,"name":name,"display_name":name,
        "text_capable": row["text_capable"].as_bool().unwrap_or_else(|| if !output.is_empty() { has(&output,"text") } else { !image_hint || method("generateContent") }),
        "image_capable": row["image_capable"].as_bool().unwrap_or(false) || has(&input,"image") || has(&input,"vision"),
        "image_out_capable": row["image_out_capable"].as_bool().unwrap_or(false) || has(&output,"image") || method("generateImages") || (output.is_empty() && image_hint),
        "voice_capable": row["voice_capable"].as_bool().unwrap_or(false) || has(&input,"audio") || has(&output,"audio"),
        "video_capable": row["video_capable"].as_bool().unwrap_or(false) || has(&input,"video") || has(&output,"video"),
        "modality_known":known,"capabilities_verified":false});
    for (key, live, cached) in [
        ("context", row.get("context_length").or_else(||row.get("context_window")), fallback.get("limit").and_then(|v|v.get("context"))),
        ("max_output", row.get("max_output_tokens"), fallback.get("limit").and_then(|v|v.get("output"))),
        ("description", row.get("description"), fallback.get("description")),
    ] { if let Some(v) = live.or(cached).filter(|v| v.is_number() || v.is_string()) { result[key] = v.clone(); } }
    result
}

/// The public models.dev catalog is the backbone; the OpenRouter listing fills gaps.
fn lookup(id: &str, provider: ProviderId, models_dev: &Value, openrouter: &Value) -> Value {
    let hint = match provider { ProviderId::OpenaiSubscription|ProviderId::OpenaiApi=>"openai",ProviderId::Gemini=>"google",ProviderId::Deepseek=>"deepseek",ProviderId::Mistral=>"mistral",ProviderId::Xai=>"xai",_=>"" };
    let bare = id.rsplit('/').next().unwrap_or(id);
    let direct = models_dev[hint]["models"].get(id).or_else(||models_dev[hint]["models"].get(bare));
    // Gateways use vendor prefixes; only unique bare matches are safe fallbacks.
    let mut matches = Vec::new();
    if let Some(providers) = models_dev.as_object() {
        for provider in providers.values() {
            if let Some(models) = provider["models"].as_object() {
                for (key, value) in models { if key == id || key == bare { matches.push(value); } }
            }
        }
    }
    let mut result = direct.or_else(|| if matches.len() == 1 { Some(matches[0]) } else { None }).cloned().unwrap_or(Value::Null);
    let router = openrouter["data"].as_array().and_then(|rows|rows.iter().find(|row|row["id"] == id));
    if let Some(router) = router {
        if result.is_null() { return router.clone(); }
        // Keep models.dev as the backbone, filling absent modalities from OpenRouter.
        for direction in ["input","output"] {
            if modalities(&result,direction).is_empty() {
                let mods = modalities(router,direction);
                result["modalities"][direction] = json!(mods);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn structured_vision_image_voice_video_and_method_discovery() {
        let row = normalize("multimodal",&json!({"architecture":{"input_modalities":["text","image","video"],"output_modalities":["text","image","audio"]}}),&Value::Null);
        for key in ["text_capable","image_capable","image_out_capable","voice_capable","video_capable","modality_known"] { assert_eq!(row[key],true); }
        let image = normalize("imagen-3",&json!({"supportedGenerationMethods":["generateImages"]}),&Value::Null);
        assert_eq!(image["image_out_capable"],true); assert_eq!(image["text_capable"],false);
    }
    #[test] fn enrichment_fills_gaps_without_overwriting_provider_modalities() {
        let fallback = json!({"modalities":{"input":["text","image"],"output":["text","image"]},"limit":{"context":123}});
        let row = normalize("bare",&json!({}),&fallback);
        assert_eq!(row["image_capable"],true);assert_eq!(row["context"],123);
        let live = normalize("bare",&json!({"input_modalities":["text"],"output_modalities":["text"]}),&fallback);
        assert_eq!(live["image_capable"],false);assert_eq!(live["image_out_capable"],false);
        assert_eq!(normalize("gpt-image-1",&json!({}),&Value::Null)["image_out_capable"],true);
    }
    /// Discovery deduplicates safe model identifiers and omits private listing fields.
    #[test] fn listings_keep_only_validated_unique_ids_and_public_fields() {
        let rows = [json!({"id":"vendor/model","api_key":"must-not-project"}),json!({"id":"vendor/model"}),json!({"id":"bad\nmodel"}),json!({"id":"valid"})];
        let models = provider_models(ProviderId::Openrouter,&rows,&Value::Null,&Value::Null);
        assert_eq!(models.len(),2);assert_eq!(models[0]["id"],"vendor/model");assert_eq!(models[1]["id"],"valid");
        assert!(!json!(models).to_string().contains("must-not-project"));
        let many: Vec<Value> = (0..300).map(|n| json!({"id":format!("m{n}")})).collect();
        assert_eq!(provider_models(ProviderId::Openrouter,&many,&Value::Null,&Value::Null).len(),256);
    }
    #[test] fn public_catalogs_enrich_by_provider_hint_and_unique_bare_match() {
        let (models_dev, openrouter) = (json!({"openai":{"models":{"gpt-x":{"modalities":{"input":["text","image"],"output":["text"]}}}}}),
            json!({"data":[{"id":"router/only","architecture":{"input_modalities":["audio"],"output_modalities":["text"]}}]}));
        let rows = [json!({"id":"gpt-x"}),json!({"id":"vendor/gpt-x"}),json!({"id":"router/only"})];
        let models = provider_models(ProviderId::OpenaiApi,&rows,&models_dev,&openrouter);
        assert_eq!(models[0]["image_capable"],true);assert_eq!(models[1]["image_capable"],true);
        assert_eq!(models[2]["voice_capable"],true);
    }
    /// Verify hidden rows are omitted while visible order and subscription image-input fallback survive normalization.
    #[test]
    fn subscription_catalog_only_lists_visible_models_in_server_order() {
        let catalog=json!({"models":[{"slug":"new-gpt","display_name":"New GPT","visibility":"list"},
            {"slug":"hidden-gpt","visibility":"hide"},{"slug":"older-gpt","visibility":"list"}]});
        let result = subscription_model_list(&catalog).unwrap();
        assert_eq!(result["models"][0]["id"],"new-gpt");
        assert_eq!(result["models"][1]["id"],"older-gpt");
        assert_eq!(result["models"].as_array().unwrap().len(),2);
        assert_eq!(result["models"][0]["image_capable"],true);
        assert_eq!(result["models"][0]["image_out_capable"],false);
        assert!(subscription_model_list(&json!({})).is_err());
    }
}
