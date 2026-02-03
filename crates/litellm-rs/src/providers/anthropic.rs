use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::providers::resolve_api_key;
use crate::types::{
    ChatContentPart, ChatContentPartImageUrl, ChatContentPartText, ChatImageUrl, ChatMessage,
    ChatMessageContent, ChatRequest, ChatResponse, Usage,
};
use base64::{engine::general_purpose, Engine as _};
use reqwest::Client;
use serde_json::Value;

const DEFAULT_VERSION: &str = "2023-06-01";

pub async fn chat(client: &Client, cfg: &ProviderConfig, req: ChatRequest) -> Result<ChatResponse> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/v1/messages", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;
    let key = key.ok_or_else(|| LiteLLMError::MissingApiKey("ANTHROPIC_API_KEY".into()))?;

    let mut messages = req.messages;
    let system_blocks = extract_system_blocks(client, &mut messages).await?;
    let mut anthropic_messages = Vec::with_capacity(messages.len());
    for message in messages {
        anthropic_messages.push(anthropic_message_from_chat(client, message).await?);
    }

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": anthropic_messages,
        "max_tokens": req
            .max_tokens
            .or(req.max_completion_tokens)
            .unwrap_or(1024),
    });
    if !system_blocks.is_empty() {
        body["system"] = serde_json::json!(system_blocks);
    }
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(top_p) = req.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(stop_sequences) = map_stop_sequences(req.stop) {
        body["stop_sequences"] = serde_json::json!(stop_sequences);
    }

    let mut tools = req.tools;
    let mut tool_choice = req.tool_choice;
    let output_format =
        map_response_format_to_output_format(&req.model, req.response_format.as_ref());
    if let Some(output_format) = output_format {
        body["output_format"] = output_format;
    } else if let Some(response_tool) = map_response_format_to_tool(req.response_format.as_ref()) {
        tools = merge_tools(tools, response_tool);
        if tool_choice.is_none() {
            tool_choice = Some(serde_json::json!({
                "type": "tool",
                "name": RESPONSE_FORMAT_TOOL_NAME,
            }));
        }
    }

    if let Some(tools_value) = tools {
        body["tools"] = tools_value;
    }
    if let Some(tool_choice_value) = tool_choice {
        body["tool_choice"] = tool_choice_value;
    }

    if let Some(metadata) = req.metadata {
        body["metadata"] = metadata;
    } else if let Some(user) = req.user {
        body["metadata"] = serde_json::json!({ "user_id": user });
    }

    let mut builder = client
        .post(url)
        .header("x-api-key", key)
        .header("anthropic-version", DEFAULT_VERSION)
        .json(&body);
    for (k, v) in &cfg.extra_headers {
        builder = builder.header(k, v);
    }

    let (parsed, _headers) = send_json::<Value>(builder).await?;
    let content = extract_text(&parsed);
    let usage = parse_usage(&parsed);

    Ok(ChatResponse {
        content,
        usage,
        raw: None,
    })
}

pub async fn chat_stream(
    client: &Client,
    cfg: &ProviderConfig,
    req: ChatRequest,
) -> Result<crate::stream::ChatStream> {
    let base = cfg
        .base_url
        .clone()
        .ok_or_else(|| LiteLLMError::Config("base_url required".into()))?;
    let url = format!("{}/v1/messages", base.trim_end_matches('/'));
    let key = resolve_api_key(cfg)?;
    let key = key.ok_or_else(|| LiteLLMError::MissingApiKey("ANTHROPIC_API_KEY".into()))?;

    let mut messages = req.messages;
    let system_blocks = extract_system_blocks(client, &mut messages).await?;
    let mut anthropic_messages = Vec::with_capacity(messages.len());
    for message in messages {
        anthropic_messages.push(anthropic_message_from_chat(client, message).await?);
    }

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": anthropic_messages,
        "max_tokens": req
            .max_tokens
            .or(req.max_completion_tokens)
            .unwrap_or(1024),
        "stream": true,
    });
    if !system_blocks.is_empty() {
        body["system"] = serde_json::json!(system_blocks);
    }
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(top_p) = req.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(stop_sequences) = map_stop_sequences(req.stop) {
        body["stop_sequences"] = serde_json::json!(stop_sequences);
    }

    let mut tools = req.tools;
    let mut tool_choice = req.tool_choice;
    let output_format =
        map_response_format_to_output_format(&req.model, req.response_format.as_ref());
    if let Some(output_format) = output_format {
        body["output_format"] = output_format;
    } else if let Some(response_tool) = map_response_format_to_tool(req.response_format.as_ref()) {
        tools = merge_tools(tools, response_tool);
        if tool_choice.is_none() {
            tool_choice = Some(serde_json::json!({
                "type": "tool",
                "name": RESPONSE_FORMAT_TOOL_NAME,
            }));
        }
    }

    if let Some(tools_value) = tools {
        body["tools"] = tools_value;
    }
    if let Some(tool_choice_value) = tool_choice {
        body["tool_choice"] = tool_choice_value;
    }

    if let Some(metadata) = req.metadata {
        body["metadata"] = metadata;
    } else if let Some(user) = req.user {
        body["metadata"] = serde_json::json!({ "user_id": user });
    }

    let mut builder = client
        .post(url)
        .header("x-api-key", key)
        .header("anthropic-version", DEFAULT_VERSION)
        .header("accept", "text/event-stream")
        .json(&body);
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

    Ok(crate::stream::parse_anthropic_sse_stream(
        resp.bytes_stream(),
    ))
}

const RESPONSE_FORMAT_TOOL_NAME: &str = "response_format";

fn map_stop_sequences(value: Option<Value>) -> Option<Vec<String>> {
    let value = value?;
    if let Some(s) = value.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(vec![trimmed.to_string()]);
    }
    if let Some(arr) = value.as_array() {
        let mut out = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    } else {
        None
    }
}

fn map_response_format_to_output_format(model: &str, value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let type_value = value.get("type")?.as_str()?;
    if type_value == "text" {
        return None;
    }
    if !model_supports_output_format(model) {
        return None;
    }
    let schema = extract_json_schema(value)?;
    Some(serde_json::json!({
        "type": "json_schema",
        "schema": schema,
    }))
}

fn map_response_format_to_tool(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let type_value = value.get("type")?.as_str()?;
    if type_value == "text" {
        return None;
    }
    let schema = extract_json_schema(value)?;
    Some(serde_json::json!({
        "name": RESPONSE_FORMAT_TOOL_NAME,
        "input_schema": schema,
    }))
}

fn extract_json_schema(value: &Value) -> Option<Value> {
    if let Some(schema) = value.get("response_schema") {
        return Some(schema.clone());
    }
    if let Some(schema) = value.get("json_schema").and_then(|v| v.get("schema")) {
        return Some(schema.clone());
    }
    if value.get("type").and_then(|v| v.as_str()) == Some("json_object") {
        return Some(serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true,
        }));
    }
    None
}

fn model_supports_output_format(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("sonnet-4.5")
        || lower.contains("sonnet-4-5")
        || lower.contains("opus-4.1")
        || lower.contains("opus-4-1")
}

fn merge_tools(tools: Option<Value>, new_tool: Value) -> Option<Value> {
    match tools {
        None => Some(Value::Array(vec![new_tool])),
        Some(Value::Array(mut arr)) => {
            arr.push(new_tool);
            Some(Value::Array(arr))
        }
        Some(other) => Some(Value::Array(vec![other, new_tool])),
    }
}

fn extract_text(resp: &Value) -> String {
    if let Some(content) = resp.get("content").and_then(|v| v.as_array()) {
        let mut out = String::new();
        for part in content {
            if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    out.push_str(text);
                }
            }
        }
        return out;
    }
    resp.get("completion")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn parse_usage(resp: &Value) -> Usage {
    let usage = resp.get("usage").and_then(|v| v.as_object());
    if let Some(u) = usage {
        return Usage {
            prompt_tokens: u
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            completion_tokens: u
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            total_tokens: None,
            cost_usd: None,
        };
    }
    Usage::default()
}

#[derive(Debug, serde::Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type")]
enum AnthropicImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

async fn extract_system_blocks(
    client: &Client,
    messages: &mut Vec<ChatMessage>,
) -> Result<Vec<AnthropicContentBlock>> {
    let mut blocks = Vec::new();
    let mut indices = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "system" {
            let mut msg_blocks = anthropic_blocks_from_content(client, &msg.content).await?;
            if msg_blocks.is_empty() {
                continue;
            }
            blocks.append(&mut msg_blocks);
            indices.push(idx);
        }
    }
    for idx in indices.into_iter().rev() {
        messages.remove(idx);
    }
    Ok(blocks)
}

async fn anthropic_message_from_chat(
    client: &Client,
    message: ChatMessage,
) -> Result<AnthropicMessage> {
    let role = match message.role.as_str() {
        "user" | "assistant" => message.role,
        other => {
            return Err(LiteLLMError::Config(format!(
                "unsupported anthropic role: {}",
                other
            )))
        }
    };
    let content = anthropic_blocks_from_content(client, &message.content).await?;
    Ok(AnthropicMessage { role, content })
}

async fn anthropic_blocks_from_content(
    client: &Client,
    content: &ChatMessageContent,
) -> Result<Vec<AnthropicContentBlock>> {
    match content {
        ChatMessageContent::Text(text) => Ok(vec![AnthropicContentBlock::Text {
            text: text.to_string(),
        }]),
        ChatMessageContent::Parts(parts) => {
            let mut out = Vec::new();
            for part in parts {
                match part {
                    ChatContentPart::Text(ChatContentPartText { text, .. }) => {
                        if text.is_empty() {
                            continue;
                        }
                        out.push(AnthropicContentBlock::Text { text: text.clone() });
                    }
                    ChatContentPart::ImageUrl(ChatContentPartImageUrl { image_url, .. }) => {
                        let source = anthropic_image_source(client, image_url).await?;
                        out.push(AnthropicContentBlock::Image { source });
                    }
                    ChatContentPart::InputAudio(_) | ChatContentPart::File(_) => {
                        return Err(LiteLLMError::Config(
                            "anthropic does not support input_audio/file parts".into(),
                        ));
                    }
                    ChatContentPart::Other(value) => {
                        return Err(LiteLLMError::Config(format!(
                            "unsupported anthropic content part: {}",
                            value
                        )));
                    }
                }
            }
            Ok(out)
        }
    }
}

async fn anthropic_image_source(
    client: &Client,
    image_url: &ChatImageUrl,
) -> Result<AnthropicImageSource> {
    let (url, format) = match image_url {
        ChatImageUrl::Url(url) => (url.as_str(), None),
        ChatImageUrl::Object(obj) => (obj.url.as_str(), obj.format.as_deref()),
    };

    if url.starts_with("https://") {
        return Ok(AnthropicImageSource::Url {
            url: url.to_string(),
        });
    }
    if url.starts_with("http://") {
        let bytes = client
            .get(url)
            .send()
            .await
            .map_err(|e| LiteLLMError::Http(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| LiteLLMError::Http(e.to_string()))?;
        let data = general_purpose::STANDARD.encode(bytes);
        let media_type = format.unwrap_or("application/octet-stream").to_string();
        return Ok(AnthropicImageSource::Base64 { media_type, data });
    }

    let (media_type, data) = parse_data_url(url, format)?;
    Ok(AnthropicImageSource::Base64 { media_type, data })
}

fn parse_data_url(url: &str, override_format: Option<&str>) -> Result<(String, String)> {
    if url.starts_with("data:") {
        let stripped = url.strip_prefix("data:").unwrap_or(url);
        if let Some((meta, data)) = stripped.split_once(",") {
            let mut media_type = meta.split(';').next().unwrap_or("application/octet-stream");
            if let Some(fmt) = override_format {
                media_type = fmt;
            }
            let data = data.to_string();
            return Ok((media_type.to_string(), data));
        }
    }

    if let Some(fmt) = override_format {
        return Ok((fmt.to_string(), url.to_string()));
    }

    Err(LiteLLMError::Config(
        "expected data URL for anthropic image; provide data:...;base64,... or format".into(),
    ))
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
