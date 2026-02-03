use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::providers::resolve_api_key;
use crate::types::{ChatRequest, ChatResponse, Usage, VideoRequest, VideoResponse};
use reqwest::Client;
use serde_json::Value;
use tokio::time::{sleep, Duration};

pub async fn chat(client: &Client, cfg: &ProviderConfig, req: ChatRequest) -> Result<ChatResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let key = resolve_api_key(cfg)?
        .ok_or_else(|| LiteLLMError::MissingApiKey("GEMINI_API_KEY".into()))?;
    let url = format!(
        "{}/models/{}:generateContent",
        base.trim_end_matches('/'),
        req.model
    );

    let contents = req
        .messages
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "parts": [{ "text": m.content }]
            })
        })
        .collect::<Vec<_>>();

    let mut body = serde_json::json!({
        "contents": contents,
    });
    if let Some(temp) = req.temperature {
        body["generationConfig"] = serde_json::json!({ "temperature": temp });
    }
    if let Some(max_tokens) = req.max_tokens {
        body["generationConfig"]
            .as_object_mut()
            .unwrap()
            .insert("maxOutputTokens".into(), serde_json::json!(max_tokens));
    }

    let mut builder = client.post(url).header("x-goog-api-key", key).json(&body);
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (resp, _headers) = send_json::<Value>(builder).await?;
    let content = extract_text(&resp);
    let usage = parse_usage(&resp);

    Ok(ChatResponse {
        content,
        usage,
        raw: None,
    })
}

pub async fn video_generation(
    client: &Client,
    cfg: &ProviderConfig,
    req: VideoRequest,
) -> Result<VideoResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let key = resolve_api_key(cfg)?
        .ok_or_else(|| LiteLLMError::MissingApiKey("GEMINI_API_KEY".into()))?;

    let url = format!(
        "{}/models/{}:predictLongRunning",
        base.trim_end_matches('/'),
        req.model
    );
    let mut parameters = serde_json::json!({});
    if let Some(seconds) = req.seconds {
        parameters["durationSeconds"] = serde_json::json!(seconds);
    }
    if let Some(size) = req.size {
        parameters["resolution"] = serde_json::json!(size);
    }

    let body = serde_json::json!({
        "instances": [{ "prompt": req.prompt }],
        "parameters": parameters,
    });

    let mut builder = client.post(url).header("x-goog-api-key", &key).json(&body);
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (resp, _headers) = send_json::<Value>(builder).await?;
    let op_name = resp
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LiteLLMError::Parse("missing operation name".into()))?;

    let op_url = if op_name.starts_with("http") {
        op_name.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), op_name)
    };

    let mut attempts = 0u32;
    let response = loop {
        let mut poll = client.get(&op_url).header("x-goog-api-key", &key);
        for (k, v) in &cfg.extra_headers {
            poll = poll.header(k, v);
        }
        let (op_resp, _headers) = send_json::<Value>(poll).await?;
        if op_resp.get("done").and_then(|v| v.as_bool()) == Some(true) {
            break op_resp;
        }
        attempts += 1;
        if attempts > 240 {
            return Err(LiteLLMError::Http("video generation timed out".into()));
        }
        sleep(Duration::from_secs(5)).await;
    };

    if response.get("error").is_some() {
        return Err(LiteLLMError::Http("video generation failed".into()));
    }

    let uri = extract_video_uri(&response)
        .ok_or_else(|| LiteLLMError::Parse("missing video uri".into()))?;

    Ok(VideoResponse {
        video_url: Some(uri),
        raw: None,
    })
}

fn extract_text(resp: &Value) -> String {
    if let Some(candidates) = resp.get("candidates").and_then(|v| v.as_array()) {
        if let Some(first) = candidates.first() {
            if let Some(parts) = first
                .get("content")
                .and_then(|v| v.get("parts"))
                .and_then(|v| v.as_array())
            {
                let mut text = String::new();
                for part in parts {
                    if let Some(piece) = part.get("text").and_then(|v| v.as_str()) {
                        text.push_str(piece);
                    }
                }
                return text;
            }
        }
    }
    String::new()
}

fn parse_usage(resp: &Value) -> Usage {
    if let Some(meta) = resp.get("usageMetadata").and_then(|v| v.as_object()) {
        return Usage {
            prompt_tokens: meta
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            completion_tokens: meta
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            total_tokens: meta
                .get("totalTokenCount")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            cost_usd: None,
        };
    }
    Usage::default()
}

fn extract_video_uri(resp: &Value) -> Option<String> {
    let response = resp.get("response")?;
    if let Some(uri) = response
        .pointer("/generateVideoResponse/generatedSamples/0/video/uri")
        .and_then(|v| v.as_str())
    {
        return Some(uri.to_string());
    }
    if let Some(uri) = response
        .pointer("/generatedVideos/0/uri")
        .and_then(|v| v.as_str())
    {
        return Some(uri.to_string());
    }
    if let Some(uri) = response.pointer("/videos/0/uri").and_then(|v| v.as_str()) {
        return Some(uri.to_string());
    }
    None
}

async fn send_json<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
) -> Result<(T, reqwest::header::HeaderMap)> {
    let resp = req
        .send()
        .await
        .map_err(|e| LiteLLMError::Http(e.to_string()))?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = resp
        .text()
        .await
        .map_err(|e| LiteLLMError::Http(e.to_string()))?;
    if !status.is_success() {
        let trimmed = text.lines().take(20).collect::<Vec<_>>().join("\n");
        return Err(LiteLLMError::Http(format!(
            "http {}: {}",
            status.as_u16(),
            trimmed
        )));
    }
    let parsed = serde_json::from_str(&text).map_err(|e| LiteLLMError::Parse(e.to_string()))?;
    Ok((parsed, headers))
}
