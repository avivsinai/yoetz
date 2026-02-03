use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use crate::providers::resolve_api_key;
use crate::types::{ChatRequest, ChatResponse, Usage};
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

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": req.messages,
        "max_tokens": req.max_tokens.unwrap_or(1024),
    });
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
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
