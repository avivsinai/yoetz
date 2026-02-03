use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::providers::resolve_api_key;
use crate::types::{
    ChatContentPart, ChatContentPartFile, ChatContentPartImageUrl, ChatContentPartInputAudio,
    ChatContentPartText, ChatFile, ChatImageUrl, ChatInputAudio, ChatMessage, ChatMessageContent,
    ChatRequest, ChatResponse, Usage, VideoRequest, VideoResponse,
};
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

    let contents = gemini_contents_from_messages(messages, &req.model)?;

    let mut body = serde_json::json!({ "contents": contents });
    if !system_instruction.is_empty() {
        body["system_instruction"] = serde_json::json!({ "parts": system_instruction });
    }
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

fn gemini_contents_from_messages(messages: Vec<ChatMessage>, model: &str) -> Result<Vec<Value>> {
    let mut contents: Vec<Value> = Vec::new();
    for message in messages {
        let role = map_gemini_role(&message.role);
        let parts = gemini_parts_from_content(&message.content, model)?;
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = contents.last_mut() {
            if last.get("role").and_then(|v| v.as_str()) == Some(role.as_str()) {
                if let Some(existing_parts) = last.get_mut("parts").and_then(|v| v.as_array_mut()) {
                    existing_parts.extend(parts);
                    continue;
                }
            }
        }
        contents.push(serde_json::json!({ "role": role, "parts": parts }));
    }
    if contents.is_empty() {
        contents.push(serde_json::json!({
            "role": "user",
            "parts": [{ "text": " " }]
        }));
    }
    Ok(contents)
}

fn map_gemini_role(role: &str) -> String {
    match role {
        "assistant" => "model".to_string(),
        _ => "user".to_string(),
    }
}

fn gemini_parts_from_content(content: &ChatMessageContent, model: &str) -> Result<Vec<Value>> {
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
        if let Some((meta, data)) = stripped.split_once(",") {
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
    let ext = path.rsplit('.').next()?.to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => return None,
    };
    Some(mime.to_string())
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
