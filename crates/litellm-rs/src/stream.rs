use crate::error::{LiteLLMError, Result};
use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use serde_json::Value;
use std::pin::Pin;

#[derive(Debug, Clone)]
pub struct ChatStreamChunk {
    pub content: String,
    pub raw: Option<Value>,
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatStreamChunk>> + Send>>;

pub fn parse_sse_stream<S>(stream: S) -> ChatStream
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let s = async_stream::try_stream! {
        let mut buffer = String::new();
        futures_util::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| LiteLLMError::Http(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let mut line = buffer[..pos].to_string();
                buffer = buffer[pos + 1..].to_string();
                line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line.trim_start_matches("data:").trim();
                if data == "[DONE]" {
                    return;
                }
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| LiteLLMError::Parse(e.to_string()))?;
                let content = value
                    .pointer("/choices/0/delta/content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                yield ChatStreamChunk {
                    content,
                    raw: Some(value),
                };
            }
        }
    };
    Box::pin(s)
}
