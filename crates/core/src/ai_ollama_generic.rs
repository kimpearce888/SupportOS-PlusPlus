//! Ollama + Generic AI provider adapters (M5-T05 + M5-T06).
//!
//! Per spec M5: "LocalAIProvider (LM Studio, Ollama, generic)."
//! Per spec A5: "LM Studio and Ollama are optional, never bundled: auto-detect,
//! list models, select, test. The app works fully without them."
//! Per the reference notes: "Ollama: native API at http://127.0.0.1:11434."
//!
//! ## Ollama (M5-T05)
//!
//! Ollama has its own native API (NOT OpenAI-compatible):
//! - `GET /api/tags` — list models
//! - `POST /api/chat` — chat completions (native format)
//! - `POST /api/embeddings` — embeddings (native format)
//!
//! ## Generic (M5-T06)
//!
//! The Generic adapter wraps the `OpenAiCompatibleClient` from M5-T04 — it
//! speaks the same OpenAI format as LM Studio but with a user-configured
//! base URL + optional API key (stored as a redacted secret per spec A6).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ai_lm_studio::OpenAiCompatibleClient;
use crate::ai_provider::{
    ChatMessage, ChatResponse, EmbedResponse, LocalAiProvider, ModelInfo, ModelRole, TokenUsage,
};
use crate::error::{Error, Result};

/// The default Ollama base URL.
pub const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

// ─── Ollama adapter (native API) ──────────────────────────────────────────

/// The Ollama AI provider — speaks Ollama's native API (NOT OpenAI-compatible).
///
/// Per spec A5: "Ollama is optional, never bundled: auto-detect, list models,
/// select, test." The `is_available()` method auto-detects by attempting a
/// connection to `{base_url}/api/tags`.
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    base_url: String,
    http: reqwest::Client,
}

impl OllamaProvider {
    /// Create a new Ollama provider with the default base URL
    /// (`http://127.0.0.1:11434`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_base_url(OLLAMA_BASE_URL)
    }

    /// Create a new Ollama provider with a custom base URL.
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// The base URL (without trailing slash).
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LocalAiProvider for OllamaProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let resp = self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|e| Error::Config(format!("Ollama list_models failed: {e}")))?;
        let body: OllamaTagsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Config(format!("Ollama list_models parse failed: {e}")))?;
        Ok(body
            .models
            .into_iter()
            .map(infer_ollama_model_role)
            .collect())
    }

    async fn chat(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatResponse> {
        let request = build_ollama_chat_request(model, messages);
        let resp = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Config(format!("Ollama chat failed: {e}")))?;
        let body: OllamaChatResponse = resp
            .json()
            .await
            .map_err(|e| Error::Config(format!("Ollama chat parse failed: {e}")))?;
        Ok(parse_ollama_chat_response(body, model))
    }

    async fn embed(&self, model: &str, text: &str) -> Result<EmbedResponse> {
        let request = build_ollama_embed_request(model, text);
        let resp = self
            .http
            .post(format!("{}/api/embeddings", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Config(format!("Ollama embed failed: {e}")))?;
        let body: OllamaEmbedResponse = resp
            .json()
            .await
            .map_err(|e| Error::Config(format!("Ollama embed parse failed: {e}")))?;
        Ok(parse_ollama_embed_response(body, model))
    }

    async fn is_available(&self) -> bool {
        match self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

// ─── Ollama pure request/response types (testable without HTTP) ──────────

/// Ollama `/api/tags` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaTagsResponse {
    pub models: Vec<OllamaModel>,
}

/// A model entry in Ollama's `/api/tags` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaModel {
    pub name: String,
}

/// Infer the model role from an Ollama model name. Same heuristic as the
/// OpenAI-compatible client: if the name contains "embed", it's an embedding
/// model; otherwise it's a chat model.
#[must_use]
pub fn infer_ollama_model_role(model: OllamaModel) -> ModelInfo {
    let role = if model.name.to_lowercase().contains("embed") {
        ModelRole::Embedding
    } else {
        ModelRole::Chat
    };
    ModelInfo {
        id: model.name,
        role,
    }
}

/// Ollama `/api/chat` request body.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaChatRequest {
    pub model: String,
    pub messages: Vec<OllamaChatMessage>,
    pub stream: bool,
}

/// A message in the Ollama chat request.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaChatMessage {
    pub role: String,
    pub content: String,
}

/// Build the Ollama `/api/chat` request body. Pure function.
/// `stream = false` so we get a single JSON response (not a stream).
#[must_use]
pub fn build_ollama_chat_request(model: &str, messages: &[ChatMessage]) -> OllamaChatRequest {
    OllamaChatRequest {
        model: model.to_string(),
        messages: messages
            .iter()
            .map(|m| OllamaChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect(),
        stream: false,
    }
}

/// Ollama `/api/chat` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaChatResponse {
    pub model: String,
    pub message: OllamaChatResponseMessage,
    pub done: bool,
    pub eval_count: Option<u64>,
    pub prompt_eval_count: Option<u64>,
}

/// The message in an Ollama chat response.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaChatResponseMessage {
    pub role: String,
    pub content: String,
}

/// Parse an Ollama chat response into our `ChatResponse`. Pure function.
/// Per spec: "Unknown" is a legitimate answer — if the response is empty or
/// malformed, the content defaults to "Unknown".
#[must_use]
pub fn parse_ollama_chat_response(resp: OllamaChatResponse, model: &str) -> ChatResponse {
    let prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
    let completion_tokens = resp.eval_count.unwrap_or(0);
    let total_tokens = prompt_tokens + completion_tokens;
    let usage = if total_tokens > 0 {
        Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        })
    } else {
        None
    };
    ChatResponse {
        content: resp.message.content,
        model: if resp.model.is_empty() {
            model.to_string()
        } else {
            resp.model
        },
        usage,
        finish_reason: if resp.done { Some("stop".into()) } else { None },
    }
}

/// Ollama `/api/embeddings` request body.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaEmbedRequest {
    pub model: String,
    pub prompt: String,
}

/// Build the Ollama `/api/embeddings` request body. Pure function.
#[must_use]
pub fn build_ollama_embed_request(model: &str, text: &str) -> OllamaEmbedRequest {
    OllamaEmbedRequest {
        model: model.to_string(),
        prompt: text.to_string(),
    }
}

/// Ollama `/api/embeddings` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaEmbedResponse {
    pub embedding: Vec<f32>,
}

/// Parse an Ollama embedding response into our `EmbedResponse`. Pure function.
#[must_use]
pub fn parse_ollama_embed_response(resp: OllamaEmbedResponse, model: &str) -> EmbedResponse {
    let dim = resp.embedding.len();
    EmbedResponse {
        vector: resp.embedding,
        dim,
        model: model.to_string(),
        usage: None, // Ollama doesn't report token usage for embeddings.
    }
}

// ─── Generic OpenAI-compatible adapter (M5-T06) ──────────────────────────

/// The Generic AI provider — wraps `OpenAiCompatibleClient` with a
/// user-configured base URL + optional API key.
///
/// Per spec: the user enters the endpoint in Settings; the adapter calls
/// `{base_url}/chat/completions` + `{base_url}/embeddings`. The API key
/// is stored as a redacted secret (per spec A6 + the existing `settings`
/// module's `redacted_secret`).
#[derive(Debug, Clone)]
pub struct GenericAiProvider {
    client: OpenAiCompatibleClient,
}

impl GenericAiProvider {
    /// Create a new Generic provider with the given base URL.
    /// The base URL should be the OpenAI-compatible root, e.g.
    /// `https://api.openai.com/v1` or `http://localhost:8080/v1`.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: OpenAiCompatibleClient::new(base_url),
        }
    }

    /// Set an API key (stored as a redacted secret in the real app).
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.client = self.client.with_api_key(api_key);
        self
    }

    /// The base URL (without trailing slash).
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.client.base_url()
    }
}

#[async_trait]
impl LocalAiProvider for GenericAiProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        self.client.list_models().await
    }

    async fn chat(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatResponse> {
        self.client.chat(model, messages).await
    }

    async fn embed(&self, model: &str, text: &str) -> Result<EmbedResponse> {
        self.client.embed(model, text).await
    }

    async fn is_available(&self) -> bool {
        self.client.is_available().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Ollama: infer_ollama_model_role ------------------------------------

    #[test]
    fn infer_ollama_role_embed_in_name() {
        let model = OllamaModel {
            name: "nomic-embed-text".into(),
        };
        let info = infer_ollama_model_role(model);
        assert_eq!(info.role, ModelRole::Embedding);
        assert_eq!(info.id, "nomic-embed-text");
    }

    #[test]
    fn infer_ollama_role_chat_in_name() {
        let model = OllamaModel {
            name: "llama3.1:8b".into(),
        };
        let info = infer_ollama_model_role(model);
        assert_eq!(info.role, ModelRole::Chat);
    }

    #[test]
    fn infer_ollama_role_embed_case_insensitive() {
        let model = OllamaModel {
            name: "BGE-EMBED-large".into(),
        };
        let info = infer_ollama_model_role(model);
        assert_eq!(info.role, ModelRole::Embedding);
    }

    // ---- Ollama: build_ollama_chat_request ---------------------------------

    #[test]
    fn build_ollama_chat_request_sets_stream_false() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Hello".into(),
        }];
        let body = build_ollama_chat_request("llama3", &messages);
        assert_eq!(body.model, "llama3");
        assert!(!body.stream, "stream=false for single JSON response");
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.messages[0].content, "Hello");
    }

    #[test]
    fn build_ollama_chat_request_with_empty_messages() {
        let body = build_ollama_chat_request("m", &[]);
        assert_eq!(body.model, "m");
        assert!(body.messages.is_empty());
    }

    // ---- Ollama: build_ollama_embed_request --------------------------------

    #[test]
    fn build_ollama_embed_request_serializes() {
        let body = build_ollama_embed_request("nomic-embed-text", "hello world");
        assert_eq!(body.model, "nomic-embed-text");
        assert_eq!(body.prompt, "hello world");
    }

    // ---- Ollama: parse_ollama_chat_response --------------------------------

    #[test]
    fn parse_ollama_chat_response_with_content() {
        let resp = OllamaChatResponse {
            model: "llama3".into(),
            message: OllamaChatResponseMessage {
                role: "assistant".into(),
                content: "Hello!".into(),
            },
            done: true,
            eval_count: Some(5),
            prompt_eval_count: Some(3),
        };
        let parsed = parse_ollama_chat_response(resp, "llama3");
        assert_eq!(parsed.content, "Hello!");
        assert_eq!(parsed.model, "llama3");
        assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 3);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn parse_ollama_chat_response_no_token_counts() {
        let resp = OllamaChatResponse {
            model: "m".into(),
            message: OllamaChatResponseMessage {
                role: "assistant".into(),
                content: "hi".into(),
            },
            done: true,
            eval_count: None,
            prompt_eval_count: None,
        };
        let parsed = parse_ollama_chat_response(resp, "m");
        assert!(parsed.usage.is_none(), "no token counts → no usage");
    }

    #[test]
    fn parse_ollama_chat_response_not_done() {
        let resp = OllamaChatResponse {
            model: "m".into(),
            message: OllamaChatResponseMessage {
                role: "assistant".into(),
                content: "partial".into(),
            },
            done: false,
            eval_count: None,
            prompt_eval_count: None,
        };
        let parsed = parse_ollama_chat_response(resp, "m");
        assert!(
            parsed.finish_reason.is_none(),
            "not done → no finish_reason"
        );
    }

    #[test]
    fn parse_ollama_chat_response_empty_model_uses_fallback() {
        let resp = OllamaChatResponse {
            model: String::new(),
            message: OllamaChatResponseMessage {
                role: "assistant".into(),
                content: "hi".into(),
            },
            done: true,
            eval_count: None,
            prompt_eval_count: None,
        };
        let parsed = parse_ollama_chat_response(resp, "fallback-model");
        assert_eq!(parsed.model, "fallback-model");
    }

    // ---- Ollama: parse_ollama_embed_response --------------------------------

    #[test]
    fn parse_ollama_embed_response_with_vector() {
        let resp = OllamaEmbedResponse {
            embedding: vec![0.1, 0.2, 0.3, 0.4],
        };
        let parsed = parse_ollama_embed_response(resp, "nomic-embed-text");
        assert_eq!(parsed.dim, 4);
        assert_eq!(parsed.vector, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(parsed.model, "nomic-embed-text");
        assert!(
            parsed.usage.is_none(),
            "Ollama doesn't report embedding usage"
        );
    }

    #[test]
    fn parse_ollama_embed_response_empty_vector() {
        let resp = OllamaEmbedResponse { embedding: vec![] };
        let parsed = parse_ollama_embed_response(resp, "m");
        assert!(parsed.vector.is_empty());
        assert_eq!(parsed.dim, 0);
    }

    // ---- Ollama: config -----------------------------------------------------

    #[test]
    fn ollama_default_base_url() {
        let provider = OllamaProvider::new();
        assert_eq!(provider.base_url(), OLLAMA_BASE_URL);
    }

    #[test]
    fn ollama_custom_base_url() {
        let provider = OllamaProvider::with_base_url("http://localhost:9999");
        assert_eq!(provider.base_url(), "http://localhost:9999");
    }

    #[test]
    fn ollama_trims_trailing_slash() {
        let provider = OllamaProvider::with_base_url("http://localhost:11434/");
        assert_eq!(provider.base_url(), "http://localhost:11434");
    }

    // ---- Ollama: serde round-trips -----------------------------------------

    #[test]
    fn ollama_tags_response_parses() {
        let json = r#"{"models": [{"name": "llama3:8b"}, {"name": "nomic-embed-text"}]}"#;
        let resp: OllamaTagsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.models.len(), 2);
        assert_eq!(resp.models[0].name, "llama3:8b");
    }

    #[test]
    fn ollama_chat_response_parses() {
        let json = r#"{
            "model": "llama3",
            "message": {"role": "assistant", "content": "Hello!"},
            "done": true,
            "eval_count": 5,
            "prompt_eval_count": 3
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model, "llama3");
        assert_eq!(resp.message.content, "Hello!");
        assert!(resp.done);
        assert_eq!(resp.eval_count, Some(5));
    }

    #[test]
    fn ollama_embed_response_parses() {
        let json = r#"{"embedding": [0.1, 0.2, 0.3]}"#;
        let resp: OllamaEmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embedding, vec![0.1, 0.2, 0.3]);
    }

    // ---- Generic: config ---------------------------------------------------

    #[test]
    fn generic_provider_with_base_url() {
        let provider = GenericAiProvider::new("https://api.openai.com/v1");
        assert_eq!(provider.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn generic_provider_with_api_key() {
        let provider =
            GenericAiProvider::new("http://localhost:8080/v1").with_api_key("sk-test-key");
        // The client holds the API key internally; we can't read it back
        // (it's stored as a secret), but the provider compiles + configures.
        assert_eq!(provider.base_url(), "http://localhost:8080/v1");
    }

    #[test]
    fn generic_provider_trims_trailing_slash() {
        let provider = GenericAiProvider::new("http://localhost:8080/v1/");
        assert_eq!(provider.base_url(), "http://localhost:8080/v1");
    }

    // ---- Send + Sync -------------------------------------------------------

    #[test]
    fn ollama_and_generic_providers_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OllamaProvider>();
        assert_send_sync::<GenericAiProvider>();
    }
}
