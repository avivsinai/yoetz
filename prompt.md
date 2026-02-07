<file_tree>
yoetz/
├── crates/
│   ├── litellm-rust/
│   │   ├── src/
│   │   │   ├── client.rs
│   │   │   ├── lib.rs
│   │   │   ├── stream.rs
│   │   │   └── types.rs
│   │   └── Cargo.toml
│   ├── yoetz-cli/
│   │   ├── src/
│   │   │   ├── commands/
│   │   │   │   ├── apply.rs
│   │   │   │   ├── ask.rs
│   │   │   │   ├── bundle.rs
│   │   │   │   ├── council.rs
│   │   │   │   ├── generate.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── models.rs
│   │   │   │   ├── pricing.rs
│   │   │   │   └── review.rs
│   │   │   ├── providers/
│   │   │   │   ├── gemini.rs
│   │   │   │   ├── mod.rs
│   │   │   │   └── openai.rs
│   │   │   ├── browser.rs
│   │   │   ├── budget.rs
│   │   │   ├── http.rs
│   │   │   ├── main.rs
│   │   │   └── registry.rs
│   │   └── Cargo.toml
│   └── yoetz-core/
│       ├── src/
│       │   ├── bundle.rs
│       │   ├── config.rs
│       │   ├── lib.rs
│       │   ├── media.rs
│       │   ├── output.rs
│       │   ├── registry.rs
│       │   ├── session.rs
│       │   └── types.rs
│       └── Cargo.toml
├── Cargo.toml
└── README.md</file_tree>

<files>
File: Cargo.toml (424 tokens)
```
[workspace]
resolver = "2"
members = ["crates/yoetz-core", "crates/yoetz-cli", "crates/litellm-rust"]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.88"
license = "MIT"
repository = "https://github.com/avivsinai/yoetz"
homepage = "https://github.com/avivsinai/yoetz"
description = "Fast CLI-first LLM council, bundler, and multimodal gateway for coding agents"
keywords = ["llm", "cli", "ai", "multimodal", "code-review"]
categories = ["command-line-utilities", "development-tools"]
authors = ["Aviv Sinai"]

[workspace.dependencies]
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "0.8"
thiserror = "1.0"
ignore = "0.4"
rand = "0.8"
time = { version = "0.3", features = ["formatting", "macros"] }
sha2 = "0.10"
hex = "0.4"
base64 = "0.22"
mime_guess = "2.0"
dotenvy = "0.15"

[workspace.lints.rust]
unsafe_code = "warn"
rust_2018_idioms = "warn"

[workspace.lints.clippy]
# Enable standard warnings
all = { level = "warn", priority = -1 }
# Disable noisy lints
cast_possible_truncation = "allow"
needless_pass_by_value = "allow"
too_many_arguments = "allow"
items_after_test_module = "allow"

[profile.release]
lto = "thin"
codegen-units = 1
strip = true

[profile.dev]
opt-level = 0
debug = true

[profile.dev.package."*"]
opt-level = 3

```

File: crates/litellm-rust/Cargo.toml (188 tokens)
```
[package]
name = "litellm-rust"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"

[dependencies]
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
reqwest = { version = "0.12", features = ["json", "rustls-tls", "multipart", "stream"] }
tokio = { version = "1.36", features = ["rt-multi-thread", "macros", "time"] }
async-trait = "0.1"
bytes = "1.5"
futures-util = "0.3"
async-stream = "0.3"
base64 = { workspace = true }
mime_guess = { workspace = true }

[dev-dependencies]
serde_json = { workspace = true }

```

File: crates/litellm-rust/src/client.rs (1047 tokens)
```
use crate::config::{Config, ProviderConfig, ProviderKind};
use crate::error::{LiteLLMError, Result};
use crate::providers::{anthropic, gemini, openai_compat};
use crate::registry::Registry;
use crate::router::{resolve_model, ResolvedModel};
use crate::stream::ChatStream;
use crate::types::{
    ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse, ImageRequest, ImageResponse,
    VideoRequest, VideoResponse,
};
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct LiteLLM {
    config: Config,
    client: Client,
    registry: Registry,
}

impl LiteLLM {
    pub fn new() -> Result<Self> {
        let registry = Registry::load_embedded()?;
        Ok(Self {
            config: Config::default(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|e| LiteLLMError::Http(e.to_string()))?,
            registry,
        })
    }

    pub fn with_default_provider(mut self, provider: impl Into<String>) -> Self {
        self.config.default_provider = Some(provider.into());
        self
    }

    pub fn with_provider(mut self, name: impl Into<String>, config: ProviderConfig) -> Self {
        self.config.providers.insert(name.into(), config);
        self
    }

    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub async fn completion(&self, mut req: ChatRequest) -> Result<ChatResponse> {
        let resolved = resolve_model(&req.model, &self.config)?;
        req.model = resolved.model.clone();
        dispatch_chat(&self.client, resolved, req).await
    }

    pub async fn stream_completion(&self, mut req: ChatRequest) -> Result<ChatStream> {
        let resolved = resolve_model(&req.model, &self.config)?;
        req.model = resolved.model.clone();
        match resolved.config.kind {
            ProviderKind::OpenAICompatible => {
                openai_compat::chat_stream(&self.client, &resolved.config, req).await
            }
            ProviderKind::Anthropic => {
                anthropic::chat_stream(&self.client, &resolved.config, req).await
            }
            _ => Err(LiteLLMError::Unsupported(
                "streaming not supported for provider".into(),
            )),
        }
    }

    pub async fn embedding(&self, mut req: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let resolved = resolve_model(&req.model, &self.config)?;
        req.model = resolved.model.clone();
        match resolved.config.kind {
            ProviderKind::OpenAICompatible => {
                openai_compat::embeddings(&self.client, &resolved.config, req).await
            }
            _ => Err(LiteLLMError::Unsupported(
                "embeddings not supported for provider".into(),
            )),
        }
    }

    pub async fn image_generation(&self, mut req: ImageRequest) -> Result<ImageResponse> {
        let resolved = resolve_model(&req.model, &self.config)?;
        req.model = resolved.model.clone();
        match resolved.config.kind {
            ProviderKind::OpenAICompatible => {
                openai_compat::image_generation(&self.client, &resolved.config, req).await
            }
            _ => Err(LiteLLMError::Unsupported(
                "image generation not supported for provider".into(),
            )),
        }
    }

    pub async fn video_generation(&self, mut req: VideoRequest) -> Result<VideoResponse> {
        let resolved = resolve_model(&req.model, &self.config)?;
        req.model = resolved.model.clone();
        match resolved.config.kind {
            ProviderKind::OpenAICompatible => {
                openai_compat::video_generation(&self.client, &resolved.config, req).await
            }
            ProviderKind::Gemini => {
                gemini::video_generation(&self.client, &resolved.config, req).await
            }
            _ => Err(LiteLLMError::Unsupported(
                "video generation not supported for provider".into(),
            )),
        }
    }

    pub fn estimate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> Option<f64> {
        self.registry
            .estimate_cost(model, input_tokens, output_tokens)
    }
}

async fn dispatch_chat(
    client: &Client,
    resolved: ResolvedModel,
    req: ChatRequest,
) -> Result<ChatResponse> {
    match resolved.config.kind {
        ProviderKind::OpenAICompatible => openai_compat::chat(client, &resolved.config, req).await,
        ProviderKind::Anthropic => anthropic::chat(client, &resolved.config, req).await,
        ProviderKind::Gemini => gemini::chat(client, &resolved.config, req).await,
    }
}

```

File: crates/litellm-rust/src/lib.rs (78 tokens)
```
pub mod client;
pub mod config;
pub mod error;
pub mod providers;
pub mod registry;
pub mod router;
pub mod stream;
pub mod types;

pub use client::LiteLLM;
pub use config::{Config, ProviderConfig, ProviderKind};
pub use error::{LiteLLMError, Result};
pub use stream::{ChatStream, ChatStreamChunk};
pub use types::*;

```

File: crates/litellm-rust/src/stream.rs (861 tokens)
```
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

pub fn parse_anthropic_sse_stream<S>(stream: S) -> ChatStream
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let s = async_stream::try_stream! {
        let mut buffer = String::new();
        let mut event_name: Option<String> = None;
        let mut data_buf = String::new();
        futures_util::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| LiteLLMError::Http(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let mut line = buffer[..pos].to_string();
                buffer = buffer[pos + 1..].to_string();
                line = line.trim_end().to_string();
                if line.is_empty() {
                    if !data_buf.is_empty() {
                        let data = data_buf.trim().to_string();
                        data_buf.clear();
                        let event = event_name.take().unwrap_or_default();
                        if data == "[DONE]" {
                            return;
                        }
                        let value: Value = serde_json::from_str(&data)
                            .map_err(|e| LiteLLMError::Parse(e.to_string()))?;
                        if event == "content_block_delta" {
                            let content = value
                                .pointer("/delta/text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !content.is_empty() {
                                yield ChatStreamChunk {
                                    content,
                                    raw: Some(value),
                                };
                            }
                        }
                    }
                    continue;
                }
                if let Some(rest) = line.strip_prefix("event:") {
                    event_name = Some(rest.trim().to_string());
                    continue;
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    if !data_buf.is_empty() {
                        data_buf.push('\n');
                    }
                    data_buf.push_str(rest.trim());
                }
            }
        }
    };
    Box::pin(s)
}

```

File: crates/litellm-rust/src/types.rs (1538 tokens)
```
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatMessageContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

impl ChatMessageContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatContentPart {
    Text(ChatContentPartText),
    ImageUrl(ChatContentPartImageUrl),
    InputAudio(ChatContentPartInputAudio),
    File(ChatContentPartFile),
    Other(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatContentPartText {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatContentPartImageUrl {
    #[serde(rename = "type")]
    pub kind: String,
    pub image_url: ChatImageUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatImageUrl {
    Url(String),
    Object(ChatImageUrlObject),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatImageUrlObject {
    pub url: String,
    pub detail: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatContentPartInputAudio {
    #[serde(rename = "type")]
    pub kind: String,
    pub input_audio: ChatInputAudio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInputAudio {
    pub data: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatContentPartFile {
    #[serde(rename = "type")]
    pub kind: String,
    pub file: ChatFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatFile {
    pub file_id: Option<String>,
    pub file_data: Option<String>,
    pub format: Option<String>,
    pub detail: Option<String>,
    pub video_metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: ChatMessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_specific_fields: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub response_format: Option<Value>,
    pub max_completion_tokens: Option<u32>,
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    pub parallel_tool_calls: Option<bool>,
    pub stop: Option<Value>,
    pub top_p: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub seed: Option<u64>,
    pub user: Option<String>,
    pub metadata: Option<Value>,
    pub reasoning_effort: Option<Value>,
    pub thinking: Option<Value>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            temperature: None,
            max_tokens: None,
            response_format: None,
            max_completion_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            stop: None,
            top_p: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            user: None,
            metadata: None,
            reasoning_effort: None,
            thinking: None,
        }
    }

    pub fn message(mut self, role: impl Into<String>, content: impl Into<String>) -> Self {
        self.messages.push(ChatMessage {
            role: role.into(),
            content: ChatMessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            function_call: None,
            provider_specific_fields: None,
        });
        self
    }

    pub fn message_with_content(
        mut self,
        role: impl Into<String>,
        content: ChatMessageContent,
    ) -> Self {
        self.messages.push(ChatMessage {
            role: role.into(),
            content,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            function_call: None,
            provider_specific_fields: None,
        });
        self
    }

    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn response_format(mut self, format: Value) -> Self {
        self.response_format = Some(format);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub usage: Usage,
    pub response_id: Option<String>,
    pub header_cost: Option<f64>,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub thoughts_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub vectors: Vec<Vec<f32>>,
    pub usage: Usage,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub n: Option<u32>,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub background: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    pub b64_json: Option<String>,
    pub url: Option<String>,
    pub revised_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResponse {
    pub images: Vec<ImageData>,
    pub usage: Usage,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoRequest {
    pub model: String,
    pub prompt: String,
    pub seconds: Option<u32>,
    pub size: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoResponse {
    pub video_url: Option<String>,
    pub raw: Option<Value>,
}

```

File: crates/yoetz-cli/Cargo.toml (263 tokens)
```
[package]
name = "yoetz"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
description.workspace = true
keywords.workspace = true
categories.workspace = true
authors.workspace = true

[[bin]]
name = "yoetz"
path = "src/main.rs"

[lints]
workspace = true

[dependencies]
yoetz-core = { path = "../yoetz-core" }
litellm-rust = { path = "../litellm-rust" }

anyhow.workspace = true
dotenvy.workspace = true
serde.workspace = true
serde_json.workspace = true
base64.workspace = true
time.workspace = true

clap = { version = "4.5", features = ["derive"] }
reqwest = { version = "0.12", default-features = false, features = [
    "json",
    "rustls-tls",
    "multipart",
] }
tokio = { version = "1.36", features = ["rt-multi-thread", "macros", "time"] }
serde_yaml = "0.9"
tempfile = "3.10"
jsonschema = "0.17"
fs2 = "0.4"

```

File: crates/yoetz-cli/src/browser.rs (1516 tokens)
```
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::process::Command;
use std::thread;
use std::time::Duration;

use yoetz_core::output::{write_jsonl_event, OutputFormat};

#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub name: Option<String>,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Deserialize)]
pub struct RecipeStep {
    pub action: Option<String>,
    pub args: Option<Vec<String>>,
    pub sleep_ms: Option<u64>,
}

pub struct RecipeContext {
    pub bundle_path: Option<String>,
    pub bundle_text: Option<String>,
}

pub fn agent_browser_bin() -> String {
    env::var("YOETZ_AGENT_BROWSER_BIN").unwrap_or_else(|_| "agent-browser".to_string())
}

pub fn run_agent_browser(args: Vec<String>, format: OutputFormat) -> Result<String> {
    let mut cmd = Command::new(agent_browser_bin());
    let mut final_args = args;

    let wants_json = matches!(format, OutputFormat::Json | OutputFormat::Jsonl);
    if wants_json && !final_args.iter().any(|a| a == "--json") {
        final_args.push("--json".to_string());
    }

    let output = cmd
        .args(&final_args)
        .output()
        .with_context(|| "failed to run agent-browser")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("agent-browser failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn run_recipe(recipe: Recipe, ctx: RecipeContext, format: OutputFormat) -> Result<()> {
    if matches!(format, OutputFormat::Jsonl) {
        if let Some(name) = recipe.name.as_deref() {
            let event = json!({
                "type": "recipe_start",
                "name": name,
            });
            write_jsonl_event(&event)?;
        }
    }

    for (idx, step) in recipe.steps.iter().enumerate() {
        if let Some(ms) = step.sleep_ms {
            thread::sleep(Duration::from_millis(ms));
            continue;
        }

        let action = step
            .action
            .as_ref()
            .ok_or_else(|| anyhow!("recipe step {idx} missing action"))?;
        let commands = expand_step(action, step.args.as_deref(), &ctx)?;

        for args in commands {
            let stdout = run_agent_browser(args.clone(), format)?;

            if matches!(format, OutputFormat::Jsonl) {
                let stdout_value = if matches!(format, OutputFormat::Json | OutputFormat::Jsonl) {
                    parse_stdout_json(&stdout).unwrap_or(Value::String(stdout.clone()))
                } else {
                    Value::String(stdout.clone())
                };
                let event = json!({
                    "type": "browser_step",
                    "index": idx,
                    "action": action,
                    "args": step.args,
                    "stdout": stdout_value,
                });
                write_jsonl_event(&event)?;
            } else {
                print!("{stdout}");
            }
        }
    }

    Ok(())
}

fn parse_stdout_json(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn expand_step(
    action: &str,
    args: Option<&[String]>,
    ctx: &RecipeContext,
) -> Result<Vec<Vec<String>>> {
    let args = args.unwrap_or_default();
    if args.iter().any(|s| s.contains("{{bundle_text}}")) {
        let text = ctx
            .bundle_text
            .as_deref()
            .ok_or_else(|| anyhow!("bundle text requested but no bundle provided"))?;
        return expand_bundle_text_step(action, args, text, ctx);
    }

    let mut command = vec![action.to_string()];
    for arg in args {
        command.push(interpolate(arg, ctx, None));
    }
    Ok(vec![command])
}

fn expand_bundle_text_step(
    action: &str,
    args: &[String],
    text: &str,
    ctx: &RecipeContext,
) -> Result<Vec<Vec<String>>> {
    const CHUNK_BYTES: usize = 4000;
    let chunks = chunk_text(text, CHUNK_BYTES);
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    if action == "find" {
        if args.len() < 4 {
            return Err(anyhow!(
                "find step requires locator, value, action, and text"
            ));
        }
        let locator = interpolate(&args[0], ctx, None);
        let value = interpolate(&args[1], ctx, None);
        let mut commands = Vec::new();

        let first = vec![
            action.to_string(),
            locator.clone(),
            value.clone(),
            "fill".to_string(),
            chunks[0].clone(),
        ];
        commands.push(first);

        for chunk in chunks.iter().skip(1) {
            commands.push(vec![
                action.to_string(),
                locator.clone(),
                value.clone(),
                "type".to_string(),
                chunk.clone(),
            ]);
        }

        return Ok(commands);
    }

    if action == "fill" || action == "type" {
        if args.len() < 2 {
            return Err(anyhow!("{action} step requires selector and text"));
        }
        let selector = interpolate(&args[0], ctx, None);
        let mut commands = Vec::new();
        commands.push(vec![
            action.to_string(),
            selector.clone(),
            chunks[0].clone(),
        ]);
        for chunk in chunks.iter().skip(1) {
            commands.push(vec!["type".to_string(), selector.clone(), chunk.clone()]);
        }
        return Ok(commands);
    }

    let mut command = vec![action.to_string()];
    for arg in args {
        command.push(interpolate(arg, ctx, Some(text)));
    }
    Ok(vec![command])
}

fn chunk_text(text: &str, max_bytes: usize) -> Vec<String> {
    if text.is_empty() || max_bytes == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = (start + max_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        chunks.push(text[start..end].to_string());
        start = end;
    }
    chunks
}

fn interpolate(value: &str, ctx: &RecipeContext, bundle_text: Option<&str>) -> String {
    let mut out = value.to_string();
    if let Some(path) = &ctx.bundle_path {
        out = out.replace("{{bundle_path}}", path);
    }
    if let Some(text) = bundle_text {
        out = out.replace("{{bundle_text}}", text);
    }
    out
}

```

File: crates/yoetz-cli/src/budget.rs (1017 tokens)
```
use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetLedger {
    pub date: String,
    pub spent_usd: f64,
}

impl Default for BudgetLedger {
    fn default() -> Self {
        Self {
            date: today_utc(),
            spent_usd: 0.0,
        }
    }
}

pub fn budget_path() -> PathBuf {
    if let Ok(path) = env::var("YOETZ_BUDGET_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".yoetz/budget.json");
    }
    PathBuf::from(".yoetz/budget.json")
}

fn budget_lock_path() -> PathBuf {
    let mut path = budget_path();
    let lock_name = match path.file_name() {
        Some(name) => format!("{}.lock", name.to_string_lossy()),
        None => "budget.json.lock".to_string(),
    };
    path.set_file_name(lock_name);
    path
}

fn acquire_budget_lock() -> Result<std::fs::File> {
    let lock_path = budget_lock_path();
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;
    file.lock_exclusive()
        .with_context(|| format!("lock budget {}", lock_path.display()))?;
    Ok(file)
}

pub fn load_ledger() -> Result<BudgetLedger> {
    let _lock = acquire_budget_lock()?;
    let path = budget_path();
    if !path.exists() {
        return Ok(BudgetLedger::default());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("read budget {}", path.display()))?;
    let mut ledger: BudgetLedger = serde_json::from_str(&content)?;
    let today = today_utc();
    if ledger.date != today {
        ledger.date = today;
        ledger.spent_usd = 0.0;
    }
    Ok(ledger)
}

pub fn save_ledger(ledger: &BudgetLedger) -> Result<()> {
    let _lock = acquire_budget_lock()?;
    let path = budget_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(ledger)?;
    let mut tmp =
        NamedTempFile::new_in(path.parent().unwrap_or_else(|| std::path::Path::new(".")))?;
    tmp.write_all(data.as_bytes())?;
    tmp.persist(&path)
        .map_err(|e| anyhow!("write budget {}: {}", path.display(), e))?;
    Ok(())
}

pub fn ensure_budget(
    estimate_usd: Option<f64>,
    max_cost_usd: Option<f64>,
    daily_budget_usd: Option<f64>,
) -> Result<BudgetLedger> {
    if let Some(max) = max_cost_usd {
        let Some(estimate) = estimate_usd else {
            return Err(anyhow!(
                "cost estimate unavailable; cannot enforce max-cost"
            ));
        };
        if estimate > max {
            return Err(anyhow!(
                "estimated cost ${estimate:.6} exceeds max ${max:.6}"
            ));
        }
    }

    let ledger = load_ledger()?;
    if let Some(limit) = daily_budget_usd {
        let Some(estimate) = estimate_usd else {
            return Err(anyhow!(
                "cost estimate unavailable; cannot enforce daily budget"
            ));
        };
        if ledger.spent_usd + estimate > limit {
            return Err(anyhow!(
                "daily budget exceeded: ${:.6} + ${:.6} > ${:.6}",
                ledger.spent_usd,
                estimate,
                limit
            ));
        }
    }

    Ok(ledger)
}

pub fn record_spend(mut ledger: BudgetLedger, spend_usd: f64) -> Result<()> {
    ledger.spent_usd += spend_usd;
    save_ledger(&ledger)
}

fn today_utc() -> String {
    OffsetDateTime::now_utc().date().to_string()
}

#[allow(dead_code)]
fn timestamp_utc() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

```

File: crates/yoetz-cli/src/commands/apply.rs (255 tokens)
```
use anyhow::{anyhow, Result};
use std::io::{self, Read};
use std::process::Command;

use crate::ApplyArgs;

pub(crate) fn handle_apply(args: ApplyArgs) -> Result<()> {
    let patch = if let Some(path) = args.patch_file {
        std::fs::read_to_string(path)?
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };

    if patch.trim().is_empty() {
        return Err(anyhow!("patch is empty"));
    }

    let mut tmp = tempfile::NamedTempFile::new()?;
    use std::io::Write;
    tmp.write_all(patch.as_bytes())?;

    let mut cmd = Command::new("git");
    cmd.arg("apply");
    if args.check {
        cmd.arg("--check");
    }
    if args.reverse {
        cmd.arg("--reverse");
    }
    cmd.arg(tmp.path());

    let status = cmd.status()?;
    if !status.success() {
        return Err(anyhow!("git apply failed"));
    }

    if args.check {
        println!("Patch OK");
    } else {
        println!("Patch applied");
    }
    Ok(())
}

```

File: crates/yoetz-cli/src/commands/ask.rs (1929 tokens)
```
use anyhow::{anyhow, Result};

use crate::providers::{gemini, openai};
use crate::{
    apply_capability_warnings, call_litellm, maybe_write_output, parse_media_input,
    parse_media_inputs, resolve_max_output_tokens, resolve_prompt, resolve_registry_model_id,
    resolve_response_format, AppContext, AskArgs,
};
use crate::{budget, providers, registry};
use std::env;
use std::path::PathBuf;
use yoetz_core::bundle::{build_bundle, estimate_tokens, BundleOptions};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, PricingEstimate, RunResult, Usage};

pub(crate) async fn handle_ask(
    ctx: &AppContext,
    args: AskArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt.clone(), args.prompt_file.clone())?;
    let config = &ctx.config;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;

    let image_inputs = parse_media_inputs(&args.image)?;
    let video_input = match args.video.as_deref() {
        Some(value) => Some(parse_media_input(value)?),
        None => None,
    };

    let include_files = args.files.clone();
    let exclude_files = args.exclude.clone();

    let bundle = if include_files.is_empty() {
        None
    } else {
        let options = BundleOptions {
            include: include_files,
            exclude: exclude_files,
            max_file_bytes: args.max_file_bytes,
            max_total_bytes: args.max_total_bytes,
            ..Default::default()
        };
        Some(build_bundle(&prompt, options)?)
    };

    let session = create_session_dir()?;
    let mut artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };

    if let Some(bundle_ref) = &bundle {
        let bundle_json = session.path.join("bundle.json");
        let bundle_md = session.path.join("bundle.md");
        write_json_file(&bundle_json, bundle_ref)?;
        write_text(&bundle_md, &crate::render_bundle_md(bundle_ref))?;
        artifacts.bundle_json = Some(bundle_json.to_string_lossy().to_string());
        artifacts.bundle_md = Some(bundle_md.to_string_lossy().to_string());
    }

    let model_id = args.model.clone().or(config.defaults.model.clone());
    let provider_id = args.provider.clone().or(config.defaults.provider.clone());
    let registry_cache = registry::load_registry_cache().ok().flatten();
    let registry_model_id = resolve_registry_model_id(
        provider_id.as_deref(),
        model_id.as_deref(),
        registry_cache.as_ref(),
    );
    let max_output_tokens = resolve_max_output_tokens(args.max_output_tokens, config);
    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    let output_tokens = max_output_tokens;
    let mut pricing = if let Some(model_id) = registry_model_id.as_deref() {
        registry::estimate_pricing(
            registry_cache.as_ref(),
            model_id,
            input_tokens,
            output_tokens,
        )?
    } else {
        PricingEstimate::default()
    };

    apply_capability_warnings(
        registry_cache.as_ref(),
        registry_model_id.as_deref(),
        !image_inputs.is_empty(),
        video_input.is_some(),
        &mut pricing,
    )?;

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let model_prompt = if let Some(bundle_ref) = &bundle {
        crate::render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    };

    let (content, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else if !image_inputs.is_empty() || video_input.is_some() {
        let provider = provider_id
            .as_deref()
            .ok_or_else(|| anyhow!("provider is required"))?;
        let model = model_id
            .as_deref()
            .ok_or_else(|| anyhow!("model is required"))?;
        if video_input.is_some() && provider != "gemini" {
            return Err(anyhow!(
                "video inputs are only supported for provider gemini"
            ));
        }
        match provider {
            "openai" => {
                if video_input.is_some() {
                    return Err(anyhow!("openai provider does not support video inputs"));
                }
                let auth = providers::resolve_provider_auth(config, provider)?;
                let result = openai::call_responses_vision(
                    &ctx.client,
                    &auth,
                    &model_prompt,
                    model,
                    &image_inputs,
                    response_format.clone(),
                    args.temperature,
                    max_output_tokens,
                )
                .await?;
                (result.content, result.usage, result.response_id, None)
            }
            "gemini" => {
                let auth = providers::resolve_provider_auth(config, provider)?;
                let result = gemini::generate_content(
                    &ctx.client,
                    &auth,
                    &model_prompt,
                    model,
                    &image_inputs,
                    video_input.as_ref(),
                    args.temperature,
                    max_output_tokens,
                )
                .await?;
                if ctx.debug || env::var("YOETZ_GEMINI_DEBUG").ok().as_deref() == Some("1") {
                    let raw_path = session.path.join("gemini_response.json");
                    let _ = write_json_file(&raw_path, &result.raw);
                }
                (result.content, result.usage, None, None)
            }
            _ => {
                let call = call_litellm(
                    &ctx.litellm,
                    Some(provider),
                    model,
                    &model_prompt,
                    args.temperature,
                    max_output_tokens,
                    response_format.clone(),
                    &image_inputs,
                    video_input.as_ref(),
                )
                .await?;
                (call.content, call.usage, call.response_id, call.header_cost)
            }
        }
    } else {
        let provider = provider_id
            .as_deref()
            .ok_or_else(|| anyhow!("provider is required"))?;
        let model = model_id
            .as_deref()
            .ok_or_else(|| anyhow!("model is required"))?;
        let result = call_litellm(
            &ctx.litellm,
            Some(provider),
            model,
            &model_prompt,
            args.temperature,
            max_output_tokens,
            response_format.clone(),
            &[],
            None,
        )
        .await?;
        (
            result.content,
            result.usage,
            result.response_id,
            result.header_cost,
        )
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }

    if usage.cost_usd.is_none() {
        if let Some(provider) = provider_id.as_deref() {
            if provider == "openrouter" {
                if let Some(id) = response_id.as_deref() {
                    if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                        usage.cost_usd = cost;
                    }
                }
            }
        }
    }

    if provider_id.as_deref() == Some("gemini") && content.trim().is_empty() {
        if let Some(thoughts) = usage.thoughts_tokens.filter(|t| *t > 0) {
            eprintln!(
                "warning: gemini returned empty content but used {thoughts} thought tokens; try increasing --max-output-tokens"
            );
        }
    }

    if let Some(ledger) = ledger {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let mut result = RunResult {
        id: session.id,
        model: model_id,
        provider: provider_id,
        bundle,
        pricing,
        usage,
        content,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("response.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("ask", &result),
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
        }
    }
}

```

File: crates/yoetz-cli/src/commands/bundle.rs (460 tokens)
```
use anyhow::{anyhow, Result};

use crate::{maybe_write_output, render_bundle_md, resolve_prompt, AppContext, BundleArgs};
use yoetz_core::bundle::{build_bundle, BundleOptions};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, BundleResult};

pub(crate) fn handle_bundle(
    ctx: &AppContext,
    args: BundleArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    if args.files.is_empty() && !args.all {
        return Err(anyhow!("--files is required unless --all is set"));
    }
    let options = BundleOptions {
        include: args.files,
        exclude: args.exclude,
        max_file_bytes: args.max_file_bytes,
        max_total_bytes: args.max_total_bytes,
        ..Default::default()
    };

    let bundle = build_bundle(&prompt, options)?;
    let session = create_session_dir()?;

    let bundle_json = session.path.join("bundle.json");
    let bundle_md = session.path.join("bundle.md");

    write_json_file(&bundle_json, &bundle)?;
    write_text(&bundle_md, &render_bundle_md(&bundle))?;

    let result = BundleResult {
        id: session.id,
        bundle,
        artifacts: ArtifactPaths {
            session_dir: session.path.to_string_lossy().to_string(),
            bundle_json: Some(bundle_json.to_string_lossy().to_string()),
            bundle_md: Some(bundle_md.to_string_lossy().to_string()),
            response_json: None,
            media_dir: None,
        },
    };

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("bundle", &result),
        OutputFormat::Text => {
            println!("Bundle created at {}", result.artifacts.session_dir);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("Bundle created at `{}`", result.artifacts.session_dir);
            Ok(())
        }
    }
}

```

File: crates/yoetz-cli/src/commands/council.rs (1877 tokens)
```
use anyhow::{anyhow, Result};

use crate::CouncilResult;
use crate::{
    add_usage, call_litellm, maybe_write_output, render_bundle_md, resolve_max_output_tokens,
    resolve_prompt, resolve_registry_model_id, resolve_response_format, AppContext, CouncilArgs,
    CouncilModelResult, CouncilPricing, ModelEstimate,
};
use crate::{budget, registry};
use std::path::PathBuf;
use yoetz_core::bundle::{build_bundle, estimate_tokens, BundleOptions};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, Usage};

pub(crate) async fn handle_council(
    ctx: &AppContext,
    args: CouncilArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt.clone(), args.prompt_file.clone())?;
    let config = &ctx.config;

    if args.models.is_empty() {
        return Err(anyhow!("at least one model is required"));
    }

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;

    let include_files = args.files.clone();
    let exclude_files = args.exclude.clone();

    let bundle = if include_files.is_empty() {
        None
    } else {
        let options = BundleOptions {
            include: include_files,
            exclude: exclude_files,
            max_file_bytes: args.max_file_bytes,
            max_total_bytes: args.max_total_bytes,
            ..Default::default()
        };
        Some(build_bundle(&prompt, options)?)
    };

    let registry_cache = registry::load_registry_cache().ok().flatten();
    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    let max_output_tokens = resolve_max_output_tokens(args.max_output_tokens, config);
    let output_tokens = max_output_tokens;

    let mut per_model = Vec::new();
    let mut estimate_sum = 0.0;
    let mut estimate_complete = true;
    for model in &args.models {
        let registry_id =
            resolve_registry_model_id(Some(&provider), Some(model), registry_cache.as_ref());
        let estimate = registry::estimate_pricing(
            registry_cache.as_ref(),
            registry_id.as_deref().unwrap_or(model),
            input_tokens,
            output_tokens,
        )?;
        if let Some(cost) = estimate.estimate_usd {
            estimate_sum += cost;
        } else {
            estimate_complete = false;
        }
        per_model.push(ModelEstimate {
            model: model.clone(),
            estimate_usd: estimate.estimate_usd,
        });
    }
    let total_estimate = if estimate_complete {
        Some(estimate_sum)
    } else {
        None
    };

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            total_estimate,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let session = create_session_dir()?;
    let mut artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };

    if let Some(bundle_ref) = &bundle {
        let bundle_json = session.path.join("bundle.json");
        let bundle_md = session.path.join("bundle.md");
        write_json_file(&bundle_json, bundle_ref)?;
        write_text(&bundle_md, &render_bundle_md(bundle_ref))?;
        artifacts.bundle_json = Some(bundle_json.to_string_lossy().to_string());
        artifacts.bundle_md = Some(bundle_md.to_string_lossy().to_string());
    }

    let mut results = Vec::new();
    let mut total_usage = Usage::default();
    let model_prompt = std::sync::Arc::new(if let Some(bundle_ref) = &bundle {
        render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    });

    if args.dry_run {
        for model in &args.models {
            let registry_id =
                resolve_registry_model_id(Some(&provider), Some(model), registry_cache.as_ref());
            results.push(CouncilModelResult {
                model: model.clone(),
                content: "(dry-run) no provider call executed".to_string(),
                usage: Usage::default(),
                pricing: registry::estimate_pricing(
                    registry_cache.as_ref(),
                    registry_id.as_deref().unwrap_or(model),
                    input_tokens,
                    output_tokens,
                )?,
                response_id: None,
            });
        }
    } else {
        let max_parallel = args.max_parallel.max(1);
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_parallel));
        let mut join_set = tokio::task::JoinSet::new();
        for (idx, model) in args.models.iter().cloned().enumerate() {
            let prompt = std::sync::Arc::clone(&model_prompt);
            let provider = provider.clone();
            let litellm = ctx.litellm.clone();
            let semaphore = std::sync::Arc::clone(&semaphore);
            let temperature = args.temperature;
            let response_format = response_format.clone();
            join_set.spawn(async move {
                let _permit = semaphore.acquire_owned().await?;
                let call = call_litellm(
                    &litellm,
                    Some(&provider),
                    &model,
                    prompt.as_str(),
                    temperature,
                    max_output_tokens,
                    response_format,
                    &[],
                    None,
                )
                .await?;
                Ok::<_, anyhow::Error>((idx, model, call))
            });
        }

        let mut ordered: Vec<Option<CouncilModelResult>> =
            (0..args.models.len()).map(|_| None).collect();
        while let Some(res) = join_set.join_next().await {
            let (idx, model, call) = res??;
            let mut usage = call.usage;
            if usage.cost_usd.is_none() {
                usage.cost_usd = call.header_cost;
            }
            if usage.cost_usd.is_none() && provider == "openrouter" {
                if let Some(id) = call.response_id.as_deref() {
                    if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                        usage.cost_usd = cost;
                    }
                }
            }

            total_usage = add_usage(total_usage, &usage);

            let registry_id =
                resolve_registry_model_id(Some(&provider), Some(&model), registry_cache.as_ref());
            let pricing = registry::estimate_pricing(
                registry_cache.as_ref(),
                registry_id.as_deref().unwrap_or(&model),
                input_tokens,
                output_tokens,
            )?;

            ordered[idx] = Some(CouncilModelResult {
                model,
                content: call.content,
                usage,
                pricing,
                response_id: call.response_id,
            });
        }

        results = ordered.into_iter().flatten().collect();
    }

    if let Some(ledger) = ledger {
        let mut spend = 0.0;
        let mut has_spend = false;
        for r in &results {
            if let Some(cost) = r.usage.cost_usd.or(r.pricing.estimate_usd) {
                spend += cost;
                has_spend = true;
            }
        }
        if has_spend {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let mut council = CouncilResult {
        id: session.id,
        provider,
        bundle,
        results,
        pricing: CouncilPricing {
            estimate_usd_total: total_estimate,
            per_model,
        },
        usage: total_usage,
        artifacts,
    };

    let response_json = PathBuf::from(&council.artifacts.session_dir).join("council.json");
    council.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &council)?;

    maybe_write_output(ctx, &council)?;

    match format {
        OutputFormat::Json => write_json(&council),
        OutputFormat::Jsonl => write_jsonl("council", &council),
        OutputFormat::Text => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            Ok(())
        }
    }
}

```

File: crates/yoetz-cli/src/commands/generate.rs (1582 tokens)
```
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::providers::{gemini, openai, resolve_provider_auth};
use crate::{
    build_model_spec, maybe_write_output, parse_media_inputs, resolve_prompt, usage_from_litellm,
    AppContext, GenerateArgs, GenerateCommand, GenerateImageArgs, GenerateVideoArgs,
};
use litellm_rust::ImageRequest;
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file};
use yoetz_core::types::{ArtifactPaths, MediaGenerationResult, Usage};

pub(crate) async fn handle_generate(
    ctx: &AppContext,
    args: GenerateArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        GenerateCommand::Image(args) => handle_generate_image(ctx, args, format).await,
        GenerateCommand::Video(args) => handle_generate_video(ctx, args, format).await,
    }
}

async fn handle_generate_image(
    ctx: &AppContext,
    args: GenerateImageArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    let config = &ctx.config;

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .ok_or_else(|| anyhow!("model is required"))?;

    let images = parse_media_inputs(&args.image)?;

    let session = create_session_dir()?;
    let media_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| session.path.join("media"));
    fs::create_dir_all(&media_dir).with_context(|| format!("create {}", media_dir.display()))?;

    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        media_dir: Some(media_dir.to_string_lossy().to_string()),
        ..Default::default()
    };

    let (outputs, usage) = if args.dry_run {
        (Vec::new(), Usage::default())
    } else if !images.is_empty() {
        match provider.as_str() {
            "openai" => {
                let auth = resolve_provider_auth(config, &provider)?;
                let result = openai::generate_images(
                    &ctx.client,
                    &auth,
                    &prompt,
                    &model,
                    &images,
                    args.size.as_deref(),
                    args.quality.as_deref(),
                    args.background.as_deref(),
                    args.n,
                    &media_dir,
                )
                .await?;
                (result.outputs, result.usage)
            }
            _ => {
                return Err(anyhow!(
                    "provider {provider} does not support image edits yet"
                ))
            }
        }
    } else {
        let model_spec = build_model_spec(Some(&provider), &model)?;
        let resp = ctx
            .litellm
            .image_generation(ImageRequest {
                model: model_spec,
                prompt: prompt.clone(),
                n: Some(args.n as u32),
                size: args.size.clone(),
                quality: args.quality.clone(),
                background: args.background.clone(),
            })
            .await?;
        let outputs =
            crate::save_image_outputs(&ctx.client, resp.images, &media_dir, &model).await?;
        (outputs, usage_from_litellm(resp.usage))
    };

    let mut result = MediaGenerationResult {
        id: session.id,
        provider: Some(provider),
        model: Some(model),
        prompt,
        usage,
        artifacts: artifacts.clone(),
        outputs,
    };

    let response_json = PathBuf::from(&artifacts.session_dir).join("response.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("generate.image", &result),
        OutputFormat::Text | OutputFormat::Markdown => {
            for output in &result.outputs {
                println!("{}", output.path.display());
            }
            Ok(())
        }
    }
}

async fn handle_generate_video(
    ctx: &AppContext,
    args: GenerateVideoArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    let config = &ctx.config;

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .ok_or_else(|| anyhow!("model is required"))?;

    let images = parse_media_inputs(&args.image)?;

    let session = create_session_dir()?;
    let media_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| session.path.join("media"));
    fs::create_dir_all(&media_dir).with_context(|| format!("create {}", media_dir.display()))?;

    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        media_dir: Some(media_dir.to_string_lossy().to_string()),
        ..Default::default()
    };

    let output_path = media_dir.join("video.mp4");

    let outputs = if args.dry_run {
        Vec::new()
    } else {
        let output = match provider.as_str() {
            "openai" => {
                let auth = resolve_provider_auth(config, &provider)?;
                openai::generate_video_sora(
                    &ctx.client,
                    &auth,
                    &prompt,
                    &model,
                    args.duration_secs,
                    args.size.as_deref(),
                    images.first(),
                    &output_path,
                )
                .await?
            }
            "gemini" => {
                let auth = resolve_provider_auth(config, &provider)?;
                gemini::generate_video_veo(
                    &ctx.client,
                    &auth,
                    &prompt,
                    &model,
                    &images,
                    args.duration_secs,
                    args.aspect_ratio.as_deref(),
                    args.resolution.as_deref(),
                    args.negative_prompt.as_deref(),
                    &output_path,
                )
                .await?
            }
            _ => {
                return Err(anyhow!(
                    "provider {provider} does not support video generation yet"
                ))
            }
        };
        vec![output]
    };

    let mut result = MediaGenerationResult {
        id: session.id,
        provider: Some(provider),
        model: Some(model),
        prompt,
        usage: Usage::default(),
        artifacts: artifacts.clone(),
        outputs,
    };

    let response_json = PathBuf::from(&artifacts.session_dir).join("response.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("generate.video", &result),
        OutputFormat::Text | OutputFormat::Markdown => {
            for output in &result.outputs {
                println!("{}", output.path.display());
            }
            Ok(())
        }
    }
}

```

File: crates/yoetz-cli/src/commands/mod.rs (48 tokens)
```
pub(crate) mod apply;
pub(crate) mod ask;
pub(crate) mod bundle;
pub(crate) mod council;
pub(crate) mod generate;
pub(crate) mod models;
pub(crate) mod pricing;
pub(crate) mod review;

```

File: crates/yoetz-cli/src/commands/models.rs (375 tokens)
```
use anyhow::Result;

use crate::{maybe_write_output, registry, AppContext, ModelsArgs, ModelsCommand};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};

pub(crate) async fn handle_models(
    ctx: &AppContext,
    args: ModelsArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        ModelsCommand::List => {
            let registry = registry::load_registry_cache()?.unwrap_or_default();
            maybe_write_output(ctx, &registry)?;
            match format {
                OutputFormat::Json => write_json(&registry),
                OutputFormat::Jsonl => write_jsonl("models_list", &registry),
                OutputFormat::Text | OutputFormat::Markdown => {
                    for model in registry.models {
                        println!("{}", model.id);
                    }
                    Ok(())
                }
            }
        }
        ModelsCommand::Sync => {
            let fetch = registry::fetch_registry(&ctx.client, &ctx.config).await?;
            let path = registry::save_registry_cache(&fetch.registry)?;
            let payload = serde_json::json!({
                "saved_to": path,
                "model_count": fetch.registry.models.len(),
                "warnings": fetch.warnings,
            });
            maybe_write_output(ctx, &payload)?;
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("models_sync", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!(
                        "Saved {} models to {}",
                        fetch.registry.models.len(),
                        path.display()
                    );
                    if !fetch.warnings.is_empty() {
                        eprintln!("Warnings:");
                        for warning in &fetch.warnings {
                            eprintln!("- {warning}");
                        }
                    }
                    Ok(())
                }
            }
        }
    }
}

```

File: crates/yoetz-cli/src/commands/pricing.rs (275 tokens)
```
use anyhow::Result;

use crate::registry;
use crate::{
    maybe_write_output, resolve_registry_model_id, AppContext, PricingArgs, PricingCommand,
};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};

pub(crate) async fn handle_pricing(
    ctx: &AppContext,
    args: PricingArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        PricingCommand::Estimate(e) => {
            let registry = registry::load_registry_cache()?.unwrap_or_default();
            let registry_id = resolve_registry_model_id(None, Some(&e.model), Some(&registry));
            let estimate = registry::estimate_pricing(
                Some(&registry),
                registry_id.as_deref().unwrap_or(&e.model),
                e.input_tokens,
                e.output_tokens,
            )?;
            maybe_write_output(ctx, &estimate)?;
            match format {
                OutputFormat::Json => write_json(&estimate),
                OutputFormat::Jsonl => write_jsonl("pricing_estimate", &estimate),
                OutputFormat::Text | OutputFormat::Markdown => {
                    if let Some(cost) = estimate.estimate_usd {
                        println!("Estimated cost: ${:.6}", cost);
                    } else {
                        println!("Estimate unavailable");
                    }
                    Ok(())
                }
            }
        }
    }
}

```

File: crates/yoetz-cli/src/commands/review.rs (2010 tokens)
```
use anyhow::{anyhow, Result};

use crate::ReviewResult;
use crate::{budget, registry};
use crate::{
    build_review_diff_prompt, build_review_file_prompt, call_litellm, git_diff, maybe_write_output,
    read_text_file, resolve_max_output_tokens, resolve_registry_model_id, resolve_response_format,
    AppContext, ReviewArgs, ReviewCommand, ReviewDiffArgs, ReviewFileArgs,
};
use std::path::PathBuf;
use yoetz_core::bundle::estimate_tokens;
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, Usage};

pub(crate) async fn handle_review(
    ctx: &AppContext,
    args: ReviewArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        ReviewCommand::Diff(diff_args) => handle_review_diff(ctx, diff_args, format).await,
        ReviewCommand::File(file_args) => handle_review_file(ctx, file_args, format).await,
    }
}

async fn handle_review_diff(
    ctx: &AppContext,
    args: ReviewDiffArgs,
    format: OutputFormat,
) -> Result<()> {
    let config = &ctx.config;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;
    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .ok_or_else(|| anyhow!("model is required"))?;
    let max_output_tokens = resolve_max_output_tokens(args.max_output_tokens, config);

    let diff = git_diff(args.staged, &args.paths)?;
    if diff.trim().is_empty() {
        return Err(anyhow!("diff is empty"));
    }

    let review_prompt = build_review_diff_prompt(&diff, args.prompt.as_deref());
    let input_tokens = estimate_tokens(review_prompt.len());
    let registry_cache = registry::load_registry_cache().ok().flatten();
    let registry_id =
        resolve_registry_model_id(Some(&provider), Some(&model), registry_cache.as_ref());
    let pricing = registry::estimate_pricing(
        registry_cache.as_ref(),
        registry_id.as_deref().unwrap_or(&model),
        input_tokens,
        max_output_tokens,
    )?;

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let session = create_session_dir()?;
    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let review_input_path = session.path.join("review_input.txt");
    write_text(&review_input_path, &review_prompt)?;

    let (content, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let result = call_litellm(
            &ctx.litellm,
            Some(&provider),
            &model,
            &review_prompt,
            args.temperature,
            max_output_tokens,
            response_format.clone(),
            &[],
            None,
        )
        .await?;
        (
            result.content,
            result.usage,
            result.response_id,
            result.header_cost,
        )
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }
    if usage.cost_usd.is_none() && provider == "openrouter" {
        if let Some(id) = response_id.as_deref() {
            if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                usage.cost_usd = cost;
            }
        }
    }

    if let Some(ledger) = ledger {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let mut result = ReviewResult {
        id: session.id,
        provider,
        model,
        pricing,
        usage,
        content,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("review.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("review", &result),
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
        }
    }
}

async fn handle_review_file(
    ctx: &AppContext,
    args: ReviewFileArgs,
    format: OutputFormat,
) -> Result<()> {
    let config = &ctx.config;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;
    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .ok_or_else(|| anyhow!("model is required"))?;
    let max_output_tokens = resolve_max_output_tokens(args.max_output_tokens, config);

    let max_file_bytes = args.max_file_bytes.unwrap_or(200_000);
    let max_total_bytes = args.max_total_bytes.unwrap_or(max_file_bytes);
    let max_bytes = max_file_bytes.min(max_total_bytes);
    let (content, truncated) = read_text_file(args.path.as_path(), max_bytes)?;
    let review_prompt = build_review_file_prompt(
        args.path.as_path(),
        &content,
        truncated,
        args.prompt.as_deref(),
    );
    let input_tokens = estimate_tokens(review_prompt.len());
    let registry_cache = registry::load_registry_cache().ok().flatten();
    let registry_id =
        resolve_registry_model_id(Some(&provider), Some(&model), registry_cache.as_ref());
    let pricing = registry::estimate_pricing(
        registry_cache.as_ref(),
        registry_id.as_deref().unwrap_or(&model),
        input_tokens,
        max_output_tokens,
    )?;

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let session = create_session_dir()?;
    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let review_input_path = session.path.join("review_input.txt");
    write_text(&review_input_path, &review_prompt)?;

    let (output, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let result = call_litellm(
            &ctx.litellm,
            Some(&provider),
            &model,
            &review_prompt,
            args.temperature,
            max_output_tokens,
            response_format.clone(),
            &[],
            None,
        )
        .await?;
        (
            result.content,
            result.usage,
            result.response_id,
            result.header_cost,
        )
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }
    if usage.cost_usd.is_none() && provider == "openrouter" {
        if let Some(id) = response_id.as_deref() {
            if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                usage.cost_usd = cost;
            }
        }
    }

    if let Some(ledger) = ledger {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let mut result = ReviewResult {
        id: session.id,
        provider,
        model,
        pricing,
        usage,
        content: output,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("review.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("review", &result),
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
        }
    }
}

```

File: crates/yoetz-cli/src/http.rs (154 tokens)
```
use anyhow::{anyhow, Result};
use reqwest::{header::HeaderMap, RequestBuilder};
use serde::de::DeserializeOwned;

pub async fn send_json<T: DeserializeOwned>(req: RequestBuilder) -> Result<(T, HeaderMap)> {
    let resp = req.send().await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = resp.text().await?;
    if !status.is_success() {
        let trimmed = text.lines().take(20).collect::<Vec<_>>().join("\n");
        return Err(anyhow!("http {}: {}", status.as_u16(), trimmed));
    }
    let parsed = serde_json::from_str(&text)?;
    Ok((parsed, headers))
}

```

File: crates/yoetz-cli/src/main.rs (8751 tokens)
```
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use clap::{Args, Parser, Subcommand};
use jsonschema::JSONSchema;
use litellm_rust::{
    ChatContentPart, ChatContentPartFile, ChatContentPartImageUrl, ChatContentPartText, ChatFile,
    ChatImageUrl, ChatMessageContent, ChatRequest, ImageData, LiteLLM,
    ProviderConfig as LiteProviderConfig, ProviderKind as LiteProviderKind,
};
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

mod browser;
mod budget;
mod commands;
mod http;
mod providers;
mod registry;

use yoetz_core::config::Config;
use yoetz_core::media::MediaInput;
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::registry::ModelRegistry;
use yoetz_core::session::{list_sessions, write_json as write_json_file};
use yoetz_core::types::{ArtifactPaths, PricingEstimate, Usage};

use http::send_json;

const DEFAULT_MAX_OUTPUT_TOKENS: usize = 1024;

#[derive(Parser)]
#[command(
    name = "yoetz",
    version,
    about = "Fast, agent-friendly LLM council tool"
)]
struct Cli {
    #[arg(long, global = true)]
    format: Option<String>,

    #[arg(long, global = true)]
    debug: bool,

    #[arg(long, global = true)]
    output_final: Option<PathBuf>,

    #[arg(long, global = true)]
    output_schema: Option<PathBuf>,

    #[arg(long, global = true)]
    profile: Option<String>,

    #[arg(long, global = true, default_value = "60")]
    timeout_secs: u64,

    #[command(subcommand)]
    command: Commands,
}

struct AppContext {
    config: Config,
    client: reqwest::Client,
    litellm: LiteLLM,
    output_final: Option<PathBuf>,
    output_schema: Option<PathBuf>,
    debug: bool,
}

#[derive(Subcommand)]
enum Commands {
    Ask(AskArgs),
    Bundle(BundleArgs),
    Status,
    Session(SessionArgs),
    Models(ModelsArgs),
    Pricing(PricingArgs),
    Browser(BrowserArgs),
    Council(CouncilArgs),
    Review(ReviewArgs),
    Apply(ApplyArgs),
    Generate(GenerateArgs),
}

#[derive(Args)]
struct AskArgs {
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long, short = 'f')]
    files: Vec<String>,

    #[arg(long)]
    exclude: Vec<String>,

    #[arg(long, default_value = "200000")]
    max_file_bytes: usize,

    #[arg(long, default_value = "5000000")]
    max_total_bytes: usize,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    video: Option<String>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct BundleArgs {
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long, short = 'f')]
    files: Vec<String>,

    #[arg(long)]
    exclude: Vec<String>,

    #[arg(long, default_value = "200000")]
    max_file_bytes: usize,

    #[arg(long, default_value = "5000000")]
    max_total_bytes: usize,

    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct SessionArgs {
    id: String,
}

#[derive(Args)]
struct BrowserArgs {
    #[command(subcommand)]
    command: BrowserCommand,
}

#[derive(Subcommand)]
enum BrowserCommand {
    Exec(BrowserExecArgs),
    Recipe(BrowserRecipeArgs),
}

#[derive(Args)]
struct BrowserExecArgs {
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct BrowserRecipeArgs {
    #[arg(long)]
    recipe: PathBuf,

    #[arg(long)]
    bundle: Option<PathBuf>,
}

#[derive(Args)]
struct CouncilArgs {
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long, short = 'f')]
    files: Vec<String>,

    #[arg(long)]
    exclude: Vec<String>,

    #[arg(long, default_value = "200000")]
    max_file_bytes: usize,

    #[arg(long, default_value = "5000000")]
    max_total_bytes: usize,

    #[arg(long, value_delimiter = ',')]
    models: Vec<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long, default_value = "4")]
    max_parallel: usize,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct ApplyArgs {
    #[arg(long)]
    patch_file: Option<PathBuf>,

    #[arg(long)]
    check: bool,

    #[arg(long)]
    reverse: bool,
}

#[derive(Args)]
struct ReviewArgs {
    #[command(subcommand)]
    command: ReviewCommand,
}

#[derive(Subcommand)]
enum ReviewCommand {
    Diff(ReviewDiffArgs),
    File(ReviewFileArgs),
}

#[derive(Args)]
struct GenerateArgs {
    #[command(subcommand)]
    command: GenerateCommand,
}

#[derive(Subcommand)]
enum GenerateCommand {
    Image(GenerateImageArgs),
    Video(GenerateVideoArgs),
}

#[derive(Args)]
struct GenerateImageArgs {
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

    #[arg(long)]
    size: Option<String>,

    #[arg(long)]
    quality: Option<String>,

    #[arg(long)]
    background: Option<String>,

    #[arg(long, default_value = "1")]
    n: usize,

    #[arg(long)]
    output_dir: Option<PathBuf>,

    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct GenerateVideoArgs {
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

    #[arg(long)]
    duration_secs: Option<u32>,

    #[arg(long)]
    aspect_ratio: Option<String>,

    #[arg(long)]
    resolution: Option<String>,

    #[arg(long)]
    size: Option<String>,

    #[arg(long)]
    negative_prompt: Option<String>,

    #[arg(long)]
    output_dir: Option<PathBuf>,

    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ReviewDiffArgs {
    #[arg(long)]
    prompt: Option<String>,

    #[arg(long)]
    staged: bool,

    #[arg(long)]
    paths: Vec<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct ReviewFileArgs {
    #[arg(long)]
    path: PathBuf,

    #[arg(long)]
    prompt: Option<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long)]
    max_file_bytes: Option<usize>,

    #[arg(long)]
    max_total_bytes: Option<usize>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct ModelsArgs {
    #[command(subcommand)]
    command: ModelsCommand,
}

#[derive(Subcommand)]
enum ModelsCommand {
    List,
    Sync,
}

#[derive(Args)]
struct PricingArgs {
    #[command(subcommand)]
    command: PricingCommand,
}

#[derive(Subcommand)]
enum PricingCommand {
    Estimate(PricingEstimateArgs),
}

#[derive(Args)]
struct PricingEstimateArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    input_tokens: usize,

    #[arg(long)]
    output_tokens: usize,
}

struct CallResult {
    content: String,
    usage: Usage,
    response_id: Option<String>,
    header_cost: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ReviewResult {
    id: String,
    provider: String,
    model: String,
    pricing: PricingEstimate,
    usage: Usage,
    content: String,
    artifacts: ArtifactPaths,
}

#[derive(Debug, Serialize)]
struct CouncilResult {
    id: String,
    provider: String,
    bundle: Option<yoetz_core::types::Bundle>,
    results: Vec<CouncilModelResult>,
    pricing: CouncilPricing,
    usage: Usage,
    artifacts: ArtifactPaths,
}

#[derive(Debug, Serialize)]
struct CouncilModelResult {
    model: String,
    content: String,
    usage: Usage,
    pricing: PricingEstimate,
    response_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct CouncilPricing {
    estimate_usd_total: Option<f64>,
    per_model: Vec<ModelEstimate>,
}

#[derive(Debug, Serialize)]
struct ModelEstimate {
    model: String,
    estimate_usd: Option<f64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment files (.env.local takes precedence over .env)
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let format = resolve_format(cli.format.as_deref())?;

    if cli.debug {
        env::set_var("YOETZ_GEMINI_DEBUG", "1");
        env::set_var("LITELLM_GEMINI_DEBUG", "1");
    }
    let config = Config::load_with_profile(cli.profile.as_deref())?;
    let client = build_client(cli.timeout_secs)?;
    let litellm = build_litellm(&config, client.clone())?;
    let ctx = AppContext {
        config,
        client,
        litellm,
        output_final: cli.output_final,
        output_schema: cli.output_schema,
        debug: cli.debug,
    };

    match cli.command {
        Commands::Ask(args) => commands::ask::handle_ask(&ctx, args, format).await,
        Commands::Bundle(args) => commands::bundle::handle_bundle(&ctx, args, format),
        Commands::Status => handle_status(&ctx, format),
        Commands::Session(args) => handle_session(&ctx, args, format),
        Commands::Models(args) => commands::models::handle_models(&ctx, args, format).await,
        Commands::Pricing(args) => commands::pricing::handle_pricing(&ctx, args, format).await,
        Commands::Browser(args) => handle_browser(args, format),
        Commands::Council(args) => commands::council::handle_council(&ctx, args, format).await,
        Commands::Apply(args) => commands::apply::handle_apply(args),
        Commands::Review(args) => commands::review::handle_review(&ctx, args, format).await,
        Commands::Generate(args) => commands::generate::handle_generate(&ctx, args, format).await,
    }
}

fn resolve_format(flag: Option<&str>) -> Result<OutputFormat> {
    if let Some(fmt) = flag {
        return fmt.parse();
    }
    if env::var("YOETZ_AGENT").ok().as_deref() == Some("1") {
        return Ok(OutputFormat::Json);
    }
    Ok(OutputFormat::Text)
}

fn build_client(timeout_secs: u64) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

fn build_litellm(config: &Config, client: reqwest::Client) -> Result<LiteLLM> {
    let mut litellm = LiteLLM::new()?.with_client(client);
    if let Some(default_provider) = config.defaults.provider.as_deref() {
        litellm = litellm.with_default_provider(default_provider);
    }
    for (name, provider) in &config.providers {
        let mut cfg = LiteProviderConfig::default();
        if let Some(base) = &provider.base_url {
            cfg = cfg.with_base_url(base.clone());
        }
        if let Some(env) = &provider.api_key_env {
            cfg = cfg.with_api_key_env(env.clone());
        }
        let kind = map_provider_kind(provider.kind.as_deref(), name);
        cfg = cfg.with_kind(kind);
        litellm = litellm.with_provider(name.clone(), cfg);
    }
    Ok(litellm)
}

fn map_provider_kind(kind: Option<&str>, name: &str) -> LiteProviderKind {
    let key = kind
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| name.to_lowercase());
    match key.as_str() {
        "anthropic" => LiteProviderKind::Anthropic,
        "gemini" => LiteProviderKind::Gemini,
        "openai" | "openai_compatible" | "openai-compatible" | "openai-compat" | "openrouter"
        | "xai" | "litellm" => LiteProviderKind::OpenAICompatible,
        _ => {
            if name.eq_ignore_ascii_case("anthropic") {
                LiteProviderKind::Anthropic
            } else if name.eq_ignore_ascii_case("gemini") {
                LiteProviderKind::Gemini
            } else {
                LiteProviderKind::OpenAICompatible
            }
        }
    }
}

fn handle_status(ctx: &AppContext, format: OutputFormat) -> Result<()> {
    let sessions = list_sessions()?;
    maybe_write_output(ctx, &sessions)?;
    match format {
        OutputFormat::Json => write_json(&sessions),
        OutputFormat::Jsonl => write_jsonl("status", &sessions),
        OutputFormat::Text | OutputFormat::Markdown => {
            for s in sessions {
                println!("{}\t{}", s.id, s.path.display());
            }
            Ok(())
        }
    }
}

fn handle_session(ctx: &AppContext, args: SessionArgs, format: OutputFormat) -> Result<()> {
    let base = yoetz_core::session::session_base_dir();
    let path = base.join(&args.id);
    if !path.exists() {
        return Err(anyhow!("session not found: {}", args.id));
    }
    maybe_write_output(ctx, &path)?;
    match format {
        OutputFormat::Json => write_json(&path),
        OutputFormat::Jsonl => write_jsonl("session", &path),
        OutputFormat::Text | OutputFormat::Markdown => {
            println!("{}", path.display());
            Ok(())
        }
    }
}

fn handle_browser(args: BrowserArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        BrowserCommand::Exec(exec) => {
            let stdout = browser::run_agent_browser(exec.args, format)?;
            print!("{stdout}");
            Ok(())
        }
        BrowserCommand::Recipe(recipe_args) => {
            let content = fs::read_to_string(&recipe_args.recipe)
                .with_context(|| format!("read recipe {}", recipe_args.recipe.display()))?;
            let recipe: browser::Recipe = serde_yaml::from_str(&content)?;

            let bundle_text = if let Some(path) = recipe_args.bundle.as_ref() {
                Some(fs::read_to_string(path)?)
            } else {
                None
            };

            let ctx = browser::RecipeContext {
                bundle_path: recipe_args.bundle.map(|p| p.to_string_lossy().to_string()),
                bundle_text,
            };

            browser::run_recipe(recipe, ctx, format)
        }
    }
}

fn build_review_diff_prompt(diff: &str, extra_prompt: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a senior engineer performing a careful code review. ");
    prompt.push_str("Return JSON only with fields: summary, findings[], risks, patches.\n");
    prompt.push_str("Each finding: {severity, file, line, message, suggestion}.\n");
    prompt.push_str("Include a unified diff in patches if needed.\n");
    if let Some(extra) = extra_prompt {
        prompt.push_str("\nAdditional instructions:\n");
        prompt.push_str(extra);
        prompt.push('\n');
    }
    prompt.push_str("\nDiff:\n```diff\n");
    prompt.push_str(diff);
    prompt.push_str("\n```\n");
    prompt
}

fn build_review_file_prompt(
    path: &std::path::Path,
    content: &str,
    truncated: bool,
    extra_prompt: Option<&str>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a senior engineer reviewing a single file. ");
    prompt.push_str("Return JSON only with fields: summary, findings[], risks, patches.\n");
    prompt.push_str("Each finding: {severity, file, line, message, suggestion}.\n");
    prompt.push_str("Include a unified diff in patches if needed.\n");
    if let Some(extra) = extra_prompt {
        prompt.push_str("\nAdditional instructions:\n");
        prompt.push_str(extra);
        prompt.push('\n');
    }
    prompt.push_str(&format!("\nFile: {}\n", path.display()));
    prompt.push_str("```text\n");
    prompt.push_str(content);
    if truncated {
        prompt.push_str("\n... [truncated]\n");
    }
    prompt.push_str("```\n");
    prompt
}

fn git_diff(staged: bool, paths: &[String]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("diff");
    cmd.arg("--no-color");
    if staged {
        cmd.arg("--staged");
    }
    if !paths.is_empty() {
        cmd.arg("--");
        for p in paths {
            cmd.arg(p);
        }
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(anyhow!("git diff failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn read_text_file(path: &std::path::Path, max_bytes: usize) -> Result<(String, bool)> {
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let truncated = metadata.len() as usize > max_bytes;
    let mut file = fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut data = vec![0u8; max_bytes];
    let read = file.read(&mut data)?;
    data.truncate(read);
    let slice = if truncated {
        &data[..max_bytes.min(data.len())]
    } else {
        &data
    };
    if slice.contains(&0) {
        return Err(anyhow!("file appears to be binary"));
    }
    match std::str::from_utf8(slice) {
        Ok(text) => Ok((text.to_string(), truncated)),
        Err(e) if truncated && e.valid_up_to() > 0 => {
            let valid = e.valid_up_to();
            let text = std::str::from_utf8(&slice[..valid]).unwrap_or("");
            Ok((text.to_string(), true))
        }
        Err(_) => Err(anyhow!("file is not valid UTF-8")),
    }
}

fn add_usage(mut total: Usage, usage: &Usage) -> Usage {
    if let Some(input) = usage.input_tokens {
        total.input_tokens = Some(total.input_tokens.unwrap_or(0) + input);
    }
    if let Some(output) = usage.output_tokens {
        total.output_tokens = Some(total.output_tokens.unwrap_or(0) + output);
    }
    if let Some(thoughts) = usage.thoughts_tokens {
        total.thoughts_tokens = Some(total.thoughts_tokens.unwrap_or(0) + thoughts);
    }
    if let Some(total_tokens) = usage.total_tokens {
        total.total_tokens = Some(total.total_tokens.unwrap_or(0) + total_tokens);
    }
    if let Some(cost) = usage.cost_usd {
        total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + cost);
    }
    total
}

fn maybe_write_output<T: Serialize>(ctx: &AppContext, value: &T) -> Result<()> {
    if ctx.output_final.is_none() && ctx.output_schema.is_none() {
        return Ok(());
    }
    let json = serde_json::to_value(value)?;
    if let Some(schema_path) = ctx.output_schema.as_ref() {
        validate_output_schema(schema_path, &json)?;
    }
    if let Some(path) = ctx.output_final.as_ref() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        write_json_file(path, &json)?;
    }
    Ok(())
}

fn validate_output_schema(path: &std::path::Path, value: &Value) -> Result<()> {
    let schema_text =
        fs::read_to_string(path).with_context(|| format!("read schema {}", path.display()))?;
    let schema_json: Value = serde_json::from_str(&schema_text)?;
    let compiled = JSONSchema::compile(&schema_json)
        .map_err(|e| anyhow!("invalid schema {}: {e}", path.display()))?;
    if let Err(errors) = compiled.validate(value) {
        let messages = errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
        return Err(anyhow!(
            "output does not match schema {}: {}",
            path.display(),
            messages
        ));
    }
    Ok(())
}

fn resolve_prompt(prompt: Option<String>, prompt_file: Option<PathBuf>) -> Result<String> {
    if let Some(p) = prompt {
        return Ok(p);
    }
    if let Some(path) = prompt_file {
        let content = fs::read_to_string(path)?;
        return Ok(content);
    }
    let mut buf = String::new();
    if !io::stdin().is_terminal() {
        io::stdin().read_to_string(&mut buf)?;
        if !buf.trim().is_empty() {
            return Ok(buf);
        }
    }
    Err(anyhow!(
        "prompt is required (--prompt, --prompt-file, or stdin)"
    ))
}

fn resolve_response_format(
    format: Option<String>,
    schema_path: Option<PathBuf>,
    schema_name: Option<String>,
) -> Result<Option<Value>> {
    if let Some(path) = schema_path {
        let schema_text =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let schema_json: Value = serde_json::from_str(&schema_text)
            .with_context(|| format!("parse schema {}", path.display()))?;
        if !schema_json.is_object() {
            return Err(anyhow!(
                "response schema must be a JSON object: {}",
                path.display()
            ));
        }
        let name = schema_name.unwrap_or_else(|| "yoetz_response".to_string());
        if let Some(fmt) = format.as_deref() {
            if fmt.eq_ignore_ascii_case("text") {
                return Err(anyhow!(
                    "--response_format=text is incompatible with --response_schema"
                ));
            }
        }
        return Ok(Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "schema": schema_json,
                "strict": true,
            }
        })));
    }

    let format = match format.as_deref() {
        Some("json") | Some("json_object") => Some(serde_json::json!({ "type": "json_object" })),
        Some("text") | None => None,
        Some(other) => {
            return Err(anyhow!(
                "unsupported response_format: {other} (use json or text)"
            ))
        }
    };
    Ok(format)
}

fn parse_media_inputs(values: &[String]) -> Result<Vec<MediaInput>> {
    let mut out = Vec::new();
    for value in values {
        out.push(parse_media_input(value)?);
    }
    Ok(out)
}

fn parse_media_input(value: &str) -> Result<MediaInput> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return MediaInput::from_url(value, None);
    }
    MediaInput::from_path(PathBuf::from(value).as_path())
}

fn render_bundle_md(bundle: &yoetz_core::types::Bundle) -> String {
    let mut out = String::new();
    out.push_str("# Yoetz Bundle\n\n");
    out.push_str("## Prompt\n\n");
    out.push_str(&bundle.prompt);
    out.push_str("\n\n## Files\n\n");
    for file in &bundle.files {
        out.push_str(&format!("### {}\n\n", file.path));
        if let Some(content) = &file.content {
            let fence = markdown_fence(content);
            out.push_str(&fence);
            out.push('\n');
            out.push_str(content);
            if file.truncated {
                out.push_str("\n... [truncated]\n");
            }
            out.push_str(&fence);
            out.push_str("\n\n");
        } else if file.is_binary {
            out.push_str("(binary file omitted)\n\n");
        } else if file.truncated {
            out.push_str("(content omitted)\n\n");
        }
    }
    out
}

fn markdown_fence(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    let len = std::cmp::max(3, max_run + 1);
    "`".repeat(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_schema_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yoetz_schema_{nanos}.json"))
    }

    #[test]
    fn response_format_json_object() {
        let fmt = resolve_response_format(Some("json".to_string()), None, None).unwrap();
        assert!(fmt.is_some());
    }

    #[test]
    fn response_format_schema_file() {
        let path = temp_schema_path();
        fs::write(
            &path,
            r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#,
        )
        .unwrap();
        let fmt = resolve_response_format(None, Some(path.clone()), None).unwrap();
        assert!(fmt.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_text_file_truncates_utf8_safely() {
        let text = "hello 🙂 world";
        let bytes = text.as_bytes();
        let cut = bytes.iter().position(|b| *b == 0xF0).unwrap_or(bytes.len());
        let path = std::env::temp_dir().join(format!(
            "yoetz_read_text_{}.txt",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, bytes).unwrap();
        let (content, truncated) = read_text_file(&path, cut + 1).unwrap();
        assert!(truncated);
        assert!(content.starts_with("hello "));
        let _ = fs::remove_file(path);
    }
}

async fn call_litellm(
    litellm: &LiteLLM,
    provider: Option<&str>,
    model: &str,
    prompt: &str,
    temperature: f32,
    max_output_tokens: usize,
    response_format: Option<Value>,
    images: &[MediaInput],
    video: Option<&MediaInput>,
) -> Result<CallResult> {
    let model_spec = build_model_spec(provider, model)?;
    let mut req = ChatRequest::new(model_spec)
        .temperature(temperature)
        .max_tokens(max_output_tokens as u32);
    req.response_format = response_format;

    if images.is_empty() && video.is_none() {
        req = req.message("user", prompt);
    } else {
        let mut parts = Vec::new();
        parts.push(ChatContentPart::Text(ChatContentPartText {
            kind: "text".to_string(),
            text: prompt.to_string(),
        }));
        for image in images {
            parts.push(media_to_image_part(image)?);
        }
        if let Some(video) = video {
            parts.push(media_to_file_part(video)?);
        }
        req = req.message_with_content("user", ChatMessageContent::Parts(parts));
    }

    let resp = litellm.completion(req).await?;
    Ok(CallResult {
        content: resp.content,
        usage: usage_from_litellm(resp.usage),
        response_id: resp.response_id,
        header_cost: resp.header_cost,
    })
}

fn build_model_spec(provider: Option<&str>, model: &str) -> Result<String> {
    let Some(provider) = provider else {
        return Ok(model.to_string());
    };
    let provider_lc = provider.to_lowercase();
    if let Some((prefix, _rest)) = model.split_once('/') {
        let prefix_lc = prefix.to_lowercase();
        if provider_lc == "gemini" && prefix_lc == "models" {
            return Ok(format!("{provider}/{model}"));
        }
        if provider_lc == "openrouter" {
            if prefix_lc == "openrouter" {
                let rest = model.split_once('/').map(|(_, rest)| rest).unwrap_or("");
                if !rest.contains('/') {
                    return Err(anyhow!(
                        "openrouter models must be namespaced (e.g. openai/gpt-5.2, anthropic/claude-sonnet-4-5)"
                    ));
                }
                return Ok(model.to_string());
            }
            return Ok(format!("{provider}/{model}"));
        }
        if prefix_lc == provider_lc {
            return Ok(model.to_string());
        }
        return Err(anyhow!(
            "model prefix '{prefix}' conflicts with provider '{provider}'. \
use --provider {prefix} or pass an unprefixed model name"
        ));
    }
    if provider_lc == "openrouter" {
        return Err(anyhow!(
            "openrouter models must be namespaced (e.g. openai/gpt-5.2, anthropic/claude-sonnet-4-5)"
        ));
    }
    Ok(format!("{provider}/{model}"))
}

fn resolve_max_output_tokens(requested: Option<usize>, config: &Config) -> usize {
    requested
        .or(config.defaults.max_output_tokens)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
}

fn resolve_registry_model_id(
    provider: Option<&str>,
    model_id: Option<&str>,
    registry: Option<&ModelRegistry>,
) -> Option<String> {
    let model_id = model_id?;
    let mut candidates = Vec::new();
    candidates.push(model_id.to_string());

    if let Some(stripped) = model_id.strip_prefix("openrouter/") {
        candidates.push(stripped.to_string());
    }
    if let Some(stripped) = model_id.strip_prefix("models/") {
        candidates.push(stripped.to_string());
    }

    if let Some(provider) = provider {
        let provider_lc = provider.to_lowercase();
        if !model_id.contains('/') {
            candidates.push(format!("{provider}/{model_id}"));
            if provider_lc == "gemini" {
                candidates.push(format!("google/{model_id}"));
            }
        }
    }

    if let Some(registry) = registry {
        for candidate in &candidates {
            if registry.find(candidate).is_some() {
                return Some(candidate.clone());
            }
        }
    }

    candidates.into_iter().next()
}

fn usage_from_litellm(usage: litellm_rust::Usage) -> Usage {
    Usage {
        input_tokens: usage.prompt_tokens.map(|v| v as usize),
        output_tokens: usage.completion_tokens.map(|v| v as usize),
        thoughts_tokens: usage.thoughts_tokens.map(|v| v as usize),
        total_tokens: usage.total_tokens.map(|v| v as usize),
        cost_usd: usage.cost_usd,
    }
}

fn apply_capability_warnings(
    registry: Option<&ModelRegistry>,
    model_id: Option<&str>,
    has_images: bool,
    has_video: bool,
    pricing: &mut PricingEstimate,
) -> Result<()> {
    if !has_images && !has_video {
        return Ok(());
    }
    let Some(model_id) = model_id else {
        return Ok(());
    };
    let Some(registry) = registry else {
        pricing.warnings.push(
            "registry unavailable; cannot validate model capabilities (run `yoetz models sync`)"
                .to_string(),
        );
        return Ok(());
    };
    let Some(entry) = registry.find(model_id) else {
        pricing.warnings.push(format!(
            "model capabilities unknown; {model_id} not in registry (run `yoetz models sync`)"
        ));
        return Ok(());
    };

    if has_images {
        match entry.capability.as_ref().and_then(|cap| cap.vision) {
            Some(true) => {}
            Some(false) => {
                return Err(anyhow!("model {model_id} does not support image inputs"));
            }
            None => pricing.warnings.push(format!(
                "model capability unknown for {model_id}; cannot validate vision inputs"
            )),
        }
    }

    if has_video {
        pricing.warnings.push(
            "video support is not tracked in registry; provider gemini is required".to_string(),
        );
    }

    Ok(())
}

fn media_to_image_part(media: &MediaInput) -> Result<ChatContentPart> {
    if media.media_type != yoetz_core::media::MediaType::Image {
        return Err(anyhow!("expected image media input"));
    }
    let url = media.as_data_url()?;
    Ok(ChatContentPart::ImageUrl(ChatContentPartImageUrl {
        kind: "image_url".to_string(),
        image_url: ChatImageUrl::Url(url),
    }))
}

fn media_to_file_part(media: &MediaInput) -> Result<ChatContentPart> {
    let url = media.as_data_url()?;
    Ok(ChatContentPart::File(ChatContentPartFile {
        kind: "file".to_string(),
        file: ChatFile {
            file_id: None,
            file_data: Some(url),
            format: Some(media.mime_type.clone()),
            detail: None,
            video_metadata: None,
        },
    }))
}

async fn save_image_outputs(
    client: &reqwest::Client,
    images: Vec<ImageData>,
    output_dir: &std::path::Path,
    model: &str,
) -> Result<Vec<yoetz_core::media::MediaOutput>> {
    let mut outputs = Vec::new();
    for (idx, image) in images.into_iter().enumerate() {
        let filename = format!("image_{idx}.png");
        let path = output_dir.join(filename);
        if let Some(b64) = image.b64_json.as_ref() {
            let bytes = general_purpose::STANDARD
                .decode(b64.as_bytes())
                .context("decode image base64")?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else if let Some(url) = image.url.as_ref() {
            let bytes = client.get(url).send().await?.bytes().await?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else {
            continue;
        }

        outputs.push(yoetz_core::media::MediaOutput {
            media_type: yoetz_core::media::MediaType::Image,
            path,
            url: image.url,
            metadata: yoetz_core::media::MediaMetadata {
                width: None,
                height: None,
                duration_secs: None,
                model: model.to_string(),
                revised_prompt: image.revised_prompt,
            },
        });
    }
    Ok(outputs)
}

async fn fetch_openrouter_cost(
    client: &reqwest::Client,
    config: &Config,
    response_id: &str,
) -> Result<Option<f64>> {
    let provider_cfg = config.providers.get("openrouter");
    let base_url = provider_cfg
        .and_then(|p| p.base_url.clone())
        .or_else(|| providers::default_base_url("openrouter"))
        .ok_or_else(|| anyhow!("base_url not found for openrouter"))?;

    let api_key_env = provider_cfg
        .and_then(|p| p.api_key_env.clone())
        .or_else(|| providers::default_api_key_env("openrouter"))
        .unwrap_or_else(|| "OPENROUTER_API_KEY".to_string());

    let api_key = match env::var(&api_key_env) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let url = format!(
        "{}/generation?id={}",
        base_url.trim_end_matches('/'),
        response_id
    );

    let (payload, _) = send_json::<Value>(client.get(url).bearer_auth(api_key)).await?;
    let data = payload.get("data").unwrap_or(&Value::Null);
    Ok(parse_cost(data.get("total_cost"))
        .or_else(|| parse_cost(data.get("total_cost_usd")))
        .or_else(|| parse_cost(payload.get("total_cost"))))
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

// defaults moved to providers module

```

File: crates/yoetz-cli/src/providers/gemini.rs (3180 tokens)
```
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
    pub raw: Value,
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
        raw: resp,
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
        let thoughts_tokens = meta
            .get("thoughtsTokenCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let total_tokens = meta
            .get("totalTokenCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        return Usage {
            input_tokens,
            output_tokens,
            thoughts_tokens,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_text_from_candidates() {
        let resp = json!({
            "candidates": [
                {"content": {"parts": [{"text": "hi"}, {"text": " there"}]}}
            ]
        });
        assert_eq!(extract_text(&resp), "hi there");
    }

    #[test]
    fn extract_video_uri_prefers_generate_video_response() {
        let resp = json!({
            "response": {
                "generateVideoResponse": {
                    "generatedSamples": [
                        {"video": {"uri": "gs://bucket/video.mp4"}}
                    ]
                }
            }
        });
        assert_eq!(
            extract_video_uri(&resp),
            Some("gs://bucket/video.mp4".to_string())
        );
    }
}

```

File: crates/yoetz-cli/src/providers/mod.rs (424 tokens)
```
use anyhow::{anyhow, Context, Result};
use std::env;

use yoetz_core::config::Config;

pub mod gemini;
pub mod openai;

#[derive(Debug, Clone)]
pub struct ProviderAuth {
    pub base_url: String,
    pub api_key: String,
}

pub fn resolve_provider_auth(config: &Config, provider: &str) -> Result<ProviderAuth> {
    let provider_cfg = config.providers.get(provider);

    let base_url = provider_cfg
        .and_then(|p| p.base_url.clone())
        .or_else(|| default_base_url(provider))
        .ok_or_else(|| anyhow!("base_url not found for provider {provider}"))?;

    let api_key_env = provider_cfg
        .and_then(|p| p.api_key_env.clone())
        .or_else(|| default_api_key_env(provider))
        .ok_or_else(|| anyhow!("api_key_env not configured for provider {provider}"))?;

    let api_key =
        env::var(&api_key_env).with_context(|| format!("missing env var {api_key_env}"))?;

    Ok(ProviderAuth { base_url, api_key })
}

pub fn default_base_url(provider: &str) -> Option<String> {
    match provider {
        "openrouter" => Some("https://openrouter.ai/api/v1".to_string()),
        "openai" => Some("https://api.openai.com/v1".to_string()),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
        _ => None,
    }
}

pub fn default_api_key_env(provider: &str) -> Option<String> {
    match provider {
        "openrouter" => Some("OPENROUTER_API_KEY".to_string()),
        "openai" => Some("OPENAI_API_KEY".to_string()),
        "litellm" => Some("LITELLM_API_KEY".to_string()),
        "gemini" => Some("GEMINI_API_KEY".to_string()),
        _ => None,
    }
}

```

File: crates/yoetz-cli/src/providers/openai.rs (4212 tokens)
```
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

```

File: crates/yoetz-cli/src/registry.rs (2790 tokens)
```
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::http::send_json;
use litellm_rust::registry::Registry as EmbeddedRegistry;
use yoetz_core::config::Config;
use yoetz_core::registry::{ModelCapability, ModelEntry, ModelPricing, ModelRegistry};

pub struct RegistryFetchResult {
    pub registry: ModelRegistry,
    pub warnings: Vec<String>,
}

pub fn registry_cache_path() -> PathBuf {
    if let Ok(path) = env::var("YOETZ_REGISTRY_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".yoetz/registry.json");
    }
    PathBuf::from(".yoetz/registry.json")
}

pub fn load_registry_cache() -> Result<Option<ModelRegistry>> {
    let path = registry_cache_path();
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("read registry {}", path.display()))?;
    let registry: ModelRegistry = serde_json::from_str(&content)?;
    Ok(Some(registry))
}

pub fn save_registry_cache(registry: &ModelRegistry) -> Result<PathBuf> {
    let path = registry_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(registry)?;
    fs::write(&path, data).with_context(|| format!("write registry {}", path.display()))?;
    Ok(path)
}

pub async fn fetch_registry(client: &Client, config: &Config) -> Result<RegistryFetchResult> {
    let mut registry = ModelRegistry::default();
    let mut warnings = Vec::new();

    if let Some(org_path) = config.registry.org_registry_path.as_deref() {
        if Path::new(org_path).exists() {
            let content = fs::read_to_string(org_path)?;
            let org_registry: ModelRegistry = serde_json::from_str(&content)?;
            registry.merge(org_registry);
        } else {
            warnings.push(format!("org registry not found: {org_path}"));
        }
    }

    match fetch_openrouter(client, config).await {
        Ok(Some(openrouter)) => registry.merge(openrouter),
        Ok(None) => warnings.push("openrouter skipped: missing API key".to_string()),
        Err(err) => warnings.push(format!("openrouter failed: {err}")),
    }

    match fetch_litellm(client, config).await {
        Ok(Some(litellm)) => registry.merge(litellm),
        Ok(None) => warnings.push("litellm skipped: missing API key".to_string()),
        Err(err) => warnings.push(format!("litellm failed: {err}")),
    }

    match embedded_gemini_registry() {
        Ok(embedded) => registry.merge(embedded),
        Err(err) => warnings.push(format!("embedded gemini registry skipped: {err}")),
    }

    registry.updated_at = Some(
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default(),
    );
    if registry.version == 0 {
        registry.version = 1;
    }

    Ok(RegistryFetchResult { registry, warnings })
}

fn embedded_gemini_registry() -> Result<ModelRegistry> {
    let embedded =
        EmbeddedRegistry::load_embedded().map_err(|e| anyhow!("load embedded registry: {e}"))?;
    let mut registry = ModelRegistry::default();
    for (name, pricing) in embedded.models.into_iter() {
        let name_lc = name.to_lowercase();
        let provider_lc = pricing.provider.as_deref().unwrap_or("").to_lowercase();
        let is_gemini = name_lc.contains("gemini")
            || name_lc.contains("veo")
            || provider_lc.contains("gemini")
            || provider_lc.contains("google");
        if !is_gemini {
            continue;
        }

        let context_length = pricing
            .max_input_tokens
            .or(pricing.max_output_tokens)
            .map(|v| v as usize);

        registry.models.push(ModelEntry {
            id: name,
            context_length,
            pricing: ModelPricing {
                prompt_per_1k: pricing.input_cost_per_1k,
                completion_per_1k: pricing.output_cost_per_1k,
                request: None,
            },
            provider: pricing.provider.clone(),
            capability: None,
        });
    }
    Ok(registry)
}

async fn fetch_openrouter(client: &Client, config: &Config) -> Result<Option<ModelRegistry>> {
    let url = config
        .registry
        .openrouter_models_url
        .clone()
        .unwrap_or_else(|| "https://openrouter.ai/api/v1/models".to_string());

    let provider = config.providers.get("openrouter");
    let api_key_env = provider
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_else(|| "OPENROUTER_API_KEY".to_string());

    let api_key = match env::var(&api_key_env) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let (payload, _) = send_json::<Value>(client.get(&url).bearer_auth(api_key)).await?;
    Ok(Some(parse_openrouter_models(&payload)))
}

async fn fetch_litellm(client: &Client, config: &Config) -> Result<Option<ModelRegistry>> {
    let provider = config.providers.get("litellm");
    let api_key_env = provider
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_else(|| "LITELLM_API_KEY".to_string());

    let api_key = env::var(&api_key_env).ok();

    let urls = if let Some(url) = config.registry.litellm_models_url.clone() {
        vec![url]
    } else {
        vec![
            "http://localhost:4000/model/info".to_string(),
            "http://localhost:4000/v1/model/info".to_string(),
        ]
    };

    let mut last_err: Option<anyhow::Error> = None;
    for url in urls {
        let mut req = client.get(&url);
        if let Some(key) = api_key.as_deref() {
            req = req.bearer_auth(key);
        }
        match send_json::<Value>(req).await {
            Ok((payload, _)) => return Ok(Some(parse_litellm_models(&payload))),
            Err(err) => last_err = Some(err),
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }
    if api_key.is_none() {
        return Ok(None);
    }
    Ok(None)
}

fn parse_openrouter_models(value: &Value) -> ModelRegistry {
    let mut registry = ModelRegistry::default();
    let data = value
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for item in data {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let context_length = item
            .get("context_length")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let pricing_obj = item.get("pricing");
        let pricing = ModelPricing {
            prompt_per_1k: pricing_obj
                .and_then(|p| parse_price(p.get("prompt")))
                .map(|v| v * 1000.0),
            completion_per_1k: pricing_obj
                .and_then(|p| parse_price(p.get("completion")))
                .map(|v| v * 1000.0),
            request: pricing_obj.and_then(|p| parse_price(p.get("request"))),
        };

        let capability = parse_openrouter_capability(&item);
        registry.models.push(ModelEntry {
            id: id.to_string(),
            context_length,
            pricing,
            provider: Some("openrouter".to_string()),
            capability,
        });
    }

    registry
}

fn parse_litellm_models(value: &Value) -> ModelRegistry {
    let mut registry = ModelRegistry::default();
    let data = value
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for item in data {
        let model_name = item
            .get("model_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let model_info = item.get("model_info").unwrap_or(&Value::Null);
        let id = if !model_name.is_empty() {
            model_name.to_string()
        } else {
            model_info
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        if id.is_empty() {
            continue;
        }

        let input_cost = parse_price(model_info.get("input_cost_per_token"));
        let output_cost = parse_price(model_info.get("output_cost_per_token"));
        let max_tokens = model_info
            .get("max_input_tokens")
            .or_else(|| model_info.get("max_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        registry.models.push(ModelEntry {
            id,
            context_length: max_tokens,
            pricing: ModelPricing {
                prompt_per_1k: input_cost.map(|v| v * 1000.0),
                completion_per_1k: output_cost.map(|v| v * 1000.0),
                request: None,
            },
            provider: Some("litellm".to_string()),
            capability: None,
        });
    }

    registry
}

fn parse_price(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}

pub fn estimate_pricing(
    registry: Option<&ModelRegistry>,
    model_id: &str,
    input_tokens: usize,
    output_tokens: usize,
) -> Result<yoetz_core::types::PricingEstimate> {
    let mut estimate = yoetz_core::types::PricingEstimate::default();
    let Some(registry) = registry else {
        estimate
            .warnings
            .push("registry unavailable; run `yoetz models sync`".to_string());
        return Ok(estimate);
    };

    let entry = registry
        .find(model_id)
        .ok_or_else(|| anyhow!("model not found in registry: {model_id}"));
    if let Ok(entry) = entry {
        estimate.input_tokens = Some(input_tokens);
        estimate.output_tokens = Some(output_tokens);
        estimate.pricing_source = entry.provider.clone();
        estimate.estimate_usd = entry.pricing.estimate(input_tokens, output_tokens);
    } else {
        estimate.warnings.push(format!(
            "model not found in registry: {model_id}; run `yoetz models sync` to refresh"
        ));
    }

    Ok(estimate)
}

fn parse_openrouter_capability(item: &Value) -> Option<ModelCapability> {
    let mut cap = ModelCapability::default();

    if let Some(modalities) = item
        .get("architecture")
        .and_then(|v| v.get("input_modalities"))
        .and_then(|v| v.as_array())
    {
        let has_image = modalities
            .iter()
            .any(|m| m.as_str().is_some_and(|s| s.eq_ignore_ascii_case("image")));
        cap.vision = Some(has_image);
    }

    if let Some(params) = item.get("supported_parameters").and_then(|v| v.as_array()) {
        let has_reasoning = params.iter().any(|p| {
            p.as_str().is_some_and(|s| {
                s.eq_ignore_ascii_case("reasoning")
                    || s.eq_ignore_ascii_case("reasoning_effort")
                    || s.eq_ignore_ascii_case("include_reasoning")
                    || s.eq_ignore_ascii_case("thinking")
            })
        });
        if has_reasoning {
            cap.reasoning = Some(true);
        }
    }

    let web_search = item
        .get("pricing")
        .and_then(|v| v.get("web_search"))
        .and_then(|v| if v.is_null() { None } else { Some(v) })
        .is_some();
    if web_search {
        cap.web_search = Some(true);
    }

    if cap.vision.is_none() && cap.reasoning.is_none() && cap.web_search.is_none() {
        None
    } else {
        Some(cap)
    }
}

```

File: crates/yoetz-core/Cargo.toml (146 tokens)
```
[package]
name = "yoetz-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Core types and utilities for yoetz LLM gateway"
keywords = ["llm", "ai", "bundler"]
categories = ["development-tools"]

[lints]
workspace = true

[dependencies]
anyhow.workspace = true
serde.workspace = true
serde_json.workspace = true
toml.workspace = true
thiserror.workspace = true
ignore.workspace = true
rand.workspace = true
time.workspace = true
sha2.workspace = true
hex.workspace = true
base64.workspace = true
mime_guess.workspace = true

```

File: crates/yoetz-core/src/bundle.rs (1731 tokens)
```
use crate::types::{Bundle, BundleFile, BundleStats};
use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BundleOptions {
    pub root: PathBuf,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub include_hidden: bool,
    pub include_binary: bool,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: 200_000,
            max_total_bytes: 5_000_000,
            include_hidden: false,
            include_binary: false,
        }
    }
}

pub fn build_bundle(prompt: &str, options: BundleOptions) -> Result<Bundle> {
    let mut override_builder = OverrideBuilder::new(&options.root);
    // OverrideBuilder uses whitelist semantics for positive patterns.
    for pattern in &options.include {
        override_builder.add(pattern)?;
    }
    for pattern in &options.exclude {
        override_builder.add(&format!("!{}", pattern))?;
    }
    let overrides = override_builder.build()?;

    let mut walker = WalkBuilder::new(&options.root);
    walker
        .hidden(!options.include_hidden)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .overrides(overrides);

    let mut files = Vec::new();
    let mut total_bytes = 0usize;
    let mut total_chars = 0usize;

    for entry in walker.build() {
        let entry = entry?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();
        let rel_path = path
            .strip_prefix(&options.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let metadata = fs::metadata(path).with_context(|| format!("stat file {rel_path}"))?;
        let file_size = metadata.len() as usize;
        let truncated_by_size = file_size > options.max_file_bytes;

        let data = read_prefix(path, options.max_file_bytes)
            .with_context(|| format!("read file {rel_path}"))?;
        let (mut content, mut truncated, is_binary) =
            extract_text(&data, options.max_file_bytes, truncated_by_size);

        if is_binary && !options.include_binary {
            files.push(BundleFile {
                path: rel_path,
                bytes: file_size,
                sha256: sha256_hex_file(path)?,
                truncated,
                is_binary,
                content: None,
            });
            continue;
        }

        let mut content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
        if content_len > 0 && total_bytes + content_len > options.max_total_bytes {
            content = Some("[omitted: exceeds max_total_bytes]".to_string());
            truncated = true;
            content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
        }

        total_chars += content_len;
        total_bytes += content_len;

        files.push(BundleFile {
            path: rel_path,
            bytes: file_size,
            sha256: sha256_hex_file(path)?,
            truncated,
            is_binary,
            content,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let stats = BundleStats {
        file_count: files.len(),
        total_bytes,
        total_chars,
        estimated_tokens: estimate_tokens(prompt.len() + total_chars),
    };

    Ok(Bundle {
        prompt: prompt.to_string(),
        files,
        stats,
    })
}

fn extract_text(
    data: &[u8],
    max_bytes: usize,
    truncated_by_size: bool,
) -> (Option<String>, bool, bool) {
    let slice = if truncated_by_size {
        &data[..max_bytes.min(data.len())]
    } else {
        data
    };

    if slice.contains(&0) {
        return (None, truncated_by_size, true);
    }

    match std::str::from_utf8(slice) {
        Ok(s) => (Some(s.to_string()), truncated_by_size, false),
        Err(e) if truncated_by_size && e.valid_up_to() > 0 => {
            let valid = e.valid_up_to();
            let s = std::str::from_utf8(&slice[..valid]).unwrap_or("");
            (Some(s.to_string()), true, false)
        }
        Err(_) => (None, truncated_by_size, true),
    }
}

fn read_prefix(path: &std::path::Path, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let read = file.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

fn sha256_hex_file(path: &std::path::Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let digest = hasher.finalize();
    Ok(hex::encode(digest))
}

pub fn estimate_tokens(chars: usize) -> usize {
    // Rough heuristic: 4 chars per token.
    chars.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::extract_text;
    use super::{build_bundle, BundleOptions};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extract_text_truncates_utf8_safely() {
        let text = "hello 🙂 world";
        let bytes = text.as_bytes();
        let cut = bytes.iter().position(|b| *b == 0xF0).unwrap_or(bytes.len());
        let data = &bytes[..cut + 1];
        let (content, truncated, is_binary) = extract_text(data, data.len(), true);
        assert!(truncated);
        assert!(!is_binary);
        let content = content.expect("expected utf-8 content");
        assert!(content.starts_with("hello "));
    }

    #[test]
    fn bundle_files_sorted_and_hash_full_file() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_bundle_test_{nanos}"));
        fs::create_dir_all(&root).unwrap();

        let a_path = root.join("a.txt");
        let b_path = root.join("b.txt");
        fs::write(&b_path, "bbb").unwrap();
        fs::write(&a_path, "aaa").unwrap();

        let options = BundleOptions {
            root: root.clone(),
            include: vec!["**/*".to_string()],
            max_file_bytes: 2, // force truncation
            ..BundleOptions::default()
        };

        let bundle = build_bundle("prompt", options).unwrap();
        let paths: Vec<_> = bundle.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);

        let mut hasher = Sha256::new();
        hasher.update(b"aaa");
        let a_hash = hex::encode(hasher.finalize());
        let file_a = bundle.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(file_a.sha256, a_hash);

        let _ = fs::remove_dir_all(&root);
    }
}

```

File: crates/yoetz-core/src/config.rs (1100 tokens)
```
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub defaults: Defaults,
    pub providers: HashMap<String, ProviderConfig>,
    pub registry: RegistryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Defaults {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub max_output_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    pub openrouter_models_url: Option<String>,
    pub litellm_models_url: Option<String>,
    pub org_registry_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    pub defaults: Option<Defaults>,
    pub providers: Option<HashMap<String, ProviderConfig>>,
    pub registry: Option<RegistryConfig>,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_with_profile(None)
    }

    pub fn load_with_profile(profile: Option<&str>) -> Result<Self> {
        let mut config = Config::default();
        for path in default_config_paths(profile) {
            if path.exists() {
                let file = load_config_file(&path)?;
                config.merge(file);
            }
        }
        Ok(config)
    }

    fn merge(&mut self, other: ConfigFile) {
        if let Some(defaults) = other.defaults {
            merge_defaults(&mut self.defaults, defaults);
        }
        if let Some(providers) = other.providers {
            for (k, v) in providers {
                self.providers
                    .entry(k)
                    .and_modify(|existing| merge_provider(existing, &v))
                    .or_insert(v);
            }
        }
        if let Some(registry) = other.registry {
            merge_registry(&mut self.registry, registry);
        }
    }
}

fn load_config_file(path: &Path) -> Result<ConfigFile> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let parsed: ConfigFile =
        toml::from_str(&content).with_context(|| format!("parse config {}", path.display()))?;
    Ok(parsed)
}

fn default_config_paths(profile: Option<&str>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = home_dir() {
        paths.push(home.join(".yoetz/config.toml"));
        paths.push(home.join(".config/yoetz/config.toml"));
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("yoetz/config.toml"));
    }
    paths.push(PathBuf::from("./yoetz.toml"));

    if let Some(name) = profile {
        if let Some(home) = home_dir() {
            paths.push(home.join(".yoetz/profiles").join(format!("{name}.toml")));
            paths.push(
                home.join(".config/yoetz/profiles")
                    .join(format!("{name}.toml")),
            );
        }
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            paths.push(
                PathBuf::from(xdg)
                    .join("yoetz/profiles")
                    .join(format!("{name}.toml")),
            );
        }
        paths.push(PathBuf::from(format!("./yoetz.{name}.toml")));
    }
    paths
}

fn home_dir() -> Option<PathBuf> {
    env::var("HOME").map(PathBuf::from).ok()
}

fn merge_defaults(target: &mut Defaults, other: Defaults) {
    if other.profile.is_some() {
        target.profile = other.profile;
    }
    if other.model.is_some() {
        target.model = other.model;
    }
    if other.provider.is_some() {
        target.provider = other.provider;
    }
    if other.max_output_tokens.is_some() {
        target.max_output_tokens = other.max_output_tokens;
    }
}

fn merge_provider(target: &mut ProviderConfig, other: &ProviderConfig) {
    if other.base_url.is_some() {
        target.base_url = other.base_url.clone();
    }
    if other.api_key_env.is_some() {
        target.api_key_env = other.api_key_env.clone();
    }
    if other.kind.is_some() {
        target.kind = other.kind.clone();
    }
}

fn merge_registry(target: &mut RegistryConfig, other: RegistryConfig) {
    if other.openrouter_models_url.is_some() {
        target.openrouter_models_url = other.openrouter_models_url;
    }
    if other.litellm_models_url.is_some() {
        target.litellm_models_url = other.litellm_models_url;
    }
    if other.org_registry_path.is_some() {
        target.org_registry_path = other.org_registry_path;
    }
}

```

File: crates/yoetz-core/src/lib.rs (37 tokens)
```
//! Core types and utilities for yoetz.

pub mod bundle;
pub mod config;
pub mod media;
pub mod output;
pub mod registry;
pub mod session;
pub mod types;

```

File: crates/yoetz-core/src/media.rs (1300 tokens)
```
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use mime_guess::MimeGuess;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MediaType {
    Image,
    Video,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaSource {
    File(PathBuf),
    Url(String),
    Base64 { data: String, mime: String },
    FileApiId { id: String, provider: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInput {
    pub source: MediaSource,
    pub media_type: MediaType,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaOutput {
    pub media_type: MediaType,
    pub path: PathBuf,
    pub url: Option<String>,
    pub metadata: MediaMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f32>,
    pub model: String,
    pub revised_prompt: Option<String>,
}

impl MediaInput {
    pub fn from_path(path: &Path) -> Result<Self> {
        let mime = guess_mime(path)?;
        let media_type = media_type_from_mime(&mime)?;
        let size_bytes = fs::metadata(path).ok().map(|m| m.len());
        Ok(Self {
            source: MediaSource::File(path.to_path_buf()),
            media_type,
            mime_type: mime,
            size_bytes,
        })
    }

    pub fn from_url(url: &str, mime_type: Option<&str>) -> Result<Self> {
        let mime = mime_type
            .map(|m| m.to_string())
            .unwrap_or_else(|| guess_mime_from_url(url));
        let media_type = media_type_from_mime(&mime)?;
        Ok(Self {
            source: MediaSource::Url(url.to_string()),
            media_type,
            mime_type: mime,
            size_bytes: None,
        })
    }

    pub fn as_data_url(&self) -> Result<String> {
        let (data, mime) = match &self.source {
            MediaSource::Base64 { data, mime } => (data.clone(), mime.clone()),
            MediaSource::File(path) => {
                let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
                (
                    general_purpose::STANDARD.encode(bytes),
                    self.mime_type.clone(),
                )
            }
            MediaSource::Url(url) => return Ok(url.clone()),
            MediaSource::FileApiId { id, .. } => {
                return Err(anyhow!("file API id cannot be converted to data url: {id}"));
            }
        };
        Ok(format!("data:{};base64,{}", mime, data))
    }

    pub fn read_bytes(&self) -> Result<Vec<u8>> {
        match &self.source {
            MediaSource::File(path) => {
                fs::read(path).with_context(|| format!("read {}", path.display()))
            }
            MediaSource::Base64 { data, .. } => general_purpose::STANDARD
                .decode(data)
                .context("decode base64 media"),
            MediaSource::Url(url) => Err(anyhow!("cannot read bytes from url: {url}")),
            MediaSource::FileApiId { id, .. } => {
                Err(anyhow!("cannot read bytes from file API id: {id}"))
            }
        }
    }
}

fn guess_mime(path: &Path) -> Result<String> {
    let mime = MimeGuess::from_path(path).first_or_octet_stream();
    Ok(mime.essence_str().to_string())
}

fn guess_mime_from_url(url: &str) -> String {
    let trimmed = url
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(url)
        .split_once('#')
        .map(|(base, _)| base)
        .unwrap_or(url);
    let mime = MimeGuess::from_path(trimmed).first_or_octet_stream();
    mime.essence_str().to_string()
}

fn media_type_from_mime(mime: &str) -> Result<MediaType> {
    if mime.starts_with("image/") {
        Ok(MediaType::Image)
    } else if mime.starts_with("video/") {
        Ok(MediaType::Video)
    } else {
        Err(anyhow!("unsupported media mime type: {mime}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(ext: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yoetz_media_test_{nanos}.{ext}"))
    }

    #[test]
    fn media_input_from_path_image() {
        let path = temp_path("png");
        fs::write(&path, [0u8, 1, 2, 3]).unwrap();
        let input = MediaInput::from_path(&path).unwrap();
        assert_eq!(input.media_type, MediaType::Image);
        assert!(input.mime_type.starts_with("image/"));
        let data_url = input.as_data_url().unwrap();
        assert!(data_url.starts_with("data:image/"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn media_input_from_path_video() {
        let path = temp_path("mp4");
        fs::write(&path, [0u8, 1, 2, 3]).unwrap();
        let input = MediaInput::from_path(&path).unwrap();
        assert_eq!(input.media_type, MediaType::Video);
        assert!(input.mime_type.starts_with("video/"));
        let _ = fs::remove_file(&path);
    }
}

```

File: crates/yoetz-core/src/output.rs (355 tokens)
```
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::io::{self, Write};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Jsonl,
    Text,
    Markdown,
}

impl FromStr for OutputFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "json" => Ok(OutputFormat::Json),
            "jsonl" => Ok(OutputFormat::Jsonl),
            "text" => Ok(OutputFormat::Text),
            "markdown" | "md" => Ok(OutputFormat::Markdown),
            _ => Err(anyhow!("unknown format: {s}")),
        }
    }
}

pub fn write_json<T: Serialize>(value: &T) -> Result<()> {
    let data = serde_json::to_string_pretty(value)?;
    println!("{data}");
    Ok(())
}

pub fn write_jsonl_event<T: Serialize>(event: &T) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let line = serde_json::to_string(event)?;
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[derive(Serialize)]
struct JsonlEvent<'a, T> {
    #[serde(rename = "type")]
    kind: &'a str,
    data: &'a T,
}

pub fn write_jsonl<T: Serialize>(kind: &str, data: &T) -> Result<()> {
    let event = JsonlEvent { kind, data };
    write_jsonl_event(&event)
}

```

File: crates/yoetz-core/src/registry.rs (424 tokens)
```
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPricing {
    pub prompt_per_1k: Option<f64>,
    pub completion_per_1k: Option<f64>,
    pub request: Option<f64>,
}

impl ModelPricing {
    pub fn estimate(&self, input_tokens: usize, output_tokens: usize) -> Option<f64> {
        let prompt_cost = self
            .prompt_per_1k
            .map(|p| p * input_tokens as f64 / 1000.0)?;
        let completion_cost = self
            .completion_per_1k
            .map(|c| c * output_tokens as f64 / 1000.0)?;
        let request_cost = self.request.unwrap_or(0.0);
        Some(prompt_cost + completion_cost + request_cost)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelEntry {
    pub id: String,
    pub context_length: Option<usize>,
    pub pricing: ModelPricing,
    pub provider: Option<String>,
    pub capability: Option<ModelCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelCapability {
    pub vision: Option<bool>,
    pub reasoning: Option<bool>,
    pub web_search: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelRegistry {
    pub version: u32,
    pub updated_at: Option<String>,
    pub models: Vec<ModelEntry>,
}

impl ModelRegistry {
    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn merge(&mut self, other: ModelRegistry) {
        for m in other.models {
            if let Some(existing) = self.models.iter_mut().find(|x| x.id == m.id) {
                *existing = m;
            } else {
                self.models.push(m);
            }
        }
    }
}

```

File: crates/yoetz-core/src/session.rs (585 tokens)
```
use crate::types::SessionInfo;
use anyhow::{Context, Result};
use rand::{distributions::Alphanumeric, Rng};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

static TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year][month][day]_[hour][minute][second]");

pub fn create_session_dir() -> Result<SessionInfo> {
    let base = session_base_dir();
    fs::create_dir_all(&base).with_context(|| format!("create sessions dir {}", base.display()))?;

    let ts = OffsetDateTime::now_utc()
        .format(TS_FORMAT)
        .unwrap_or_else(|_| "unknown".to_string());
    let rand: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    let id = format!("{ts}_{rand}");
    let path = base.join(&id);
    fs::create_dir_all(&path).with_context(|| format!("create session {}", path.display()))?;

    Ok(SessionInfo { id, path })
}

pub fn session_base_dir() -> PathBuf {
    if let Ok(dir) = env::var("YOETZ_DIR") {
        return PathBuf::from(dir).join("sessions");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".yoetz/sessions");
    }
    PathBuf::from(".yoetz/sessions")
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let data = serde_json::to_string_pretty(value)?;
    fs::write(path, data).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn list_sessions() -> Result<Vec<SessionInfo>> {
    let base = session_base_dir();
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(&base).with_context(|| format!("read {}", base.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let id = entry.file_name().to_string_lossy().to_string();
            items.push(SessionInfo {
                id,
                path: entry.path(),
            });
        }
    }
    items.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(items)
}

```

File: crates/yoetz-core/src/types.rs (541 tokens)
```
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::media::MediaOutput;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub prompt: String,
    pub files: Vec<BundleFile>,
    pub stats: BundleStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleFile {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
    pub truncated: bool,
    pub is_binary: bool,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleStats {
    pub file_count: usize,
    pub total_bytes: usize,
    pub total_chars: usize,
    pub estimated_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingEstimate {
    pub estimate_usd: Option<f64>,
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub pricing_source: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub thoughts_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub bundle: Option<Bundle>,
    pub pricing: PricingEstimate,
    pub usage: Usage,
    pub content: String,
    pub artifacts: ArtifactPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactPaths {
    pub session_dir: String,
    pub bundle_json: Option<String>,
    pub bundle_md: Option<String>,
    pub response_json: Option<String>,
    pub media_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaGenerationResult {
    pub id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub usage: Usage,
    pub artifacts: ArtifactPaths,
    pub outputs: Vec<MediaOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleResult {
    pub id: String,
    pub bundle: Bundle,
    pub artifacts: ArtifactPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
}

```

File: README.md (1315 tokens)
```
# yoetz

[![CI](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml/badge.svg)](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

Fast, CLI-first LLM council + bundler + multimodal gateway for coding agents.

> **Note**: This project is under active development. APIs may change.

## Features

- **Bundle**: Package code files with gitignore-awareness for LLM context
- **Ask**: Query LLMs with text, images, or video
- **Council**: Multi-model consensus with configurable voting
- **Review**: AI-powered code review for diffs and files
- **Generate**: Create images (OpenAI) and videos (Sora, Veo)
- **Browser**: Fallback to web UIs via recipes

## Installation

### From Source

```bash
cargo install --git https://github.com/avivsinai/yoetz
```

### Build Locally

```bash
git clone https://github.com/avivsinai/yoetz.git
cd yoetz
cargo build --release
```

## Quick Start

### Configuration

Create `~/.yoetz/config.toml`:

```toml
[defaults]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-5-20250929"

[providers.openrouter]
api_key_env = "OPENROUTER_API_KEY"

[providers.openai]
api_key_env = "OPENAI_API_KEY"

[providers.gemini]
api_key_env = "GEMINI_API_KEY"
```

### Basic Usage

```bash
# Bundle files for LLM context
yoetz bundle --prompt "Review this code" --files "src/**/*.rs"

# Ask a question
yoetz ask --prompt "Explain this function" --files "src/main.rs"

# Ask with structured JSON output (OpenAI-compatible)
yoetz ask --prompt "Return JSON only" --provider openai --model gpt-5.2 --response-format json

# Ask with an image (vision)
yoetz ask --prompt "Describe this diagram" --image diagram.png --provider gemini --model gemini-3-flash-preview

# Ask about a video
yoetz ask --prompt "Summarize this" --video meeting.mp4 --provider gemini --model gemini-3-flash-preview

> Note: Gemini can return empty content if `--max-output-tokens` is too low because tokens are consumed by thoughts. If you see warnings or empty output, increase the limit.

# Debug raw provider responses
yoetz --debug ask --provider gemini --model gemini-3-flash-preview --prompt "ping"

# Multi-model council
yoetz council --prompt "Review this PR" --models "openai/gpt-5.2-codex,anthropic/claude-sonnet-4-5-20250929"

# Code review
yoetz review diff --model openai/gpt-5.2-codex
yoetz review file --path src/lib.rs --model anthropic/claude-sonnet-4-5-20250929
```

### Generation

```bash
# Generate images
yoetz generate image --prompt "A cozy cabin in snow" --provider openai --model gpt-image-1.5

# Generate video (Sora)
yoetz generate video --prompt "Drone flyover" --provider openai --model sora-2-pro

# Generate video (Veo)
yoetz generate video --prompt "Ocean waves" --provider gemini --model veo-3.1-generate-preview
```

### Browser Fallback

```bash
# Direct browser command
yoetz browser exec -- open https://chatgpt.com/

# Use a recipe
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle bundle.md
```

## Architecture

```
yoetz/
├── crates/
│   ├── yoetz-core/       # Core library
│   │   ├── bundle.rs     # File bundling with gitignore
│   │   ├── config.rs     # TOML config loading + profiles
│   │   ├── media.rs      # Media types for multimodal
│   │   └── types.rs      # Shared types
│   ├── litellm-rust/       # LiteLLM-style SDK (library only)
│   └── yoetz-cli/        # CLI binary
│       ├── main.rs       # Command handlers
│       ├── providers/    # OpenAI, Gemini implementations
│       ├── registry.rs   # Model registry (OpenRouter, LiteLLM)
│       └── budget.rs     # Daily spend tracking
├── recipes/              # Browser automation YAML
└── docs/                 # Configuration examples
```

## Supported Providers

| Provider | Text | Vision | Image Gen | Video Gen | Video Understanding |
|----------|------|--------|-----------|-----------|---------------------|
| OpenRouter | ✅ | via model | - | - | - |
| OpenAI | ✅ | ✅ | ✅ | ✅ (Sora) | - |
| Gemini | ✅ | ✅ | - | ✅ (Veo) | ✅ |
| LiteLLM | ✅ | via model | - | - | - |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `GEMINI_API_KEY` | Google Gemini API key |
| `LITELLM_API_KEY` | LiteLLM proxy key |
| `YOETZ_CONFIG_PATH` | Custom config path |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

[MIT](LICENSE)

```

</files>

