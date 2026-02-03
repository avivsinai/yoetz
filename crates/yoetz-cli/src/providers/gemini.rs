use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use reqwest::Client;
use serde_json::Value;
use std::path::Path;
use tokio::time::{sleep, Duration};

use yoetz_core::media::{MediaInput, MediaMetadata, MediaOutput, MediaSource, MediaType};
use yoetz_core::types::Usage;

use crate::http::send_json;
use crate::providers::ProviderAuth;

const INLINE_LIMIT_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct GeminiTextResult {
    pub content: String,
    pub usage: Usage,
}

pub async fn generate_content(
    client: &Client,
    auth: &ProviderAuth,
    prompt: &str,
    model: &str,
    images: &[MediaInput],
    video: Option<&MediaInput>,
    temperature: f32,
    max_output_tokens: usize,
) -> Result<GeminiTextResult> {
    let mut parts = Vec::with_capacity(images.len() + 2);
    parts.push(serde_json::json!({ "text": prompt }));

    for image in images {
        parts.push(media_part(client, auth, image).await?);
    }
    if let Some(video) = video {
        parts.push(media_part(client, auth, video).await?);
    }

    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": parts }],
        "generationConfig": {
            "temperature": temperature,
            "maxOutputTokens": max_output_tokens,
        }
    });

    let url = format!(
        "{}/models/{}:generateContent",
        auth.base_url.trim_end_matches('/'),
        model
    );

    let (resp, _headers) = send_json::<Value>(
        client
            .post(url)
            .header("x-goog-api-key", &auth.api_key)
            .json(&body),
    )
    .await?;

    Ok(GeminiTextResult {
        content: extract_text(&resp),
        usage: parse_usage(&resp),
    })
}

pub async fn generate_video_veo(
    client: &Client,
    auth: &ProviderAuth,
    prompt: &str,
    model: &str,
    reference_images: &[MediaInput],
    duration_secs: Option<u32>,
    aspect_ratio: Option<&str>,
    resolution: Option<&str>,
    negative_prompt: Option<&str>,
    output_path: &Path,
) -> Result<MediaOutput> {
    let mut parameters = serde_json::json!({});
    if let Some(duration) = duration_secs {
        parameters["durationSeconds"] = Value::from(duration as i64);
    }
    if let Some(aspect) = aspect_ratio {
        parameters["aspectRatio"] = Value::String(aspect.to_string());
    }
    if let Some(resolution) = resolution {
        parameters["resolution"] = Value::String(resolution.to_string());
    }
    if let Some(negative) = negative_prompt {
        parameters["negativePrompt"] = Value::String(negative.to_string());
    }

    if !reference_images.is_empty() {
        let mut refs = Vec::with_capacity(reference_images.len());
        for image in reference_images {
            let payload = veo_image_payload(client, auth, image).await?;
            refs.push(serde_json::json!({ "image": payload }));
        }
        parameters["referenceImages"] = Value::Array(refs);
    }

    let instance = serde_json::json!({ "prompt": prompt });
    let body = serde_json::json!({
        "instances": [instance],
        "parameters": parameters,
    });

    let url = format!(
        "{}/models/{}:predictLongRunning",
        auth.base_url.trim_end_matches('/'),
        model
    );

    let (resp, _headers) = send_json::<Value>(
        client
            .post(url)
            .header("x-goog-api-key", &auth.api_key)
            .json(&body),
    )
    .await?;

    let op_name = resp
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing operation name"))?;

    let op_url = if op_name.starts_with("http") {
        op_name.to_string()
    } else {
        format!("{}/{}", auth.base_url.trim_end_matches('/'), op_name)
    };

    let mut attempts = 0u32;
    let response = loop {
        let (op_resp, _headers) =
            send_json::<Value>(client.get(&op_url).header("x-goog-api-key", &auth.api_key)).await?;
        if op_resp.get("done").and_then(|v| v.as_bool()) == Some(true) {
            break op_resp;
        }
        attempts += 1;
        if attempts > 240 {
            return Err(anyhow!("timed out waiting for video"));
        }
        sleep(Duration::from_secs(5)).await;
    };

    if let Some(error) = response.get("error") {
        return Err(anyhow!("video generation failed: {error}"));
    }

    let uri =
        extract_video_uri(&response).ok_or_else(|| anyhow!("missing video uri in response"))?;

    let bytes = match client
        .get(&uri)
        .header("x-goog-api-key", &auth.api_key)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp.bytes().await?,
        _ => client.get(&uri).send().await?.bytes().await?,
    };

    std::fs::write(output_path, &bytes)
        .with_context(|| format!("write {}", output_path.display()))?;

    Ok(MediaOutput {
        media_type: MediaType::Video,
        path: output_path.to_path_buf(),
        url: Some(uri),
        metadata: MediaMetadata {
            width: None,
            height: None,
            duration_secs: duration_secs.map(|d| d as f32),
            model: model.to_string(),
            revised_prompt: None,
        },
    })
}

async fn media_part(client: &Client, auth: &ProviderAuth, media: &MediaInput) -> Result<Value> {
    match &media.source {
        MediaSource::Url(url) => Ok(serde_json::json!({
            "file_data": {
                "mime_type": media.mime_type,
                "file_uri": url,
            }
        })),
        MediaSource::FileApiId { id, .. } => Ok(serde_json::json!({
            "file_data": {
                "mime_type": media.mime_type,
                "file_uri": id,
            }
        })),
        MediaSource::Base64 { data, mime } => Ok(serde_json::json!({
            "inline_data": {
                "mime_type": mime,
                "data": data,
            }
        })),
        MediaSource::File(path) => {
            let size = media.size_bytes.unwrap_or(0);
            if size > INLINE_LIMIT_BYTES {
                let bytes =
                    std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                let file = upload_file(client, auth, path, &media.mime_type, bytes).await?;
                Ok(serde_json::json!({
                    "file_data": {
                        "mime_type": media.mime_type,
                        "file_uri": file,
                    }
                }))
            } else {
                let bytes =
                    std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                let b64 = general_purpose::STANDARD.encode(bytes);
                Ok(serde_json::json!({
                    "inline_data": {
                        "mime_type": media.mime_type,
                        "data": b64,
                    }
                }))
            }
        }
    }
}

async fn veo_image_payload(
    client: &Client,
    auth: &ProviderAuth,
    media: &MediaInput,
) -> Result<Value> {
    match &media.source {
        MediaSource::Url(url) => Ok(serde_json::json!({
            "fileUri": url,
            "mimeType": media.mime_type,
        })),
        MediaSource::FileApiId { id, .. } => Ok(serde_json::json!({
            "fileUri": id,
            "mimeType": media.mime_type,
        })),
        MediaSource::Base64 { data, mime } => Ok(serde_json::json!({
            "inlineData": {
                "data": data,
                "mimeType": mime,
            }
        })),
        MediaSource::File(path) => {
            let size = media.size_bytes.unwrap_or(0);
            if size > INLINE_LIMIT_BYTES {
                let bytes =
                    std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                let file = upload_file(client, auth, path, &media.mime_type, bytes).await?;
                Ok(serde_json::json!({
                    "fileUri": file,
                    "mimeType": media.mime_type,
                }))
            } else {
                let bytes =
                    std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                let b64 = general_purpose::STANDARD.encode(bytes);
                Ok(serde_json::json!({
                    "inlineData": {
                        "data": b64,
                        "mimeType": media.mime_type,
                    }
                }))
            }
        }
    }
}

async fn upload_file(
    client: &Client,
    auth: &ProviderAuth,
    path: &Path,
    mime_type: &str,
    bytes: Vec<u8>,
) -> Result<String> {
    let base_root = auth
        .base_url
        .trim_end_matches("/v1beta")
        .trim_end_matches('/');
    let start_url = format!("{}/upload/v1beta/files?key={}", base_root, auth.api_key);
    let display_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload");

    let start_resp = client
        .post(start_url)
        .header("X-Goog-Upload-Protocol", "resumable")
        .header("X-Goog-Upload-Command", "start")
        .header("X-Goog-Upload-Header-Content-Type", mime_type)
        .header(
            "X-Goog-Upload-Header-Content-Length",
            bytes.len().to_string(),
        )
        .json(&serde_json::json!({ "file": { "display_name": display_name } }))
        .send()
        .await?;

    let status = start_resp.status();
    let headers = start_resp.headers().clone();
    if !status.is_success() {
        let text = start_resp.text().await.unwrap_or_default();
        return Err(anyhow!("upload start failed {}: {}", status.as_u16(), text));
    }

    let upload_url = headers
        .get("x-goog-upload-url")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow!("missing upload url"))?
        .to_string();

    let (resp, _headers) = send_json::<Value>(
        client
            .post(upload_url)
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("X-Goog-Upload-Offset", "0")
            .body(bytes),
    )
    .await?;

    let file = resp.get("file").unwrap_or(&resp);
    let uri = file
        .get("uri")
        .and_then(|v| v.as_str())
        .or_else(|| file.get("file_uri").and_then(|v| v.as_str()))
        .ok_or_else(|| anyhow!("missing file uri"))?;

    Ok(uri.to_string())
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
        let input_tokens = meta
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let output_tokens = meta
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let total_tokens = meta
            .get("totalTokenCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        return Usage {
            input_tokens,
            output_tokens,
            total_tokens,
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
