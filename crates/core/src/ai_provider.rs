//! LocalAIProvider — the abstraction boundary for local AI providers (M5-T03).
//!
//! Per spec M5: "LocalAIProvider (LM Studio, Ollama, generic)."
//! Per spec A5: "LM Studio and Ollama are optional, never bundled: auto-detect,
//! list models, select, test. The app works fully without them."
//! Per spec: "AI is always advisory. Automatic customer-reply sending is
//! permanently OFF. 'Unknown' is a legitimate answer; never fabricate values."
//!
//! ## Design
//!
//! The trait is async (unlike the sync `VectorStore` trait) because the real
//! providers (LM Studio, Ollama, Generic) make HTTP calls — `reqwest` is async.
//! The `FakeAiProvider` (for tests + demo mode) is sync internally but wrapped
//! in async to satisfy the trait. This is the idiomatic Rust pattern for
//! async traits with a sync test impl.
//!
//! Per spec A12: "no mocks in real mode (the Fake provider exists only for
//! demo mode and tests)." The `FakeAiProvider` is the AI equivalent of the
//! `FakeHelpScoutProvider` from M2 and the `InMemoryVectorStore` from M5-T01.
//!
//! ## Two model roles
//!
//! Per the reference notes (`ai-client-backup-desktop-testing-troubleshooting.md`):
//! - **Chat** (reasoning): the model generates a text response from a conversation.
//! - **Embedding** (vectors): the model converts text to a dense vector.
//!
//! The user picks one model of each role. The embedding dim is read from the
//! model's first response; the VectorStore collection is created with that dim.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A chat message — mirrors the OpenAI chat completions message format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// The role: "system", "user", or "assistant".
    pub role: String,
    /// The message content.
    pub content: String,
}

/// Token usage info — mirrors the OpenAI usage object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Tokens in the prompt.
    pub prompt_tokens: u64,
    /// Tokens in the completion.
    pub completion_tokens: u64,
    /// Total tokens (prompt + completion).
    pub total_tokens: u64,
}

/// The response from a chat completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// The generated text.
    pub content: String,
    /// The model that generated the response.
    pub model: String,
    /// Token usage (if reported by the provider).
    pub usage: Option<TokenUsage>,
    /// The finish reason: "stop", "length", "content_filter", etc.
    /// Per spec: "Unknown" is a legitimate answer — if the provider doesn't
    /// report a finish reason, this is `None`.
    pub finish_reason: Option<String>,
}

/// The response from an embedding request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// The dimension of the vector (convenience — same as `vector.len()`).
    pub dim: usize,
    /// The model that generated the embedding.
    pub model: String,
    /// Token usage (if reported by the provider).
    pub usage: Option<TokenUsage>,
}

/// A model descriptor — returned by `list_models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// The model id (e.g., "llama-3.1-8b-instruct").
    pub id: String,
    /// The model's role: "chat" or "embedding".
    pub role: ModelRole,
}

/// The two model roles a local AI provider supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Chat (reasoning) — generates text from a conversation.
    Chat,
    /// Embedding (vectors) — converts text to a dense vector.
    Embedding,
}

impl ModelRole {
    /// All variants in spec order.
    pub const ALL: [Self; 2] = [Self::Chat, Self::Embedding];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
        }
    }
}

/// The LocalAIProvider trait — the abstraction boundary per spec M5.
///
/// Per spec: "AI is always advisory. Automatic customer-reply sending is
/// permanently OFF. 'Unknown' is a legitimate answer; never fabricate values."
///
/// All methods are async because the real providers make HTTP calls.
/// The `FakeAiProvider` is sync internally but wrapped in async.
#[async_trait]
pub trait LocalAiProvider: Send + Sync {
    /// List available models. Returns models tagged with their role
    /// (chat or embedding). Returns an empty vec if no provider is running
    /// (the app works fully without AI providers per spec A5).
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;

    /// Chat completion: generate a response from a conversation.
    /// `model` is the model id (from `list_models`); `messages` is the
    /// conversation so far (system + user + assistant turns).
    ///
    /// Per spec: "Unknown" is a legitimate answer — if the provider can't
    /// generate a response, return a `ChatResponse` with content = "Unknown"
    /// rather than fabricating.
    async fn chat(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatResponse>;

    /// Embedding: convert text to a dense vector.
    /// `model` is the embedding model id; `text` is the input text.
    /// Returns an `EmbedResponse` with the vector + dim.
    async fn embed(&self, model: &str, text: &str) -> Result<EmbedResponse>;

    /// Whether the provider is currently available (e.g., LM Studio is running
    /// at 127.0.0.1:1234). Per spec A5: "auto-detect, list models, select, test."
    /// Returns `false` if no provider is running (the app works fully without).
    async fn is_available(&self) -> bool;
}

// ─── FakeAiProvider (for tests + demo mode) ───────────────────────────────

/// A deterministic Fake AI provider — for tests + demo mode.
/// Returns canned responses keyed by input hash so tests are reproducible.
///
/// Per spec A12: "no mocks in real mode (the Fake provider exists only for
/// demo mode and tests)."
///
/// Per spec: "AI is always advisory. 'Unknown' is a legitimate answer."
/// The Fake provider returns deterministic but clearly-artificial responses
/// (e.g., "Unknown", "Demo response") — it never fabricates realistic AI
/// output that could be mistaken for a real model's response.
#[derive(Debug)]
pub struct FakeAiProvider {
    /// The embedding dimension to return (default 8 for cheap tests).
    embedding_dim: usize,
    /// canned model list (default: 1 chat + 1 embedding model).
    models: Vec<ModelInfo>,
}

impl FakeAiProvider {
    /// Create a new Fake provider with the default config (dim=8, 2 models).
    #[must_use]
    pub fn new() -> Self {
        Self {
            embedding_dim: 8,
            models: vec![
                ModelInfo {
                    id: "fake-chat-model".into(),
                    role: ModelRole::Chat,
                },
                ModelInfo {
                    id: "fake-embedding-model".into(),
                    role: ModelRole::Embedding,
                },
            ],
        }
    }

    /// Set the embedding dimension (for tests that need a specific dim).
    #[must_use]
    pub fn with_embedding_dim(mut self, dim: usize) -> Self {
        self.embedding_dim = dim;
        self
    }

    /// Set the model list (for tests that need specific models).
    #[must_use]
    pub fn with_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = models;
        self
    }

    /// A deterministic hash of the input text. Used to generate reproducible
    /// embeddings + chat responses so tests are stable.
    fn input_hash(text: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// Generate a deterministic embedding from a hash. Each dimension is
    /// derived from the hash + dimension index, normalized to [-1.0, 1.0].
    /// This is NOT a real embedding — it's a deterministic placeholder for tests.
    fn deterministic_embedding(&self, text: &str) -> Vec<f32> {
        let hash = Self::input_hash(text);
        (0..self.embedding_dim)
            .map(|i| {
                // Mix the hash with the dimension index.
                let mixed = hash.wrapping_mul(31).wrapping_add(i as u64);
                // Map to [-1.0, 1.0].
                (mixed % 10_000) as f32 / 5_000.0 - 1.0
            })
            .collect()
    }
}

impl Default for FakeAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LocalAiProvider for FakeAiProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self.models.clone())
    }

    async fn chat(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatResponse> {
        // Per spec: "AI is always advisory. 'Unknown' is a legitimate answer."
        // The Fake provider returns a clearly-artificial response — never
        // fabricates realistic AI output.
        let last_user_msg = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("(empty)");

        let content = format!(
            "[Fake AI demo response] I received your message: '{last_user_msg}'. \
             This is a deterministic placeholder — no real AI model is running. \
             Configure LM Studio or Ollama to get real AI responses."
        );

        let prompt_tokens: u64 = messages.iter().map(|m| m.content.len() as u64 / 4).sum();
        let completion_tokens = content.len() as u64 / 4;

        Ok(ChatResponse {
            content,
            model: model.to_string(),
            usage: Some(TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }),
            finish_reason: Some("stop".into()),
        })
    }

    async fn embed(&self, model: &str, text: &str) -> Result<EmbedResponse> {
        let vector = self.deterministic_embedding(text);
        let dim = vector.len();
        Ok(EmbedResponse {
            vector,
            dim,
            model: model.to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: text.len() as u64 / 4,
                completion_tokens: 0,
                total_tokens: text.len() as u64 / 4,
            }),
        })
    }

    async fn is_available(&self) -> bool {
        // The Fake provider is always "available" (it's the demo-mode fallback).
        true
    }
}

/// A no-op provider — used when no AI provider is configured.
/// Per spec A5: "The app works fully without them." This provider returns
/// "Unknown" for chat (per spec: "'Unknown' is a legitimate answer") and
/// empty for embeddings.
#[derive(Debug, Default)]
pub struct NoopAiProvider;

#[async_trait]
impl LocalAiProvider for NoopAiProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }

    async fn chat(&self, _model: &str, _messages: &[ChatMessage]) -> Result<ChatResponse> {
        // Per spec: "'Unknown' is a legitimate answer; never fabricate values."
        Ok(ChatResponse {
            content: "Unknown".into(),
            model: "none".into(),
            usage: None,
            finish_reason: Some("no_provider".into()),
        })
    }

    async fn embed(&self, _model: &str, _text: &str) -> Result<EmbedResponse> {
        // No provider → empty embedding. The caller should check `is_available()`
        // before calling `embed()` in real mode; this is a fail-safe.
        Ok(EmbedResponse {
            vector: Vec::new(),
            dim: 0,
            model: "none".into(),
            usage: None,
        })
    }

    async fn is_available(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
        }
    }

    // ---- ModelRole ---------------------------------------------------------

    #[test]
    fn model_role_all_has_two_variants() {
        assert_eq!(ModelRole::ALL.len(), 2);
        assert!(ModelRole::ALL.contains(&ModelRole::Chat));
        assert!(ModelRole::ALL.contains(&ModelRole::Embedding));
    }

    #[test]
    fn model_role_as_str() {
        assert_eq!(ModelRole::Chat.as_str(), "chat");
        assert_eq!(ModelRole::Embedding.as_str(), "embedding");
    }

    #[test]
    fn model_role_serializes_snake_case() {
        let s = serde_json::to_string(&ModelRole::Chat).unwrap();
        assert_eq!(s, "\"chat\"");
        let s = serde_json::to_string(&ModelRole::Embedding).unwrap();
        assert_eq!(s, "\"embedding\"");
    }

    // ---- FakeAiProvider: list_models ---------------------------------------

    #[tokio::test]
    async fn fake_list_models_returns_two_models() {
        let provider = FakeAiProvider::new();
        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|m| m.role == ModelRole::Chat));
        assert!(models.iter().any(|m| m.role == ModelRole::Embedding));
    }

    #[tokio::test]
    async fn fake_list_models_with_custom_models() {
        let provider = FakeAiProvider::new().with_models(vec![ModelInfo {
            id: "custom-chat".into(),
            role: ModelRole::Chat,
        }]);
        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "custom-chat");
    }

    // ---- FakeAiProvider: chat ----------------------------------------------

    #[tokio::test]
    async fn fake_chat_returns_canned_response() {
        let provider = FakeAiProvider::new();
        let response = provider
            .chat("fake-chat-model", &[user_msg("Hello")])
            .await
            .unwrap();
        assert_eq!(response.model, "fake-chat-model");
        assert!(
            response.content.contains("Hello"),
            "response mentions the user message"
        );
        assert!(
            response.content.contains("[Fake AI demo response]"),
            "clearly labeled as fake"
        );
        assert!(response.finish_reason.as_deref() == Some("stop"));
    }

    #[tokio::test]
    async fn fake_chat_includes_token_usage() {
        let provider = FakeAiProvider::new();
        let response = provider
            .chat("fake-chat-model", &[user_msg("Hello world")])
            .await
            .unwrap();
        let usage = response.usage.expect("usage should be reported");
        assert!(usage.prompt_tokens > 0, "prompt tokens > 0");
        assert!(usage.completion_tokens > 0, "completion tokens > 0");
        assert_eq!(
            usage.total_tokens,
            usage.prompt_tokens + usage.completion_tokens
        );
    }

    #[tokio::test]
    async fn fake_chat_uses_last_user_message() {
        let provider = FakeAiProvider::new();
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            user_msg("First message"),
            ChatMessage {
                role: "assistant".into(),
                content: "Ok.".into(),
            },
            user_msg("Second message"),
        ];
        let response = provider.chat("fake-chat-model", &messages).await.unwrap();
        // The response should reference the LAST user message, not the first.
        assert!(response.content.contains("Second message"));
        assert!(!response.content.contains("First message"));
    }

    #[tokio::test]
    async fn fake_chat_with_empty_messages() {
        let provider = FakeAiProvider::new();
        let response = provider.chat("fake-chat-model", &[]).await.unwrap();
        // Doesn't panic — falls back to "(empty)".
        assert!(response.content.contains("(empty)"));
    }

    // ---- FakeAiProvider: embed ---------------------------------------------

    #[tokio::test]
    async fn fake_embed_returns_vector_with_correct_dim() {
        let provider = FakeAiProvider::new().with_embedding_dim(384);
        let response = provider
            .embed("fake-embedding-model", "hello world")
            .await
            .unwrap();
        assert_eq!(response.dim, 384);
        assert_eq!(response.vector.len(), 384);
        assert_eq!(response.model, "fake-embedding-model");
    }

    #[tokio::test]
    async fn fake_embed_is_deterministic() {
        let provider = FakeAiProvider::new().with_embedding_dim(8);
        let r1 = provider.embed("m", "same text").await.unwrap();
        let r2 = provider.embed("m", "same text").await.unwrap();
        assert_eq!(r1.vector, r2.vector, "same input → same embedding");
    }

    #[tokio::test]
    async fn fake_embed_different_inputs_produce_different_vectors() {
        let provider = FakeAiProvider::new().with_embedding_dim(8);
        let r1 = provider.embed("m", "hello").await.unwrap();
        let r2 = provider.embed("m", "world").await.unwrap();
        assert_ne!(
            r1.vector, r2.vector,
            "different inputs → different embeddings"
        );
    }

    #[tokio::test]
    async fn fake_embed_values_in_range_minus_1_to_1() {
        let provider = FakeAiProvider::new().with_embedding_dim(100);
        let response = provider.embed("m", "test text").await.unwrap();
        for v in &response.vector {
            assert!(
                *v >= -1.0 && *v <= 1.0,
                "value {v} out of [-1.0, 1.0] range"
            );
        }
    }

    #[tokio::test]
    async fn fake_embed_includes_token_usage() {
        let provider = FakeAiProvider::new();
        let response = provider.embed("m", "hello world").await.unwrap();
        let usage = response.usage.expect("usage should be reported");
        assert!(usage.prompt_tokens > 0);
        assert_eq!(
            usage.completion_tokens, 0,
            "embeddings have no completion tokens"
        );
    }

    // ---- FakeAiProvider: is_available --------------------------------------

    #[tokio::test]
    async fn fake_is_available_returns_true() {
        let provider = FakeAiProvider::new();
        assert!(
            provider.is_available().await,
            "Fake provider is always available (demo mode)"
        );
    }

    // ---- NoopAiProvider ----------------------------------------------------

    #[tokio::test]
    async fn noop_list_models_returns_empty() {
        let provider = NoopAiProvider;
        assert!(provider.list_models().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn noop_chat_returns_unknown() {
        let provider = NoopAiProvider;
        let response = provider.chat("any", &[user_msg("test")]).await.unwrap();
        assert_eq!(
            response.content, "Unknown",
            "per spec: 'Unknown' is a legitimate answer"
        );
        assert_eq!(response.finish_reason.as_deref(), Some("no_provider"));
    }

    #[tokio::test]
    async fn noop_embed_returns_empty_vector() {
        let provider = NoopAiProvider;
        let response = provider.embed("any", "test").await.unwrap();
        assert!(response.vector.is_empty());
        assert_eq!(response.dim, 0);
    }

    #[tokio::test]
    async fn noop_is_available_returns_false() {
        let provider = NoopAiProvider;
        assert!(
            !provider.is_available().await,
            "Noop provider is never available"
        );
    }

    // ---- serde round-trips -------------------------------------------------

    #[test]
    fn chat_message_serializes() {
        let msg = ChatMessage {
            role: "user".into(),
            content: "Hello".into(),
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"role\":\"user\""));
        assert!(s.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn chat_response_serializes() {
        let r = ChatResponse {
            content: "test".into(),
            model: "m".into(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
            finish_reason: Some("stop".into()),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"content\":\"test\""));
        assert!(s.contains("\"total_tokens\":15"));
    }

    #[test]
    fn embed_response_serializes() {
        let r = EmbedResponse {
            vector: vec![0.1, 0.2, 0.3],
            dim: 3,
            model: "m".into(),
            usage: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"dim\":3"));
        assert!(s.contains("\"vector\":[0.1,0.2,0.3]"));
    }

    #[test]
    fn model_info_serializes() {
        let m = ModelInfo {
            id: "test-model".into(),
            role: ModelRole::Embedding,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"id\":\"test-model\""));
        assert!(s.contains("\"role\":\"embedding\""));
    }

    // ---- Send + Sync -------------------------------------------------------

    #[test]
    fn fake_ai_provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FakeAiProvider>();
        assert_send_sync::<NoopAiProvider>();
    }

    // ---- input_hash determinism (indirect via embed) -----------------------

    #[tokio::test]
    async fn input_hash_is_deterministic_across_provider_instances() {
        // Two separate FakeAiProvider instances with the same config should
        // produce the same embedding for the same input (the hash is purely
        // a function of the input text, not the provider instance).
        let p1 = FakeAiProvider::new().with_embedding_dim(8);
        let p2 = FakeAiProvider::new().with_embedding_dim(8);
        let r1 = p1.embed("m", "same text").await.unwrap();
        let r2 = p2.embed("m", "same text").await.unwrap();
        assert_eq!(r1.vector, r2.vector, "deterministic across instances");
    }
}
