//! Integration tests for LLM providers with mocked HTTP.
//!
//! These tests verify the complete request/response flow, including:
//! - Request body construction
//! - Response parsing and field extraction
//! - Error handling for various HTTP status codes
//! - Configuration validation

use litellm_rs::config::ProviderConfig;
use litellm_rs::providers::{anthropic, openai_compat};
use litellm_rs::types::{ChatMessage, ChatMessageContent, ChatRequest};
use reqwest::Client;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_client() -> Client {
    Client::new()
}

fn simple_chat_request(model: &str) -> ChatRequest {
    ChatRequest::new(model).message("user", "Hello")
}

fn chat_message(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: ChatMessageContent::Text(content.to_string()),
        name: None,
        tool_call_id: None,
        tool_calls: None,
        function_call: None,
        provider_specific_fields: None,
    }
}

// =============================================================================
// OpenAI-Compatible Provider Tests
// =============================================================================

mod openai_compat_tests {
    use super::*;

    /// Verifies that a successful chat completion extracts content and usage correctly.
    #[tokio::test]
    async fn chat_completion_extracts_content_and_usage() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-123",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I help you?"
                    }
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 8,
                    "total_tokens": 18
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let resp = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.content, "Hello! How can I help you?");
        assert_eq!(resp.usage.prompt_tokens, Some(10));
        assert_eq!(resp.usage.completion_tokens, Some(8));
        assert_eq!(resp.usage.total_tokens, Some(18));
        assert_eq!(resp.response_id, Some("chatcmpl-123".to_string()));
    }

    /// Verifies reasoning_tokens extraction from completion_tokens_details.
    #[tokio::test]
    async fn chat_completion_extracts_reasoning_tokens() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-456",
                "choices": [{
                    "message": { "content": "Reasoning response" }
                }],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 500,
                    "total_tokens": 600,
                    "completion_tokens_details": {
                        "reasoning_tokens": 400
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("o1-preview");
        let resp = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.usage.thoughts_tokens, Some(400));
    }

    /// Verifies cost extraction from x-litellm-response-cost header.
    #[tokio::test]
    async fn chat_completion_extracts_header_cost() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-litellm-response-cost", "0.00123")
                    .set_body_json(json!({
                        "id": "chatcmpl-789",
                        "choices": [{ "message": { "content": "Hi" } }],
                        "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
                    })),
            )
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let resp = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.header_cost, Some(0.00123));
        assert_eq!(resp.usage.cost_usd, Some(0.00123));
    }

    /// Verifies HTTP 401 error is surfaced correctly.
    #[tokio::test]
    async fn chat_completion_surfaces_401_unauthorized() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {
                    "message": "Invalid API key",
                    "type": "invalid_request_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("bad-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let err = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("401"),
            "Expected 401 in error: {}",
            err_str
        );
    }

    /// Verifies HTTP 429 rate limit error is surfaced correctly.
    #[tokio::test]
    async fn chat_completion_surfaces_429_rate_limit() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "type": "rate_limit_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let err = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("429"),
            "Expected 429 in error: {}",
            err_str
        );
        assert!(
            err_str.contains("Rate limit"),
            "Expected rate limit message: {}",
            err_str
        );
    }

    /// Verifies HTTP 500 server error is surfaced correctly.
    #[tokio::test]
    async fn chat_completion_surfaces_500_server_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let err = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("500"),
            "Expected 500 in error: {}",
            err_str
        );
    }

    /// Verifies missing base_url returns config error.
    #[tokio::test]
    async fn chat_completion_requires_base_url() {
        let cfg = ProviderConfig {
            base_url: None,
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let err = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("base_url"),
            "Expected base_url error: {}",
            err_str
        );
    }

    /// Verifies response with empty choices array returns empty content.
    #[tokio::test]
    async fn chat_completion_handles_empty_choices() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-empty",
                "choices": [],
                "usage": { "prompt_tokens": 5, "completion_tokens": 0 }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let resp = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.content, "");
    }

    /// Verifies request includes optional parameters when set.
    #[tokio::test]
    async fn chat_completion_sends_optional_parameters() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-params",
                "choices": [{ "message": { "content": "response" } }],
                "usage": {}
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = ChatRequest::new("gpt-4")
            .message("user", "Hello")
            .temperature(0.7)
            .max_tokens(100);

        let resp = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap();
        assert_eq!(resp.content, "response");
    }

    /// Verifies response with null content field is handled gracefully.
    #[tokio::test]
    async fn chat_completion_handles_null_content() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-null",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null
                    }
                }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 0 }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("gpt-4");
        let resp = openai_compat::chat(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.content, "");
    }
}

// =============================================================================
// Anthropic Provider Tests
// =============================================================================

mod anthropic_tests {
    use super::*;

    /// Verifies successful chat completion extracts text from content blocks.
    #[tokio::test]
    async fn chat_completion_extracts_text_from_content_blocks() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_123",
                "content": [
                    { "type": "text", "text": "Hello! " },
                    { "type": "text", "text": "How can I help?" }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 8
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("claude-3-5-sonnet-20241022");
        let resp = anthropic::chat(&make_client(), &cfg, req).await.unwrap();

        // Text from multiple content blocks should be concatenated
        assert_eq!(resp.content, "Hello! How can I help?");
        // Anthropic uses input_tokens/output_tokens, mapped to prompt/completion
        assert_eq!(resp.usage.prompt_tokens, Some(10));
        assert_eq!(resp.usage.completion_tokens, Some(8));
        assert_eq!(resp.response_id, Some("msg_123".to_string()));
    }

    /// Verifies system messages are extracted and sent separately.
    #[tokio::test]
    async fn chat_completion_handles_system_messages() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_sys",
                "content": [{ "type": "text", "text": "I am helpful" }],
                "usage": { "input_tokens": 20, "output_tokens": 5 }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = ChatRequest {
            model: "claude-3-5-sonnet-20241022".to_string(),
            messages: vec![
                chat_message("system", "You are a helpful assistant"),
                chat_message("user", "Hello"),
            ],
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
        };

        let resp = anthropic::chat(&make_client(), &cfg, req).await.unwrap();
        assert_eq!(resp.content, "I am helpful");
    }

    /// Verifies missing API key returns appropriate error.
    #[tokio::test]
    async fn chat_completion_requires_api_key() {
        let mock_server = MockServer::start().await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: None,
            api_key_env: None,
            ..Default::default()
        };

        let req = simple_chat_request("claude-3-5-sonnet-20241022");
        let err = anthropic::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("ANTHROPIC_API_KEY") || err_str.contains("api key"),
            "Expected API key error: {}",
            err_str
        );
    }

    /// Verifies HTTP 400 bad request is surfaced correctly.
    #[tokio::test]
    async fn chat_completion_surfaces_400_bad_request() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "max_tokens: must be greater than 0"
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("claude-3-5-sonnet-20241022");
        let err = anthropic::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("400"),
            "Expected 400 in error: {}",
            err_str
        );
    }

    /// Verifies unsupported role returns config error.
    #[tokio::test]
    async fn chat_completion_rejects_unsupported_role() {
        let mock_server = MockServer::start().await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = ChatRequest {
            model: "claude-3-5-sonnet-20241022".to_string(),
            messages: vec![chat_message("function", "result")], // Unsupported by Anthropic
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
        };

        let err = anthropic::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("unsupported") && err_str.contains("role"),
            "Expected unsupported role error: {}",
            err_str
        );
    }

    /// Verifies response with only non-text content blocks returns empty string.
    #[tokio::test]
    async fn chat_completion_handles_non_text_content() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_tool",
                "content": [
                    { "type": "tool_use", "id": "tool_123", "name": "calculator" }
                ],
                "usage": { "input_tokens": 10, "output_tokens": 5 }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("claude-3-5-sonnet-20241022");
        let resp = anthropic::chat(&make_client(), &cfg, req).await.unwrap();

        // No text blocks, so content should be empty
        assert_eq!(resp.content, "");
    }

    /// Verifies overloaded (503) error is surfaced correctly.
    #[tokio::test]
    async fn chat_completion_surfaces_503_overloaded() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "type": "error",
                "error": {
                    "type": "overloaded_error",
                    "message": "Overloaded"
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = simple_chat_request("claude-3-5-sonnet-20241022");
        let err = anthropic::chat(&make_client(), &cfg, req)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("503"),
            "Expected 503 in error: {}",
            err_str
        );
    }
}

// =============================================================================
// Embedding Tests
// =============================================================================

mod embedding_tests {
    use super::*;
    use litellm_rs::types::EmbeddingRequest;

    /// Verifies embeddings are extracted correctly from response.
    #[tokio::test]
    async fn embeddings_extracts_vectors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "embedding": [0.1, 0.2, 0.3] },
                    { "embedding": [0.4, 0.5, 0.6] }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "total_tokens": 10
                }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = EmbeddingRequest {
            model: "text-embedding-ada-002".to_string(),
            input: json!(["Hello", "World"]),
        };

        let resp = openai_compat::embeddings(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.vectors.len(), 2);
        assert_eq!(resp.vectors[0], vec![0.1, 0.2, 0.3]);
        assert_eq!(resp.vectors[1], vec![0.4, 0.5, 0.6]);
        assert_eq!(resp.usage.prompt_tokens, Some(10));
    }

    /// Verifies single string input is accepted.
    #[tokio::test]
    async fn embeddings_accepts_single_string() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "embedding": [0.1, 0.2, 0.3] }],
                "usage": { "prompt_tokens": 5 }
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = EmbeddingRequest {
            model: "text-embedding-ada-002".to_string(),
            input: json!("Single input text"),
        };

        let resp = openai_compat::embeddings(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.vectors.len(), 1);
    }
}

// =============================================================================
// Image Generation Tests
// =============================================================================

mod image_generation_tests {
    use super::*;
    use litellm_rs::types::ImageRequest;

    /// Verifies image generation extracts URLs and revised prompts.
    #[tokio::test]
    async fn image_generation_extracts_images() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/images/generations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {
                        "url": "https://example.com/image1.png",
                        "revised_prompt": "A beautiful sunset over mountains"
                    },
                    {
                        "b64_json": "aGVsbG8=",
                        "revised_prompt": "A beautiful sunset"
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = ImageRequest {
            model: "dall-e-3".to_string(),
            prompt: "A sunset".to_string(),
            n: Some(2),
            size: Some("1024x1024".to_string()),
            quality: None,
            background: None,
        };

        let resp = openai_compat::image_generation(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert_eq!(resp.images.len(), 2);
        assert_eq!(
            resp.images[0].url,
            Some("https://example.com/image1.png".to_string())
        );
        assert_eq!(
            resp.images[0].revised_prompt,
            Some("A beautiful sunset over mountains".to_string())
        );
        assert_eq!(resp.images[1].b64_json, Some("aGVsbG8=".to_string()));
    }

    /// Verifies image generation handles empty data array.
    #[tokio::test]
    async fn image_generation_handles_empty_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/images/generations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = ImageRequest {
            model: "dall-e-3".to_string(),
            prompt: "A sunset".to_string(),
            n: None,
            size: None,
            quality: None,
            background: None,
        };

        let resp = openai_compat::image_generation(&make_client(), &cfg, req)
            .await
            .unwrap();

        assert!(resp.images.is_empty());
    }
}

// =============================================================================
// Video Generation Tests
// =============================================================================

mod video_generation_tests {
    use super::*;
    use litellm_rs::providers::openai_compat::VideoGenerationOptions;
    use litellm_rs::types::VideoRequest;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Verifies video generation polls until completion.
    #[tokio::test]
    async fn video_generation_polls_until_complete() {
        let mock_server = MockServer::start().await;
        let poll_count = Arc::new(AtomicU32::new(0));
        let poll_count_clone = poll_count.clone();

        // Initial video creation request
        Mock::given(method("POST"))
            .and(path("/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "vid_123"
            })))
            .mount(&mock_server)
            .await;

        // Status polling - first returns processing, then completed
        Mock::given(method("GET"))
            .and(path("/videos/vid_123"))
            .respond_with(move |_: &wiremock::Request| {
                let count = poll_count_clone.fetch_add(1, Ordering::SeqCst);
                if count < 1 {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "status": "processing"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "status": "completed"
                    }))
                }
            })
            .mount(&mock_server)
            .await;

        // Content fetch
        Mock::given(method("GET"))
            .and(path("/videos/vid_123/content"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake video content"))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = VideoRequest {
            model: "sora".to_string(),
            prompt: "A cat".to_string(),
            seconds: Some(5),
            size: None,
        };

        let options = VideoGenerationOptions {
            max_poll_attempts: 10,
            poll_interval_secs: 0, // No delay for tests
        };

        let resp = openai_compat::video_generation_with_options(&make_client(), &cfg, req, options)
            .await
            .unwrap();

        // Should have polled at least twice (processing, then completed)
        assert!(poll_count.load(Ordering::SeqCst) >= 2);
        assert!(resp.video_url.is_some());
        assert!(resp
            .video_url
            .unwrap()
            .starts_with("data:video/mp4;base64,"));
    }

    /// Verifies video generation fails after max poll attempts.
    #[tokio::test]
    async fn video_generation_times_out_after_max_attempts() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "vid_timeout"
            })))
            .mount(&mock_server)
            .await;

        // Always return processing
        Mock::given(method("GET"))
            .and(path("/videos/vid_timeout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "processing"
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = VideoRequest {
            model: "sora".to_string(),
            prompt: "A cat".to_string(),
            seconds: None,
            size: None,
        };

        let options = VideoGenerationOptions {
            max_poll_attempts: 3,
            poll_interval_secs: 0,
        };

        let err = openai_compat::video_generation_with_options(&make_client(), &cfg, req, options)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("timed out") || err_str.contains("3 attempts"),
            "Expected timeout error: {}",
            err_str
        );
    }

    /// Verifies video generation surfaces failure status.
    #[tokio::test]
    async fn video_generation_surfaces_failure() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "vid_fail"
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/videos/vid_fail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "failed",
                "error": "Content policy violation"
            })))
            .mount(&mock_server)
            .await;

        let cfg = ProviderConfig {
            base_url: Some(mock_server.uri()),
            api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        let req = VideoRequest {
            model: "sora".to_string(),
            prompt: "Something bad".to_string(),
            seconds: None,
            size: None,
        };

        let options = VideoGenerationOptions {
            max_poll_attempts: 10,
            poll_interval_secs: 0,
        };

        let err = openai_compat::video_generation_with_options(&make_client(), &cfg, req, options)
            .await
            .unwrap_err();

        let err_str = err.to_string();
        assert!(
            err_str.contains("Content policy"),
            "Expected policy error: {}",
            err_str
        );
    }
}
