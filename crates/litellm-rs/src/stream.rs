use crate::error::{LiteLLMError, Result};
use crate::http::MAX_SSE_BUFFER_SIZE;
use crate::types::Usage;
use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt, TryStreamExt};
use serde_json::Value;
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;

#[derive(Debug, Clone)]
pub struct ChatStreamChunk {
    pub content: String,
    pub raw: Option<Value>,
    pub usage: Option<Usage>,
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatStreamChunk>> + Send>>;

#[derive(Debug, Clone)]
struct SseEvent {
    event: Option<String>,
    data: String,
}

type SseEventStream = Pin<Box<dyn Stream<Item = Result<SseEvent>> + Send>>;

fn sse_event_stream<S>(stream: S) -> SseEventStream
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let s = async_stream::try_stream! {
        let stream = stream.map_err(std::io::Error::other);
        let reader = StreamReader::new(stream);
        let mut lines = BufReader::new(reader).lines();

        let mut event_name: Option<String> = None;
        let mut data_buf = String::new();

        while let Some(line) = lines.next_line().await.map_err(LiteLLMError::from)? {
            if line.is_empty() {
                if !data_buf.is_empty() {
                    let data = std::mem::take(&mut data_buf);
                    let event = event_name.take();
                    yield SseEvent { event, data };
                } else {
                    event_name = None;
                }
                continue;
            }

            if line.starts_with(':') {
                continue;
            }

            let (field, value) = if let Some((field, value)) = line.split_once(':') {
                (field, value.strip_prefix(' ').unwrap_or(value))
            } else {
                (line.as_str(), "")
            };

            match field {
                "event" => {
                    event_name = Some(value.to_string());
                }
                "data" => {
                    if !data_buf.is_empty() {
                        data_buf.push('\n');
                    }
                    data_buf.push_str(value);
                    if data_buf.len() > MAX_SSE_BUFFER_SIZE {
                        Err(LiteLLMError::http(format!(
                            "SSE data buffer exceeded maximum size of {} bytes",
                            MAX_SSE_BUFFER_SIZE
                        )))?;
                    }
                }
                _ => {}
            }
        }

        if !data_buf.is_empty() {
            let data = std::mem::take(&mut data_buf);
            let event = event_name.take();
            yield SseEvent { event, data };
        }
    };
    Box::pin(s)
}

/// Parse an OpenAI-compatible SSE stream into chat chunks.
///
/// This function includes protection against unbounded memory growth by limiting
/// the internal buffer size to `MAX_SSE_BUFFER_SIZE`.
pub fn parse_sse_stream<S>(stream: S) -> ChatStream
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let s = async_stream::try_stream! {
        let mut events = sse_event_stream(stream);
        while let Some(event) = events.next().await {
            let event = event?;
            let data = event.data.trim();
            if data == "[DONE]" {
                return;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|e| LiteLLMError::Parse(e.to_string()))?;
            let usage = parse_usage(&value);
            let content = value
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            yield ChatStreamChunk {
                content,
                raw: Some(value),
                usage,
            };
        }
    };
    Box::pin(s)
}

/// Parse an Anthropic SSE stream into chat chunks.
///
/// This function includes protection against unbounded memory growth by limiting
/// the internal buffer size to `MAX_SSE_BUFFER_SIZE`.
pub fn parse_anthropic_sse_stream<S>(stream: S) -> ChatStream
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let s = async_stream::try_stream! {
        let mut events = sse_event_stream(stream);
        while let Some(event) = events.next().await {
            let event = event?;
            let data = event.data.trim();
            if data == "[DONE]" {
                return;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|e| LiteLLMError::Parse(e.to_string()))?;
            let usage = parse_usage(&value);
            if event.event.as_deref() == Some("content_block_delta") {
                let content = value
                    .pointer("/delta/text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !content.is_empty() {
                    yield ChatStreamChunk {
                        content,
                        raw: Some(value),
                        usage,
                    };
                }
            }
        }
    };
    Box::pin(s)
}

fn parse_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?.as_object()?;
    let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64());
    let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64());
    let total_tokens = usage.get("total_tokens").and_then(|v| v.as_u64());
    let cost_usd = usage
        .get("cost")
        .and_then(|v| v.as_f64())
        .or_else(|| usage.get("cost").and_then(|v| v.as_str())?.parse().ok())
        .or_else(|| usage.get("cost_usd").and_then(|v| v.as_f64()))
        .or_else(|| usage.get("total_cost").and_then(|v| v.as_f64()));
    Some(Usage {
        prompt_tokens,
        completion_tokens,
        thoughts_tokens: None,
        total_tokens,
        cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;

    #[tokio::test]
    async fn parse_sse_basic() {
        let data = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\" World\"}}]}\n\n\
                    data: [DONE]\n\n";
        let bytes_stream = stream::iter(vec![Ok(Bytes::from(data))]);
        let mut chat_stream = parse_sse_stream(bytes_stream);

        let chunk1 = chat_stream.next().await.unwrap().unwrap();
        assert_eq!(chunk1.content, "Hello");

        let chunk2 = chat_stream.next().await.unwrap().unwrap();
        assert_eq!(chunk2.content, " World");

        assert!(chat_stream.next().await.is_none());
    }

    #[tokio::test]
    async fn parse_anthropic_sse_basic() {
        let data = "event: content_block_delta\n\
                    data: {\"delta\":{\"text\":\"Hello\"}}\n\n\
                    event: content_block_delta\n\
                    data: {\"delta\":{\"text\":\" World\"}}\n\n";
        let bytes_stream = stream::iter(vec![Ok(Bytes::from(data))]);
        let mut chat_stream = parse_anthropic_sse_stream(bytes_stream);

        let chunk1 = chat_stream.next().await.unwrap().unwrap();
        assert_eq!(chunk1.content, "Hello");

        let chunk2 = chat_stream.next().await.unwrap().unwrap();
        assert_eq!(chunk2.content, " World");
    }

    #[tokio::test]
    async fn parse_sse_handles_split_chunks() {
        // Simulate data coming in multiple network chunks
        let chunk1 = "data: {\"choices\":[{\"delta\":{\"con";
        let chunk2 = "tent\":\"Split\"}}]}\n\ndata: [DONE]\n\n";
        let bytes_stream = stream::iter(vec![Ok(Bytes::from(chunk1)), Ok(Bytes::from(chunk2))]);
        let mut chat_stream = parse_sse_stream(bytes_stream);

        let chunk = chat_stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.content, "Split");

        assert!(chat_stream.next().await.is_none());
    }
}
