use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::http::send_json;
use crate::providers::resolve_api_key;
use crate::types::{
    ChatContentPart, ChatContentPartFile, ChatContentPartImageUrl, ChatContentPartInputAudio,
    ChatContentPartText, ChatFile, ChatImageUrl, ChatInputAudio, ChatMessage, ChatMessageContent,
    ChatRequest, ChatResponse, Usage, VideoRequest, VideoResponse,
};
use base64::{engine::general_purpose, Engine as _};
use mime_guess::MimeGuess;
use reqwest::header::CONTENT_TYPE;
use reqwest::Client;
use serde_json::Value;
use std::env;
use tokio::time::{sleep, Duration};

/// Default maximum polling attempts for video generation (240 * 5s = 20 minutes)
pub const DEFAULT_VIDEO_MAX_POLL_ATTEMPTS: u32 = 240;
/// Default polling interval for video generation status checks
pub const DEFAULT_VIDEO_POLL_INTERVAL_SECS: u64 = 5;

pub async fn chat(client: &Client, cfg: &ProviderConfig, req: ChatRequest) -> Result<ChatResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let key = resolve_api_key(cfg)?
        .ok_or_else(|| LiteLLMError::MissingApiKey("GEMINI_API_KEY".into()))?;
    let model = req.model.trim_start_matches("models/");
    let url = format!(
        "{}/models/{}:generateContent",
        base.trim_end_matches('/'),
        model
    );

    let mut messages = req.messages;
    let system_instruction = extract_system_instruction(&mut messages)?;
    if messages.is_empty() && !system_instruction.is_empty() {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatMessageContent::Text(" ".to_string()),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            function_call: None,
            provider_specific_fields: None,
        });
    }

    let contents = gemini_contents_from_messages(client, messages, model).await?;

    let mut body = serde_json::json!({ "contents": contents });
    if !system_instruction.is_empty() {
        body["system_instruction"] = serde_json::json!({ "parts": system_instruction });
    }

    // Build generationConfig safely without unwrap
    let mut generation_config = serde_json::Map::new();
    if let Some(temp) = req.temperature {
        generation_config.insert("temperature".to_string(), serde_json::json!(temp));
    }
    if let Some(max_tokens) = req.max_tokens {
        generation_config.insert("maxOutputTokens".to_string(), serde_json::json!(max_tokens));
    }
    if !generation_config.is_empty() {
        body["generationConfig"] = Value::Object(generation_config);
    }

    let mut builder = client.post(url).header("x-goog-api-key", key).json(&body);
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (resp, _headers) = send_json::<Value>(builder).await?;
    let content = extract_text(&resp);
    let usage = parse_usage(&resp);
    let debug = env::var("LITELLM_GEMINI_DEBUG").ok().as_deref() == Some("1")
        || env::var("YOETZ_GEMINI_DEBUG").ok().as_deref() == Some("1");
    if debug {
        if let Ok(pretty) = serde_json::to_string_pretty(&resp) {
            eprintln!("litellm-rs gemini raw response:\n{pretty}");
        }
    }

    Ok(ChatResponse {
        content,
        usage,
        response_id: None,
        header_cost: None,
        raw: if debug { Some(resp) } else { None },
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
    let key = resolve_api_key(cfg)?
        .ok_or_else(|| LiteLLMError::MissingApiKey("GEMINI_API_KEY".into()))?;
    let model = req.model.trim_start_matches("models/");

    let url = format!(
        "{}/models/{}:predictLongRunning",
        base.trim_end_matches('/'),
        model
    );

    let mut parameters = serde_json::Map::new();
    if let Some(seconds) = req.seconds {
        parameters.insert("durationSeconds".to_string(), serde_json::json!(seconds));
    }
    if let Some(size) = req.size {
        parameters.insert("resolution".to_string(), serde_json::json!(size));
    }

    let body = serde_json::json!({
        "instances": [{ "prompt": req.prompt }],
        "parameters": Value::Object(parameters),
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

    let poll_interval = Duration::from_secs(options.poll_interval_secs);

    for attempt in 0..options.max_poll_attempts {
        let mut poll = client.get(&op_url).header("x-goog-api-key", &key);
        for (k, v) in &cfg.extra_headers {
            poll = poll.header(k, v);
        }
        let (op_resp, _headers) = send_json::<Value>(poll).await?;

        if op_resp.get("done").and_then(|v| v.as_bool()) == Some(true) {
            if op_resp.get("error").is_some() {
                return Err(LiteLLMError::http("video generation failed"));
            }

            let uri = extract_video_uri(&op_resp)
                .ok_or_else(|| LiteLLMError::Parse("missing video uri".into()))?;

            return Ok(VideoResponse {
                video_url: Some(uri),
                raw: None,
            });
        }

        if attempt + 1 >= options.max_poll_attempts {
            return Err(LiteLLMError::http(format!(
                "video generation timed out after {} attempts",
                options.max_poll_attempts
            )));
        }
        sleep(poll_interval).await;
    }

    Err(LiteLLMError::http("video generation timed out"))
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
            prompt_tokens: meta.get("promptTokenCount").and_then(|v| v.as_u64()),
            completion_tokens: meta.get("candidatesTokenCount").and_then(|v| v.as_u64()),
            thoughts_tokens: meta.get("thoughtsTokenCount").and_then(|v| v.as_u64()),
            total_tokens: meta.get("totalTokenCount").and_then(|v| v.as_u64()),
            cost_usd: None,
        };
    }
    Usage::default()
}

fn extract_system_instruction(messages: &mut Vec<ChatMessage>) -> Result<Vec<Value>> {
    let mut parts = Vec::new();
    let mut indices = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "system" {
            match &msg.content {
                ChatMessageContent::Text(text) => {
                    if !text.is_empty() {
                        parts.push(serde_json::json!({ "text": text }));
                    }
                }
                ChatMessageContent::Parts(content_parts) => {
                    let mut system_text = String::new();
                    for part in content_parts {
                        if let ChatContentPart::Text(ChatContentPartText { text, .. }) = part {
                            system_text.push_str(text);
                        }
                    }
                    if !system_text.is_empty() {
                        parts.push(serde_json::json!({ "text": system_text }));
                    }
                }
            }
            indices.push(idx);
        }
    }
    for idx in indices.into_iter().rev() {
        messages.remove(idx);
    }
    Ok(parts)
}

async fn gemini_contents_from_messages(
    client: &Client,
    messages: Vec<ChatMessage>,
    model: &str,
) -> Result<Vec<Value>> {
    let mut contents: Vec<Value> = Vec::new();
    let mut msg_i = 0;
    let mut tool_call_responses: Vec<Value> = Vec::new();
    let mut last_tool_calls: Vec<ToolCallInfo> = Vec::new();

    while msg_i < messages.len() {
        let role = messages[msg_i].role.as_str();
        if role == "user" {
            let mut parts: Vec<Value> = Vec::new();
            while msg_i < messages.len() && messages[msg_i].role == "user" {
                parts.extend(
                    gemini_parts_from_content(client, &messages[msg_i].content, model).await?,
                );
                msg_i += 1;
            }
            if !parts.is_empty() {
                contents.push(serde_json::json!({ "role": "user", "parts": parts }));
            }
            continue;
        }

        if role == "assistant" {
            let mut parts: Vec<Value> = Vec::new();
            while msg_i < messages.len() && messages[msg_i].role == "assistant" {
                let message = &messages[msg_i];
                parts.extend(gemini_parts_from_content(client, &message.content, model).await?);
                let (tool_parts, tool_infos) = tool_call_parts_from_message(message)?;
                if !tool_parts.is_empty() {
                    parts.extend(tool_parts);
                    if !tool_infos.is_empty() {
                        last_tool_calls = tool_infos;
                    }
                }
                msg_i += 1;
            }
            if !parts.is_empty() {
                contents.push(serde_json::json!({ "role": "model", "parts": parts }));
            }
            continue;
        }

        if role == "tool" || role == "function" {
            while msg_i < messages.len()
                && (messages[msg_i].role == "tool" || messages[msg_i].role == "function")
            {
                let response_parts =
                    tool_response_parts(client, &messages[msg_i], &last_tool_calls).await?;
                tool_call_responses.extend(response_parts);
                msg_i += 1;
            }
            if !tool_call_responses.is_empty() {
                contents.push(serde_json::json!({ "parts": tool_call_responses }));
                tool_call_responses = Vec::new();
            }
            continue;
        }

        let parts = gemini_parts_from_content(client, &messages[msg_i].content, model).await?;
        if !parts.is_empty() {
            contents.push(serde_json::json!({ "role": "user", "parts": parts }));
        }
        msg_i += 1;
    }

    if !tool_call_responses.is_empty() {
        contents.push(serde_json::json!({ "parts": tool_call_responses }));
    }
    if contents.is_empty() {
        contents.push(serde_json::json!({
            "role": "user",
            "parts": [{ "text": " " }]
        }));
    }
    Ok(contents)
}

async fn gemini_parts_from_content(
    _client: &Client,
    content: &ChatMessageContent,
    model: &str,
) -> Result<Vec<Value>> {
    match content {
        ChatMessageContent::Text(text) => Ok(vec![serde_json::json!({ "text": text })]),
        ChatMessageContent::Parts(parts) => {
            let mut out = Vec::new();
            for part in parts {
                match part {
                    ChatContentPart::Text(ChatContentPartText { text, .. }) => {
                        if !text.is_empty() {
                            out.push(serde_json::json!({ "text": text }));
                        }
                    }
                    ChatContentPart::ImageUrl(ChatContentPartImageUrl { image_url, .. }) => {
                        let detail = match image_url {
                            ChatImageUrl::Object(obj) => obj.detail.as_deref(),
                            ChatImageUrl::Url(_) => None,
                        };
                        out.push(process_gemini_media(image_url, detail, None, None, model)?);
                    }
                    ChatContentPart::InputAudio(ChatContentPartInputAudio {
                        input_audio, ..
                    }) => {
                        out.push(process_gemini_audio(input_audio)?);
                    }
                    ChatContentPart::File(ChatContentPartFile { file, .. }) => {
                        out.push(process_gemini_file(file, model)?);
                    }
                    ChatContentPart::Other(value) => {
                        return Err(LiteLLMError::Config(format!(
                            "unsupported gemini content part: {}",
                            value
                        )));
                    }
                }
            }
            Ok(out)
        }
    }
}

#[derive(Debug, Clone)]
struct ToolCallInfo {
    id: Option<String>,
    name: String,
}

fn tool_call_parts_from_message(message: &ChatMessage) -> Result<(Vec<Value>, Vec<ToolCallInfo>)> {
    let mut parts = Vec::new();
    let mut infos = Vec::new();

    if let Some(tool_calls) = &message.tool_calls {
        if let Some(array) = tool_calls.as_array() {
            for tool in array {
                let function = tool
                    .get("function")
                    .ok_or_else(|| LiteLLMError::Config("tool_call missing function".into()))?;
                let name = function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| LiteLLMError::Config("tool_call missing name".into()))?;
                let args_raw = function
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = if args_raw.trim().is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(args_raw)
                        .map_err(|e| LiteLLMError::Parse(e.to_string()))?
                };
                parts.push(serde_json::json!({
                    "function_call": { "name": name, "args": args }
                }));
                let id = tool
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                infos.push(ToolCallInfo {
                    id,
                    name: name.to_string(),
                });
            }
            return Ok((parts, infos));
        }
    }

    if let Some(function_call) = &message.function_call {
        let name = function_call
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LiteLLMError::Config("function_call missing name".into()))?;
        let args_raw = function_call
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = if args_raw.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(args_raw).map_err(|e| LiteLLMError::Parse(e.to_string()))?
        };
        parts.push(serde_json::json!({
            "function_call": { "name": name, "args": args }
        }));
        infos.push(ToolCallInfo {
            id: None,
            name: name.to_string(),
        });
    }

    Ok((parts, infos))
}

async fn tool_response_parts(
    client: &Client,
    message: &ChatMessage,
    last_tool_calls: &[ToolCallInfo],
) -> Result<Vec<Value>> {
    let name = resolve_tool_name(message, last_tool_calls)?;
    let (content_str, mut inline_parts) = extract_tool_content(client, &message.content).await?;
    let response = parse_tool_response_data(&content_str);
    let function_part = serde_json::json!({
        "function_response": { "name": name, "response": response }
    });
    let mut parts = vec![function_part];
    parts.append(&mut inline_parts);
    Ok(parts)
}

fn resolve_tool_name(message: &ChatMessage, last_tool_calls: &[ToolCallInfo]) -> Result<String> {
    if let Some(name) = &message.name {
        return Ok(name.clone());
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        if let Some(info) = last_tool_calls
            .iter()
            .find(|info| info.id.as_deref() == Some(tool_call_id.as_str()))
        {
            return Ok(info.name.clone());
        }
    }
    Err(LiteLLMError::Config(
        "missing tool name for tool response".into(),
    ))
}

async fn extract_tool_content(
    client: &Client,
    content: &ChatMessageContent,
) -> Result<(String, Vec<Value>)> {
    let mut text = String::new();
    let mut inline_parts = Vec::new();
    match content {
        ChatMessageContent::Text(t) => {
            text.push_str(t);
        }
        ChatMessageContent::Parts(parts) => {
            for part in parts {
                match part {
                    ChatContentPart::Text(ChatContentPartText { text: t, .. }) => {
                        text.push_str(t);
                    }
                    ChatContentPart::ImageUrl(ChatContentPartImageUrl { image_url, .. }) => {
                        inline_parts.push(inline_data_from_image_url(client, image_url).await?);
                    }
                    ChatContentPart::InputAudio(ChatContentPartInputAudio {
                        input_audio, ..
                    }) => {
                        inline_parts.push(inline_data_from_audio(input_audio)?);
                    }
                    ChatContentPart::File(ChatContentPartFile { file, .. }) => {
                        inline_parts.push(inline_data_from_file(client, file).await?);
                    }
                    ChatContentPart::Other(_) => {}
                }
            }
        }
    }
    Ok((text, inline_parts))
}

fn parse_tool_response_data(content: &str) -> Value {
    let trimmed = content.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str(trimmed) {
            return value;
        }
    }
    serde_json::json!({ "content": content })
}

async fn inline_data_from_image_url(client: &Client, image_url: &ChatImageUrl) -> Result<Value> {
    let (url, format) = match image_url {
        ChatImageUrl::Url(url) => (url.as_str(), None),
        ChatImageUrl::Object(obj) => (obj.url.as_str(), obj.format.as_deref()),
    };
    inline_data_from_url(client, url, format).await
}

fn inline_data_from_audio(input_audio: &ChatInputAudio) -> Result<Value> {
    let format = if input_audio.format.starts_with("audio/") {
        input_audio.format.clone()
    } else {
        format!("audio/{}", input_audio.format)
    };
    Ok(serde_json::json!({
        "inline_data": { "mime_type": format, "data": input_audio.data }
    }))
}

async fn inline_data_from_file(client: &Client, file: &ChatFile) -> Result<Value> {
    let passed = file
        .file_id
        .as_ref()
        .or(file.file_data.as_ref())
        .ok_or_else(|| LiteLLMError::Config("file_id or file_data required".into()))?;
    inline_data_from_url(client, passed, file.format.as_deref()).await
}

async fn inline_data_from_url(client: &Client, url: &str, format: Option<&str>) -> Result<Value> {
    if let Some((mime, data)) = parse_data_url(url, format) {
        return Ok(serde_json::json!({
            "inline_data": { "mime_type": mime, "data": data }
        }));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let (mime, data) = fetch_bytes_with_mime(client, url, format).await?;
        return Ok(serde_json::json!({
            "inline_data": { "mime_type": mime, "data": data }
        }));
    }
    Err(LiteLLMError::Config("unsupported inline data url".into()))
}

async fn fetch_bytes_with_mime(
    client: &Client,
    url: &str,
    format: Option<&str>,
) -> Result<(String, String)> {
    let resp = client.get(url).send().await.map_err(LiteLLMError::from)?;
    let headers = resp.headers().clone();
    let bytes = resp.bytes().await.map_err(LiteLLMError::from)?;
    let header_mime = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());
    let mime = format
        .map(|v| v.to_string())
        .or(header_mime)
        .or_else(|| mime_type_from_url(url))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let data = general_purpose::STANDARD.encode(bytes);
    Ok((mime, data))
}

fn process_gemini_audio(input_audio: &ChatInputAudio) -> Result<Value> {
    let format = if input_audio.format.starts_with("audio/") {
        input_audio.format.clone()
    } else {
        format!("audio/{}", input_audio.format)
    };
    Ok(serde_json::json!({
        "inline_data": {
            "mime_type": format,
            "data": input_audio.data
        }
    }))
}

fn process_gemini_file(file: &ChatFile, model: &str) -> Result<Value> {
    let passed = file
        .file_id
        .as_ref()
        .or(file.file_data.as_ref())
        .ok_or_else(|| LiteLLMError::Config("file_id or file_data required".into()))?;
    process_gemini_media_url(
        passed,
        file.format.as_deref(),
        file.detail.as_deref(),
        file.video_metadata.as_ref(),
        model,
    )
}

fn process_gemini_media(
    image_url: &ChatImageUrl,
    detail: Option<&str>,
    video_metadata: Option<&Value>,
    format: Option<&str>,
    model: &str,
) -> Result<Value> {
    let (url, format) = match image_url {
        ChatImageUrl::Url(url) => (url.as_str(), format),
        ChatImageUrl::Object(obj) => (obj.url.as_str(), obj.format.as_deref().or(format)),
    };
    process_gemini_media_url(url, format, detail, video_metadata, model)
}

fn process_gemini_media_url(
    url: &str,
    format: Option<&str>,
    detail: Option<&str>,
    video_metadata: Option<&Value>,
    _model: &str,
) -> Result<Value> {
    if url.starts_with("gs://") || url.starts_with("https://") || url.starts_with("http://") {
        let mime_type = format
            .map(|v| v.to_string())
            .or_else(|| mime_type_from_url(url))
            .ok_or_else(|| LiteLLMError::Config("missing media mime type".into()))?;
        let mut part = serde_json::json!({
            "file_data": { "mime_type": mime_type, "file_uri": url }
        });
        apply_gemini_metadata(&mut part, detail, video_metadata);
        return Ok(part);
    }

    if let Some((media_type, data)) = parse_data_url(url, format) {
        let mut part = serde_json::json!({
            "inline_data": { "mime_type": media_type, "data": data }
        });
        apply_gemini_metadata(&mut part, detail, video_metadata);
        return Ok(part);
    }

    Err(LiteLLMError::Config("unsupported gemini media url".into()))
}

fn apply_gemini_metadata(part: &mut Value, detail: Option<&str>, video_metadata: Option<&Value>) {
    if let Some(detail) = detail {
        if let Some(level) = detail_to_media_resolution(detail) {
            if let Some(obj) = part.as_object_mut() {
                obj.insert(
                    "media_resolution".to_string(),
                    serde_json::json!({ "level": level }),
                );
            }
        }
    }
    if let Some(video_metadata) = video_metadata {
        if let Some(obj) = part.as_object_mut() {
            obj.insert("video_metadata".to_string(), video_metadata.clone());
        }
    }
}

fn detail_to_media_resolution(detail: &str) -> Option<&'static str> {
    match detail {
        "low" => Some("MEDIA_RESOLUTION_LOW"),
        "medium" => Some("MEDIA_RESOLUTION_MEDIUM"),
        "high" => Some("MEDIA_RESOLUTION_HIGH"),
        "ultra_high" => Some("MEDIA_RESOLUTION_ULTRA_HIGH"),
        _ => None,
    }
}

fn parse_data_url(url: &str, override_format: Option<&str>) -> Option<(String, String)> {
    if url.starts_with("data:") {
        let stripped = url.strip_prefix("data:").unwrap_or(url);
        if let Some((meta, data)) = stripped.split_once(',') {
            let mut media_type = meta.split(';').next().unwrap_or("application/octet-stream");
            if let Some(fmt) = override_format {
                media_type = fmt;
            }
            return Some((media_type.to_string(), data.to_string()));
        }
    }
    None
}

fn mime_type_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url);
    MimeGuess::from_path(path)
        .first_raw()
        .map(|m| m.to_string())
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
