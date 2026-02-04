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
                .map_err(LiteLLMError::from)?,
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
