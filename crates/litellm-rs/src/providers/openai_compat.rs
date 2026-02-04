use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::http::send_json;
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

/// Default maximum polling attempts for video generation (120 * 5s = 10 minutes)
pub const DEFAULT_VIDEO_MAX_POLL_ATTEMPTS: u32 = 120;
/// Default polling interval for video generation status checks
pub const DEFAULT_VIDEO_POLL_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
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
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cost: Option<Value>,
    completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct CompletionTokensDetails {
    reasoning_tokens: Option<u64>,
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

/// Build the chat request body from a ChatRequest.
///
/// This is shared between streaming and non-streaming chat calls.
fn build_chat_body(req: &ChatRequest, stream: bool) -> Value {
    let mut body = serde_json::json!({
        "model": req.model,
        "messages": req.messages,
    });

    if stream {
        body["stream"] = serde_json::json!(true);
    }

    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(max_tokens) = req.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(ref fmt) = req.response_format {
        body["response_format"] = fmt.clone();
    }
    if let Some(max_completion_tokens) = req.max_completion_tokens {
        body["max_completion_tokens"] = serde_json::json!(max_completion_tokens);
    }
    if let Some(ref tools) = req.tools {
        body["tools"] = tools.clone();
    }
    if let Some(ref tool_choice) = req.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }
    if let Some(parallel) = req.parallel_tool_calls {
        body["parallel_tool_calls"] = serde_json::json!(parallel);
    }
    if let Some(ref stop) = req.stop {
        body["stop"] = stop.clone();
    }
    if let Some(top_p) = req.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(presence) = req.presence_penalty {
        body["presence_penalty"] = serde_json::json!(presence);
    }
    if let Some(frequency) = req.frequency_penalty {
        body["frequency_penalty"] = serde_json::json!(frequency);
    }
    if let Some(seed) = req.seed {
        body["seed"] = serde_json::json!(seed);
    }
    if let Some(ref user) = req.user {
        body["user"] = serde_json::json!(user);
    }
    if let Some(ref metadata) = req.metadata {
        body["metadata"] = metadata.clone();
    }
    if let Some(ref reasoning_effort) = req.reasoning_effort {
        body["reasoning_effort"] = reasoning_effort.clone();
    }
    if let Some(ref thinking) = req.thinking {
        body["thinking"] = thinking.clone();
    }

    body
}

pub async fn chat(client: &Client, cfg: &ProviderConfig, req: ChatRequest) -> Result<ChatResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;

    let body = build_chat_body(&req, false);

    let mut builder = client.post(url).json(&body);
    if let Some(key) = key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (parsed, headers) = send_json::<OpenAIChatResponse>(builder).await?;
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();
    let header_cost = headers
        .get("x-litellm-response-cost")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok());
    let mut usage = map_usage(parsed.usage);
    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }

    Ok(ChatResponse {
        content,
        usage,
        response_id: parsed.id,
        header_cost,
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

    let body = build_chat_body(&req, true);

    let mut builder = client.post(url).json(&body);
    if let Some(key) = key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let resp = builder.send().await.map_err(LiteLLMError::from)?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.map_err(LiteLLMError::from)?;
        return Err(LiteLLMError::http(format!(
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
    if let Some(ref size) = req.size {
        body["size"] = serde_json::json!(size);
    }
    if let Some(ref quality) = req.quality {
        body["quality"] = serde_json::json!(quality);
    }
    if let Some(ref background) = req.background {
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

/// Video generation options for configurable timeouts.
#[derive(Debug, Clone)]
pub struct VideoGenerationOptions {
    /// Maximum number of polling attempts
    pub max_poll_attempts: u32,
    /// Interval between polling attempts in seconds
    pub poll_interval_secs: u64,
}

impl Default for VideoGenerationOptions {
    fn default() -> Self {
        Self {
            max_poll_attempts: DEFAULT_VIDEO_MAX_POLL_ATTEMPTS,
            poll_interval_secs: DEFAULT_VIDEO_POLL_INTERVAL_SECS,
        }
    }
}

pub async fn video_generation(
    client: &Client,
    cfg: &ProviderConfig,
    req: VideoRequest,
) -> Result<VideoResponse> {
    video_generation_with_options(client, cfg, req, VideoGenerationOptions::default()).await
}

pub async fn video_generation_with_options(
    client: &Client,
    cfg: &ProviderConfig,
    req: VideoRequest,
    options: VideoGenerationOptions,
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
    if let Some(ref key) = key {
        builder = builder.bearer_auth(key.clone());
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
    let poll_interval = Duration::from_secs(options.poll_interval_secs);

    for attempt in 0..options.max_poll_attempts {
        let mut status_builder = client.get(&status_url);
        if let Some(ref key) = key {
            status_builder = status_builder.bearer_auth(key.clone());
        }
        let (status_resp, _headers) = send_json::<Value>(status_builder).await?;
        let status = status_resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match status {
            "completed" => {
                return fetch_video_content(client, &base, video_id, key.as_deref()).await;
            }
            "failed" => {
                let msg = status_resp
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("video generation failed");
                return Err(LiteLLMError::http(msg.to_string()));
            }
            _ => {
                if attempt + 1 >= options.max_poll_attempts {
                    return Err(LiteLLMError::http(format!(
                        "video generation timed out after {} attempts",
                        options.max_poll_attempts
                    )));
                }
                sleep(poll_interval).await;
            }
        }
    }

    Err(LiteLLMError::http("video generation timed out"))
}

async fn fetch_video_content(
    client: &Client,
    base: &str,
    video_id: &str,
    key: Option<&str>,
) -> Result<VideoResponse> {
    let content_url = format!("{}/videos/{}/content", base.trim_end_matches('/'), video_id);
    let mut content_builder = client.get(&content_url);
    if let Some(key) = key {
        content_builder = content_builder.bearer_auth(key);
    }

    let bytes = content_builder
        .send()
        .await
        .map_err(LiteLLMError::from)?
        .bytes()
        .await
        .map_err(LiteLLMError::from)?;
    let b64 = general_purpose::STANDARD.encode(bytes);

    Ok(VideoResponse {
        video_url: Some(format!("data:video/mp4;base64,{b64}")),
        raw: None,
    })
}

fn map_usage(usage: Option<OpenAIUsage>) -> Usage {
    usage.map_or_else(Usage::default, |u| Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        thoughts_tokens: u.completion_tokens_details.and_then(|d| d.reasoning_tokens),
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
