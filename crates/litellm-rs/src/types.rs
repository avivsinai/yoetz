use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;

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
    pub kind: Cow<'static, str>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatContentPartImageUrl {
    #[serde(rename = "type")]
    pub kind: Cow<'static, str>,
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
    pub kind: Cow<'static, str>,
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
    pub kind: Cow<'static, str>,
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

/// Token usage statistics from an LLM API response.
///
/// Uses `u64` for token counts to handle large values consistently
/// across different platforms and avoid truncation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub thoughts_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
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
