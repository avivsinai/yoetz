# litellm-rs

Minimal Rust SDK port of LiteLLM (library only). Provides a unified interface for chat, embeddings, images, and video across OpenAI‑compatible endpoints plus Anthropic, Gemini, and xAI (via OpenAI‑compatible API).

## Status

- OpenAI‑compatible: chat, embeddings, image generation, video generation (Sora‑style)
- Anthropic: chat (messages API)
- Gemini: chat + video generation (Veo LRO)
- Streaming: OpenAI‑compatible SSE

## Usage

```rust
use litellm_rs::{
    LiteLLM, ProviderConfig, ProviderKind,
    ChatRequest, EmbeddingRequest, ImageRequest, VideoRequest,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = LiteLLM::new()?
        .with_provider(
            "openai",
            ProviderConfig::default()
                .with_kind(ProviderKind::OpenAICompatible)
                .with_api_key_env("OPENAI_API_KEY"),
        )
        .with_provider(
            "gemini",
            ProviderConfig::default()
                .with_kind(ProviderKind::Gemini)
                .with_api_key_env("GEMINI_API_KEY"),
        );

    let resp = client
        .completion(ChatRequest::new("openai/gpt-5.2").message("user", "hello"))
        .await?;
    println!("{}", resp.content);

    let embed = client
        .embedding(EmbeddingRequest {
            model: "openai/text-embedding-3-small".to_string(),
            input: serde_json::json!("hello"),
        })
        .await?;
    println!("{} vectors", embed.vectors.len());

    let images = client
        .image_generation(ImageRequest {
            model: "openai/gpt-image-1.5".to_string(),
            prompt: "A cozy cabin in snow".to_string(),
            n: Some(1),
            size: None,
            quality: None,
            background: None,
        })
        .await?;
    println!("{} images", images.images.len());

    let video = client
        .video_generation(VideoRequest {
            model: "openai/sora-2-pro".to_string(),
            prompt: "Drone flyover".to_string(),
            seconds: Some(5),
            size: None,
        })
        .await?;
    println!("video url: {:?}", video.video_url);

    Ok(())
}
```

## Streaming

```rust
use futures_util::StreamExt;
use litellm_rs::{LiteLLM, ChatRequest};

# async fn run() -> anyhow::Result<()> {
let client = LiteLLM::new()?;
let mut stream = client
    .stream_completion(ChatRequest::new("openai/gpt-5.2").message("user", "hello"))
    .await?;
while let Some(chunk) = stream.next().await {
    let chunk = chunk?;
    print!("{}", chunk.content);
}
# Ok(())
# }
```

## Notes

- xAI uses OpenAI‑compatible endpoints. Configure provider `xai` with base URL `https://api.x.ai/v1` and `XAI_API_KEY`.
- This crate intentionally excludes LiteLLM proxy/server features.
