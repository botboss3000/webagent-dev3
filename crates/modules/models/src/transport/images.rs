//! Fixed image-output transports. Provider-selected remote URLs are never fetched.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
pub struct GeneratedImage {
    pub media_type: String,
    pub data: String,
}
pub fn image_transport(provider: ProviderId) -> bool {
    matches!(
        provider,
        ProviderId::OpenaiSubscription
            | ProviderId::OpenaiApi
            | ProviderId::Openrouter
            | ProviderId::Gemini
    )
}
fn image(data: &str, declared: Option<&str>) -> Result<GeneratedImage> {
    if data.len() > 24 * 1024 * 1024 {
        bail!("Generated image exceeded response limit");
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| anyhow!("Invalid generated image encoding"))?;
    let media = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[255, 216, 255]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        bail!("Provider returned unsupported image bytes");
    };
    if declared.is_some_and(|v| v != media) {
        bail!("Generated image type does not match its bytes");
    }
    Ok(GeneratedImage {
        media_type: media.into(),
        data: STANDARD.encode(bytes),
    })
}
pub fn image_output(provider: ProviderId, value: &Value) -> Result<GeneratedImage> {
    match provider {
        ProviderId::OpenaiApi | ProviderId::Openrouter => image(
            value["data"][0]["b64_json"]
                .as_str()
                .ok_or_else(|| anyhow!("Image output missing"))?,
            value["data"][0]["media_type"].as_str(),
        ),
        ProviderId::Gemini => {
            let part = value["candidates"][0]["content"]["parts"]
                .as_array()
                .and_then(|parts| parts.iter().find(|v| v["inlineData"].is_object()))
                .ok_or_else(|| anyhow!("Image output missing"))?;
            image(
                part["inlineData"]["data"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Image output missing"))?,
                part["inlineData"]["mimeType"].as_str(),
            )
        }
        ProviderId::OpenaiSubscription => {
            let item = value["output"]
                .as_array()
                .and_then(|items| items.iter().find(|v| v["type"] == "image_generation_call"))
                .ok_or_else(|| anyhow!("Subscription returned no generated image"))?;
            image(
                item["result"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Image output missing"))?,
                None,
            )
        }
        _ => bail!("Assigned Image Out transport does not support image generation"),
    }
}
impl Provider {
    pub async fn generate_image(
        &self,
        selection: &ProviderSelection,
        tokens: &Value,
        prompt: &str,
        sid: &str,
        cancel: CancellationToken,
    ) -> Result<GeneratedImage> {
        if prompt.trim().is_empty() || prompt.len() > 16384 || !image_transport(selection.provider)
        {
            bail!("Invalid image request or unsupported assigned Image Out transport");
        }
        let value = if selection.provider == ProviderId::OpenaiSubscription {
            let request = call::SubscriptionRequest {
                max_output_tokens: selection.limits.as_ref().map(|v| v.output_tokens),
                reasoning_effort: None,
                model: &selection.model,
                input: vec![json!({"role":"user","content":[{"type":"input_text","text":prompt}]})],
                tools: vec![json!({"type":"image_generation"})],
                session: sid,
                instructions: "Generate the image requested by the user.",
            };
            let credentials = call::SubscriptionCredentials {
                access_token: tokens["access_token"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow!("Image connection unavailable"))?,
                account_id: tokens["account_id"].as_str(),
                residency: tokens["residency"].as_str(),
            };
            let (tx, mut rx) = mpsc::channel(16);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let result = self
                .send_subscription(request, credentials, tx, cancel.clone())
                .await;
            drain.abort();
            result?
        } else {
            let api_key = tokens["api_key"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("Image connection unavailable"))?;
            let (endpoint, body) = match selection.provider {
                ProviderId::OpenaiApi => {
                    let mut body = json!({"model":selection.model,"prompt":prompt,"n":1});
                    if !selection.model.starts_with("gpt-image") {
                        body["response_format"] = json!("b64_json");
                    }
                    (
                        "https://api.openai.com/v1/images/generations".to_owned(),
                        body,
                    )
                }
                ProviderId::Openrouter => (
                    "https://openrouter.ai/api/v1/images".to_owned(),
                    json!({"model":selection.model,"prompt":prompt,"n":1,"output_format":"png"}),
                ),
                ProviderId::Gemini => {
                    let mut url = reqwest::Url::parse(
                        "https://generativelanguage.googleapis.com/v1beta/models/",
                    )?;
                    url.path_segments_mut()
                        .map_err(|_| anyhow!("Invalid image route"))?
                        .pop_if_empty()
                        .push(&format!("{}:generateContent", selection.model));
                    (
                        url.to_string(),
                        json!({"contents":[{"parts":[{"text":prompt}]}],"generationConfig":{"responseModalities":["TEXT","IMAGE"]}}),
                    )
                }
                _ => bail!("Unsupported image transport"),
            };
            #[cfg(not(any(test, feature = "test-support")))]
            let endpoint = endpoint.as_str();
            #[cfg(any(test, feature = "test-support"))]
            let endpoint = self.responses_endpoint.as_deref().unwrap_or(&endpoint);
            let request = self.client.post(endpoint).json(&body);
            let request = if selection.provider == ProviderId::Gemini {
                request.header("x-goog-api-key", api_key)
            } else {
                request.bearer_auth(api_key)
            };
            let response = tokio::select! {biased;_=cancel.cancelled()=>bail!("Image request cancelled"),
            response=tokio::time::timeout(Duration::from_secs(180),request.send())=>response.map_err(|_|anyhow!("Image request timed out"))?.map_err(|_|anyhow!("Image connection failed"))?};
            if !response.status().is_success() {
                return Err(response_failure(response, &cancel).await);
            }
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            loop {
                let chunk = tokio::select! {biased;_=cancel.cancelled()=>bail!("Image request cancelled"),
                chunk=tokio::time::timeout(Duration::from_secs(30),stream.next())=>chunk.map_err(|_|anyhow!("Image response timed out"))?};
                let Some(chunk) = chunk else { break };
                let chunk = chunk.map_err(|_| anyhow!("Image response interrupted"))?;
                if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                    bail!("Image response exceeded limit");
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("Invalid image response"))?
        };
        image_output(selection.provider, &value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_is_inline_typed_and_bounded() {
        let png = STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture");
        let value = json!({"data":[{"b64_json":png}]});
        assert_eq!(
            image_output(ProviderId::OpenaiApi, &value)
                .unwrap()
                .media_type,
            "image/png"
        );
        let remote = json!({"choices":[{"message":{"images":[{"image_url":{"url":"https://example.invalid/image.png"}}]}}]});
        assert!(image_output(ProviderId::Openrouter, &remote).is_err());
        assert!(image("aGVsbG8=", None).is_err());
        assert!(image(&png, Some("image/jpeg")).is_err());
        assert!(!image_transport(ProviderId::Deepseek));
    }
}
