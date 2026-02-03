use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::providers::resolve_api_key;
use crate::stream::{parse_sse_stream, ChatStream};
use crate::types::{
    ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse, ImageData, ImageRequest,
    ImageResponse, Usage, VideoRequest, VideoResponse,
};
use base64::{engine::general_purpose, Engine as _};
use reqwest::multipart::Form;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    #[allow(dead_code)]
    id: Option<String>,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
    cost: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct OpenAIEmbeddingResponse {
    data: Vec<OpenAIEmbeddingItem>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIEmbeddingItem {
    embedding: Vec<f32>,
}

pub async fn chat(client: &Client, cfg: &ProviderConfig, req: ChatRequest) -> Result<ChatResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": req.messages,
    });
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(max_tokens) = req.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(fmt) = req.response_format {
        body["response_format"] = fmt;
    }

    let mut builder = client.post(url).json(&body);
    if let Some(key) = key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (parsed, _headers) = send_json::<OpenAIChatResponse>(builder).await?;
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    Ok(ChatResponse {
        content,
        usage: map_usage(parsed.usage),
        raw: None,
    })
}

pub async fn chat_stream(
    client: &Client,
    cfg: &ProviderConfig,
    req: ChatRequest,
) -> Result<ChatStream> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": req.messages,
        "stream": true,
    });
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(max_tokens) = req.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(fmt) = req.response_format {
        body["response_format"] = fmt;
    }

    let mut builder = client.post(url).json(&body);
    if let Some(key) = key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let resp = builder
        .send()
        .await
        .map_err(|e| LiteLLMError::Http(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp
            .text()
            .await
            .map_err(|e| LiteLLMError::Http(e.to_string()))?;
        return Err(LiteLLMError::Http(format!(
            "http {}: {}",
            status.as_u16(),
            text
        )));
    }

    Ok(parse_sse_stream(resp.bytes_stream()))
}

pub async fn embeddings(
    client: &Client,
    cfg: &ProviderConfig,
    req: EmbeddingRequest,
) -> Result<EmbeddingResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/embeddings", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;

    let body = serde_json::json!({
        "model": req.model,
        "input": req.input,
    });

    let mut builder = client.post(url).json(&body);
    if let Some(key) = key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (parsed, _headers) = send_json::<OpenAIEmbeddingResponse>(builder).await?;
    let vectors = parsed.data.into_iter().map(|d| d.embedding).collect();

    Ok(EmbeddingResponse {
        vectors,
        usage: map_usage(parsed.usage),
        raw: None,
    })
}

pub async fn image_generation(
    client: &Client,
    cfg: &ProviderConfig,
    req: ImageRequest,
) -> Result<ImageResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/images/generations", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;

    let mut body = serde_json::json!({
        "model": req.model,
        "prompt": req.prompt,
    });
    if let Some(n) = req.n {
        body["n"] = serde_json::json!(n);
    }
    if let Some(size) = req.size {
        body["size"] = serde_json::json!(size);
    }
    if let Some(quality) = req.quality {
        body["quality"] = serde_json::json!(quality);
    }
    if let Some(background) = req.background {
        body["background"] = serde_json::json!(background);
    }

    let mut builder = client.post(url).json(&body);
    if let Some(key) = key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (parsed, _headers) = send_json::<Value>(builder).await?;
    let images = parsed
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| ImageData {
                    b64_json: item
                        .get("b64_json")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    url: item
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    revised_prompt: item
                        .get("revised_prompt")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(ImageResponse {
        images,
        usage: Usage::default(),
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
    let url = format!("{}/videos", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;

    let mut form = Form::new()
        .text("model", req.model)
        .text("prompt", req.prompt);
    if let Some(seconds) = req.seconds {
        form = form.text("seconds", seconds.to_string());
    }
    if let Some(size) = req.size {
        form = form.text("size", size);
    }

    let mut builder = client.post(url).multipart(form);
    if let Some(key) = key.clone() {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (parsed, _headers) = send_json::<Value>(builder).await?;
    let video_id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LiteLLMError::Parse("missing video id".into()))?;

    let status_url = format!("{}/videos/{}", base.trim_end_matches('/'), video_id);
    let mut attempts = 0u32;
    loop {
        let mut status_builder = client.get(&status_url);
        if let Some(key) = key.clone() {
            status_builder = status_builder.bearer_auth(key);
        }
        let (status_resp, _headers) = send_json::<Value>(status_builder).await?;
        let status = status_resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        match status {
            "completed" => break,
            "failed" => {
                let msg = status_resp
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("video generation failed");
                return Err(LiteLLMError::Http(msg.to_string()));
            }
            _ => {
                attempts += 1;
                if attempts > 120 {
                    return Err(LiteLLMError::Http("video generation timed out".into()));
                }
                sleep(Duration::from_secs(5)).await;
            }
        }
    }

    let content_url = format!("{}/videos/{}/content", base.trim_end_matches('/'), video_id);
    let mut content_builder = client.get(&content_url);
    if let Some(key) = key {
        content_builder = content_builder.bearer_auth(key);
    }

    let bytes = content_builder
        .send()
        .await
        .map_err(|e| LiteLLMError::Http(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| LiteLLMError::Http(e.to_string()))?;
    let b64 = general_purpose::STANDARD.encode(bytes);

    Ok(VideoResponse {
        video_url: Some(format!("data:video/mp4;base64,{b64}")),
        raw: None,
    })
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

fn map_usage(usage: Option<OpenAIUsage>) -> Usage {
    usage.map_or_else(Usage::default, |u| Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
        cost_usd: parse_cost(u.cost.as_ref()),
    })
}

fn parse_cost(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}
