use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde_json::Value;
use std::path::Path;
use tokio::time::{sleep, Duration};

use yoetz_core::media::{MediaInput, MediaMetadata, MediaOutput, MediaType};
use yoetz_core::types::Usage;

use crate::http::send_json;
use crate::providers::ProviderAuth;

#[derive(Debug, Clone)]
pub struct OpenAITextResult {
    pub content: String,
    pub usage: Usage,
    pub response_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OpenAIMediaResult {
    pub outputs: Vec<MediaOutput>,
    pub usage: Usage,
}

pub async fn call_responses_vision(
    client: &Client,
    auth: &ProviderAuth,
    prompt: &str,
    model: &str,
    images: &[MediaInput],
    response_format: Option<Value>,
    temperature: f32,
    max_output_tokens: usize,
) -> Result<OpenAITextResult> {
    if model.contains("gpt-image") {
        return Err(anyhow!(
            "openai image models are not valid for the Responses API; use a text model"
        ));
    }
    let url = format!("{}/responses", auth.base_url.trim_end_matches('/'));

    let mut content = Vec::with_capacity(images.len() + 1);
    content.push(serde_json::json!({ "type": "input_text", "text": prompt }));
    for image in images {
        let data_url = image.as_data_url()?;
        content.push(serde_json::json!({
            "type": "input_image",
            "image_url": data_url,
        }));
    }

    let mut body = serde_json::json!({
        "model": model,
        "input": [{ "role": "user", "content": content }],
        "temperature": temperature,
        "max_output_tokens": max_output_tokens,
    });
    if let Some(format) = response_format {
        body["response_format"] = format;
    }

    let (resp, _headers) =
        send_json::<Value>(client.post(url).bearer_auth(&auth.api_key).json(&body)).await?;

    Ok(OpenAITextResult {
        content: extract_output_text(&resp),
        usage: parse_usage(&resp),
        response_id: resp
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

pub async fn generate_images(
    client: &Client,
    auth: &ProviderAuth,
    prompt: &str,
    model: &str,
    images: &[MediaInput],
    size: Option<&str>,
    quality: Option<&str>,
    background: Option<&str>,
    n: usize,
    output_dir: &Path,
) -> Result<OpenAIMediaResult> {
    if model.contains("gpt-image") {
        return generate_images_via_images_api(
            client, auth, prompt, model, images, size, quality, background, n, output_dir,
        )
        .await;
    }
    let url = format!("{}/responses", auth.base_url.trim_end_matches('/'));

    let mut tool = serde_json::json!({
        "type": "image_generation",
        "n": n,
    });
    if let Some(size) = size {
        tool["size"] = Value::String(size.to_string());
    }
    if let Some(quality) = quality {
        tool["quality"] = Value::String(quality.to_string());
    }
    if let Some(background) = background {
        tool["background"] = Value::String(background.to_string());
    }

    let input = if images.is_empty() {
        Value::String(prompt.to_string())
    } else {
        let mut content = Vec::with_capacity(images.len() + 1);
        content.push(serde_json::json!({ "type": "input_text", "text": prompt }));
        for image in images {
            let data_url = image.as_data_url()?;
            content.push(serde_json::json!({
                "type": "input_image",
                "image_url": data_url,
            }));
        }
        serde_json::json!([{ "role": "user", "content": content }])
    };

    let body = serde_json::json!({
        "model": model,
        "input": input,
        "tools": [tool],
        "tool_choice": { "type": "image_generation" },
    });

    let (resp, _headers) =
        send_json::<Value>(client.post(url).bearer_auth(&auth.api_key).json(&body)).await?;

    let images = extract_image_payloads(&resp);
    if images.is_empty() {
        return Err(anyhow!("no images returned"));
    }

    let mut outputs = Vec::with_capacity(images.len());
    for (idx, payload) in images.into_iter().enumerate() {
        let filename = format!("image_{idx}.png");
        let path = output_dir.join(filename);
        if let Some(b64) = payload.b64_json {
            let bytes = general_purpose::STANDARD
                .decode(b64)
                .context("decode image base64")?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else if let Some(url) = &payload.url {
            let bytes = client
                .get(url)
                .bearer_auth(&auth.api_key)
                .send()
                .await?
                .bytes()
                .await?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else {
            continue;
        }

        outputs.push(MediaOutput {
            media_type: MediaType::Image,
            path,
            url: payload.url,
            metadata: MediaMetadata {
                width: None,
                height: None,
                duration_secs: None,
                model: model.to_string(),
                revised_prompt: payload.revised_prompt,
            },
        });
    }

    Ok(OpenAIMediaResult {
        outputs,
        usage: parse_usage(&resp),
    })
}

async fn generate_images_via_images_api(
    client: &Client,
    auth: &ProviderAuth,
    prompt: &str,
    model: &str,
    images: &[MediaInput],
    size: Option<&str>,
    quality: Option<&str>,
    background: Option<&str>,
    n: usize,
    output_dir: &Path,
) -> Result<OpenAIMediaResult> {
    let base = auth.base_url.trim_end_matches('/');
    let (resp, _headers) = if images.is_empty() {
        let mut body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": n,
        });
        if let Some(size) = size {
            body["size"] = Value::String(size.to_string());
        }
        if let Some(quality) = quality {
            body["quality"] = Value::String(quality.to_string());
        }
        if let Some(background) = background {
            body["background"] = Value::String(background.to_string());
        }
        send_json::<Value>(
            client
                .post(format!("{}/images/generations", base))
                .bearer_auth(&auth.api_key)
                .json(&body),
        )
        .await?
    } else {
        let mut form = Form::new()
            .text("model", model.to_string())
            .text("prompt", prompt.to_string())
            .text("n", n.to_string());
        if let Some(size) = size {
            form = form.text("size", size.to_string());
        }
        if let Some(quality) = quality {
            form = form.text("quality", quality.to_string());
        }
        if let Some(background) = background {
            form = form.text("background", background.to_string());
        }
        let field_name = if images.len() > 1 { "image[]" } else { "image" };
        for image in images {
            let bytes = image.read_bytes()?;
            let filename = match &image.source {
                yoetz_core::media::MediaSource::File(path) => path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("input.png"),
                _ => "input.png",
            };
            let part = Part::bytes(bytes).file_name(filename.to_string());
            form = form.part(field_name, part);
        }
        send_json::<Value>(
            client
                .post(format!("{}/images/edits", base))
                .bearer_auth(&auth.api_key)
                .multipart(form),
        )
        .await?
    };

    let images = extract_image_payloads(&resp);
    if images.is_empty() {
        return Err(anyhow!("no images returned"));
    }

    let mut outputs = Vec::with_capacity(images.len());
    for (idx, payload) in images.into_iter().enumerate() {
        let filename = format!("image_{idx}.png");
        let path = output_dir.join(filename);

        if let Some(b64) = payload.b64_json.as_ref() {
            let bytes = general_purpose::STANDARD
                .decode(b64.as_bytes())
                .context("decode image base64")?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else if let Some(url) = &payload.url {
            let bytes = client
                .get(url)
                .bearer_auth(&auth.api_key)
                .send()
                .await?
                .bytes()
                .await?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else {
            continue;
        }

        outputs.push(MediaOutput {
            media_type: MediaType::Image,
            path,
            url: payload.url,
            metadata: MediaMetadata {
                width: None,
                height: None,
                duration_secs: None,
                model: model.to_string(),
                revised_prompt: payload.revised_prompt,
            },
        });
    }

    Ok(OpenAIMediaResult {
        outputs,
        usage: parse_usage(&resp),
    })
}

pub async fn generate_video_sora(
    client: &Client,
    auth: &ProviderAuth,
    prompt: &str,
    model: &str,
    seconds: Option<u32>,
    size: Option<&str>,
    input_reference: Option<&MediaInput>,
    output_path: &Path,
) -> Result<MediaOutput> {
    let url = format!("{}/videos", auth.base_url.trim_end_matches('/'));

    let mut form = Form::new()
        .text("model", model.to_string())
        .text("prompt", prompt.to_string());

    if let Some(seconds) = seconds {
        form = form.text("seconds", seconds.to_string());
    }
    if let Some(size) = size {
        form = form.text("size", size.to_string());
    }

    if let Some(reference) = input_reference {
        let bytes = reference.read_bytes()?;
        let filename = match &reference.source {
            yoetz_core::media::MediaSource::File(path) => path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("reference.png"),
            _ => "reference.png",
        };
        let part = Part::bytes(bytes).file_name(filename.to_string());
        form = form.part("input_reference", part);
    }

    let (resp, _headers) =
        send_json::<Value>(client.post(url).bearer_auth(&auth.api_key).multipart(form)).await?;

    let video_id = resp
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing video id"))?;

    let status_url = format!(
        "{}/videos/{}",
        auth.base_url.trim_end_matches('/'),
        video_id
    );

    let mut attempts = 0u32;
    loop {
        let (status_resp, _headers) =
            send_json::<Value>(client.get(&status_url).bearer_auth(&auth.api_key)).await?;
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
                return Err(anyhow!(msg.to_string()));
            }
            _ => {
                attempts += 1;
                if attempts > 120 {
                    return Err(anyhow!("timed out waiting for video"));
                }
                sleep(Duration::from_secs(5)).await;
            }
        }
    }

    let content_url = format!(
        "{}/videos/{}/content",
        auth.base_url.trim_end_matches('/'),
        video_id
    );

    let bytes = client
        .get(content_url)
        .bearer_auth(&auth.api_key)
        .send()
        .await?
        .bytes()
        .await?;

    std::fs::write(output_path, &bytes)
        .with_context(|| format!("write {}", output_path.display()))?;

    Ok(MediaOutput {
        media_type: MediaType::Video,
        path: output_path.to_path_buf(),
        url: None,
        metadata: MediaMetadata {
            width: None,
            height: None,
            duration_secs: seconds.map(|s| s as f32),
            model: model.to_string(),
            revised_prompt: None,
        },
    })
}

fn extract_output_text(resp: &Value) -> String {
    if let Some(text) = resp.get("output_text").and_then(|v| v.as_str()) {
        return text.to_string();
    }
    let mut out = String::new();
    if let Some(output) = resp.get("output").and_then(|v| v.as_array()) {
        for item in output {
            if item.get("type").and_then(|v| v.as_str()) == Some("message") {
                if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                    for part in content {
                        if part.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                out.push_str(text);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn parse_usage(resp: &Value) -> Usage {
    let usage = resp.get("usage").and_then(|v| v.as_object());
    if let Some(usage) = usage {
        let input_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let total_tokens = usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let cost_usd = usage
            .get("cost")
            .and_then(|v| v.as_f64())
            .or_else(|| usage.get("cost").and_then(|v| v.as_str())?.parse().ok());
        Usage {
            input_tokens,
            output_tokens,
            thoughts_tokens: None,
            total_tokens,
            cost_usd,
        }
    } else {
        Usage::default()
    }
}

struct ImagePayload {
    b64_json: Option<String>,
    url: Option<String>,
    revised_prompt: Option<String>,
}

fn extract_image_payloads(resp: &Value) -> Vec<ImagePayload> {
    let mut images = Vec::new();

    if let Some(data) = resp.get("data").and_then(|v| v.as_array()) {
        for item in data {
            images.push(ImagePayload {
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
            });
        }
        if !images.is_empty() {
            return images;
        }
    }

    if let Some(output) = resp.get("output").and_then(|v| v.as_array()) {
        for item in output {
            if item.get("type").and_then(|v| v.as_str()) == Some("image_generation_call") {
                let revised_prompt = item
                    .get("revised_prompt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(result) = item.get("result") {
                    if let Some(b64) = result.as_str() {
                        images.push(ImagePayload {
                            b64_json: Some(b64.to_string()),
                            url: None,
                            revised_prompt: revised_prompt.clone(),
                        });
                        continue;
                    }
                    if let Some(obj) = result.as_object() {
                        let b64 = obj
                            .get("b64_json")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let url = obj
                            .get("url")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        images.push(ImagePayload {
                            b64_json: b64,
                            url,
                            revised_prompt: revised_prompt.clone(),
                        });
                        continue;
                    }
                }
            }
            if item.get("type").and_then(|v| v.as_str()) == Some("message") {
                if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                    for part in parts {
                        if part.get("type").and_then(|v| v.as_str()) == Some("output_image") {
                            let b64 = part
                                .get("image_base64")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let url = part
                                .get("image_url")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            images.push(ImagePayload {
                                b64_json: b64,
                                url,
                                revised_prompt: None,
                            });
                        }
                    }
                }
            }
        }
    }

    images
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_output_text_reads_message_parts() {
        let resp = json!({
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "hello"}]}
            ]
        });
        assert_eq!(extract_output_text(&resp), "hello");
    }

    #[test]
    fn extract_image_payloads_from_generation_call() {
        let resp = json!({
            "output": [
                {
                    "type": "image_generation_call",
                    "revised_prompt": "test",
                    "result": {"b64_json": "dGVzdA=="}
                }
            ]
        });
        let images = extract_image_payloads(&resp);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].b64_json.as_deref(), Some("dGVzdA=="));
        assert_eq!(images[0].revised_prompt.as_deref(), Some("test"));
    }
}
