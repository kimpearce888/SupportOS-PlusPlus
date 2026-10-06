//! LM Studio adapter — OpenAI-compatible HTTP at 127.0.0.1:1234/v1 (M5-T04).
//!
//! Per spec M5: "LocalAIProvider (LM Studio, Ollama, generic)."
//! Per spec A5: "LM Studio and Ollama are optional, never bundled: auto-detect,
//! list models, select, test. The app works fully without them."
//! Per the reference notes: "LM Studio (default): OpenAI-compatible API at
//! http://127.0.0.1:1234/v1. Two endpoints: chat completions and embeddings."
//!
//! ## Design
//!
//! LM Studio exposes an OpenAI-compatible API. This module provides:
//!
//! - `OpenAiCompatibleClient`: shared HTTP client logic (used by LM Studio
//!   and Generic adapter in M5-T06). Builds requests (pure, testable) and
//!   sends them via `reqwest` (I/O).
//! - `LmStudioProvider`: the LM Studio-specific config (base URL, default
//!   port 1234) wrapping `OpenAiCompatibleClient`.
//!
//! Per spec A12: "Keep pure logic separate from I/O." The request-building
//! functions (`build_chat_request_body`, `build_embed_request_body`,
//! `parse_chat_response`, `parse_embed_response`) are pure + testable without
//! a running LM Studio. The actual HTTP send (`send_chat`, `send_embed`) is I/O
//! that requires a running LM Studio — tests skip it and use the `FakeAiProvider`
//! from M5-T03 for trait-level tests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::ai_provider::{
    ChatMessage, ChatResponse, EmbedResponse, LocalAiProvider, ModelInfo, ModelRole, TokenUsage,
};
use crate::error::{Error, Result};

/// The default LM Studio base URL.
pub const LM_STUDIO_BASE_URL: &str = "http://127.0.0.1:1234/v1";

/// The default request timeout (reference `lmstudio_timeout_ms`, 120 s).
pub const LM_STUDIO_DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Normalize a base URL so every request path (`/models`,
/// `/chat/completions`, `/embeddings`) resolves against the OpenAI-compatible
/// `/v1` root (AI-22 / M10): strip ONE trailing `/`, strip ONE trailing
/// `/v1`, then append `/v1` — so `http://x:1234`, `http://x:1234/`,
/// `http://x:1234/v1` and `http://x:1234/v1/` all resolve to
/// `http://x:1234/v1`.
#[must_use]
pub fn normalize_v1_base_url(url: &str) -> String {
    format!("{}/v1", crate::embeddings::normalize_lm_base_url(url))
}

/// The LM Studio AI provider — wraps an OpenAI-compatible HTTP client.
///
/// Per spec A5: "LM Studio and Ollama are optional, never bundled: auto-detect,
/// list models, select, test." The `is_available()` method auto-detects by
/// attempting a connection to `{base_url}/models`. If LM Studio isn't running,
/// `is_available()` returns `false` and the app works fully without AI.
#[derive(Debug, Clone)]
pub struct LmStudioProvider {
    client: OpenAiCompatibleClient,
}

impl LmStudioProvider {
    /// Create a new LM Studio provider with the default base URL
    /// (`http://127.0.0.1:1234/v1`) and the default 120 s request timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::with_base_url(LM_STUDIO_BASE_URL)
    }

    /// Create a new LM Studio provider with a custom base URL (for tests
    /// or non-default LM Studio configs). The URL is normalized to the
    /// `/v1` root (AI-22) so `http://x:1234` and `http://x:1234/v1` behave
    /// identically.
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            client: OpenAiCompatibleClient::new(base_url),
        }
    }

    /// Create a new LM Studio provider with a custom base URL AND a request
    /// timeout in milliseconds (`lmstudio_timeout_ms`, AI-22 / M10). Every
    /// request through the client — `/models`, `/chat/completions`,
    /// `/embeddings` — is bounded by it; a hung LM Studio can no longer hang
    /// the caller forever.
    #[must_use]
    pub fn with_base_url_and_timeout(base_url: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            client: OpenAiCompatibleClient::new_with_timeout(base_url, timeout_ms),
        }
    }
}

impl Default for LmStudioProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LocalAiProvider for LmStudioProvider {
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

// ─── OpenAI-compatible HTTP client (shared by LM Studio + Generic) ────────

/// An OpenAI-compatible HTTP client. Used by `LmStudioProvider` (M5-T04)
/// and `GenericAiProvider` (M5-T06). Both speak the OpenAI API format:
/// - `GET /models` — list models
/// - `POST /chat/completions` — chat completions
/// - `POST /embeddings` — embeddings
///
/// Per spec A12: "Keep pure logic separate from I/O." The request/response
/// structs are pure (serde-serializable); the HTTP send is I/O.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleClient {
    /// The base URL (e.g., `http://127.0.0.1:1234/v1`).
    base_url: String,
    /// The reqwest client (shared across requests for connection pooling).
    http: reqwest::Client,
    /// Optional API key (for the Generic adapter; LM Studio doesn't require one).
    api_key: Option<String>,
}

impl OpenAiCompatibleClient {
    /// Create a new client with the given base URL and the default request
    /// timeout (120 s). The URL is normalized to the `/v1` root (AI-22).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::new_with_timeout(base_url, LM_STUDIO_DEFAULT_TIMEOUT_MS)
    }

    /// Create a new client with the given base URL and a request timeout in
    /// milliseconds (reference `lmstudio_timeout_ms`, AI-22 / M10). The
    /// timeout bounds the WHOLE request (connect + headers + body), so a
    /// hung LM Studio process fails the caller instead of blocking it
    /// forever.
    #[must_use]
    pub fn new_with_timeout(base_url: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            base_url: normalize_v1_base_url(&base_url.into()),
            http: build_http_client(timeout_ms),
            api_key: None,
        }
    }

    /// Rebuild the HTTP client with a different request timeout, preserving
    /// the normalized base URL and API key (AI-22).
    #[must_use]
    pub fn with_timeout_ms(self, timeout_ms: u64) -> Self {
        Self {
            base_url: self.base_url,
            http: build_http_client(timeout_ms),
            api_key: self.api_key,
        }
    }

    /// Set an API key (for the Generic adapter; LM Studio doesn't require one).
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// The base URL (without trailing slash).
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Auto-detect: returns `true` if the provider is running (i.e., `GET /models`
    /// succeeds). Per spec A5: "auto-detect, list models, select, test."
    /// Returns `false` on any connection error (provider not running).
    pub async fn is_available(&self) -> bool {
        match self
            .http
            .get(format!("{}/models", self.base_url))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// List models via `GET /models`. Returns models tagged with their role
    /// (chat or embedding — inferred from the model id, since the OpenAI
    /// `/models` endpoint doesn't report roles).
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let resp = self
            .http
            .get(format!("{}/models", self.base_url))
            .send()
            .await
            .map_err(|e| Error::Config(format!("LM Studio list_models failed: {e}")))?;
        let body: ModelsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Config(format!("LM Studio list_models parse failed: {e}")))?;
        Ok(body.data.into_iter().map(infer_model_role).collect())
    }

    /// Chat completion via `POST /chat/completions`.
    pub async fn chat(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatResponse> {
        let request = build_chat_request_body(model, messages);
        let mut req_builder = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .json(&request);
        if let Some(ref key) = self.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        let resp = req_builder
            .send()
            .await
            .map_err(|e| Error::Config(format!("LM Studio chat failed: {e}")))?;
        let body: ChatCompletionResponse = resp
            .json()
            .await
            .map_err(|e| Error::Config(format!("LM Studio chat parse failed: {e}")))?;
        Ok(parse_chat_response(body))
    }

    /// Chat completion via `POST /chat/completions` — full reference options
    /// (`lmStudioClient.chat`): temperature, max_tokens, JSON mode. `model`
    /// is optional exactly like the reference (`opts.model ?? settings
    /// .chat_model ?? undefined` — when absent the field is omitted and the
    /// server picks its default).
    pub async fn chat_with_opts(
        &self,
        model: Option<&str>,
        messages: &[ChatMessage],
        opts: ChatOpts,
    ) -> Result<ChatOptsResult> {
        let mut request = ChatCompletionRequest {
            model: model.unwrap_or_default().to_string(),
            messages: messages
                .iter()
                .map(|m| ChatCompletionMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                })
                .collect(),
            temperature: opts.temperature,
            max_tokens: opts.max_tokens,
            response_format: if opts.json_mode {
                Some(serde_json::json!({ "type": "json_object" }))
            } else {
                None
            },
            stream: false,
        };
        if model.is_none() {
            // Omit the model field entirely (serde can't skip a non-Option
            // field, so serialize through a shim).
            request.model = String::new();
            let shim = serde_json::to_value(&request)?;
            let mut map = shim.as_object().cloned().unwrap_or_default();
            map.remove("model");
            return self.post_chat_body(serde_json::Value::Object(map)).await;
        }
        self.post_chat_body(serde_json::to_value(&request)?).await
    }

    /// Shared POST + parse for the chat endpoints.
    async fn post_chat_body(&self, body: serde_json::Value) -> Result<ChatOptsResult> {
        let started = std::time::Instant::now();
        let mut req_builder = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(ref key) = self.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        let resp = req_builder
            .send()
            .await
            .map_err(|e| Error::Config(format!("LM Studio chat failed: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Config(format!("LM Studio chat read failed: {e}")))?;
        if !status.is_success() {
            let snippet: String = text.chars().take(300).collect();
            return Err(Error::Config(format!(
                "LM Studio chat request failed (HTTP {status}): {snippet}"
            )));
        }
        let body: ChatCompletionResponse = serde_json::from_str(&text)
            .map_err(|e| Error::Config(format!("LM Studio chat parse failed: {e}")))?;
        let latency_ms = started.elapsed().as_millis() as u64;
        let content = body.choices.first().and_then(|c| c.message.content.clone());
        let tool_calls = body
            .choices
            .first()
            .map(|c| {
                c.message
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|tc| ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(ChatOptsResult {
            content,
            model: body.model,
            latency_ms,
            tool_calls,
        })
    }

    /// The Copilot tool loop call (reference `lmStudioClient.chat` with
    /// `tools`): sends the tool definitions, returns the model's tool calls
    /// (if any) alongside the content.
    pub async fn chat_with_tools(
        &self,
        model: Option<&str>,
        messages: &[CopilotWireMessage],
        tools: &[ChatTool],
        temperature: f64,
        max_tokens: u32,
    ) -> Result<ChatOptsResult> {
        let mut body = serde_json::json!({
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(
                tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            },
                        })
                    })
                    .collect::<Vec<_>>(),
            )?;
        }
        if let Some(model) = model {
            body["model"] = serde_json::Value::String(model.to_string());
        }
        self.post_chat_body(body).await
    }

    /// Embedding via `POST /embeddings`.
    pub async fn embed(&self, model: &str, text: &str) -> Result<EmbedResponse> {
        let request = build_embed_request_body(model, text);
        let mut req_builder = self
            .http
            .post(format!("{}/embeddings", self.base_url))
            .json(&request);
        if let Some(ref key) = self.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        let resp = req_builder
            .send()
            .await
            .map_err(|e| Error::Config(format!("LM Studio embed failed: {e}")))?;
        let body: EmbeddingResponse = resp
            .json()
            .await
            .map_err(|e| Error::Config(format!("LM Studio embed parse failed: {e}")))?;
        Ok(parse_embed_response(body))
    }
}

/// Build the reqwest client with the given whole-request timeout (AI-22:
/// `lmstudio_timeout_ms`). Falls back to a default client if the builder
/// fails (e.g. a broken TLS environment) — never to an unbounded one.
fn build_http_client(timeout_ms: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms.max(1)))
        .build()
        .unwrap_or_default()
}

// ─── Pure request/response types (testable without HTTP) ──────────────────

/// The OpenAI `/models` response.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<OpenAiModel>,
}

/// A model entry in the `/models` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiModel {
    pub id: String,
}

/// Infer the model role from the model id. The OpenAI `/models` endpoint
/// doesn't report whether a model is a chat model or an embedding model.
/// We use a simple heuristic: if the id contains "embed", it's an embedding
/// model; otherwise it's a chat model. This matches the convention used by
/// LM Studio and most OpenAI-compatible providers.
#[must_use]
pub fn infer_model_role(model: OpenAiModel) -> ModelInfo {
    let role = if model.id.to_lowercase().contains("embed") {
        ModelRole::Embedding
    } else {
        ModelRole::Chat
    };
    ModelInfo { id: model.id, role }
}

/// Serde skip predicate for the always-false `stream` flag.
fn is_false(v: &bool) -> bool {
    !*v
}

/// The request body for `POST /chat/completions`.
///
/// `temperature` / `max_tokens` / `response_format` mirror the reference
/// `lmStudioClient.chat` options (temperature 0.2, max_tokens 2048,
/// `response_format: {type: 'json_object'}` in JSON mode); they are omitted
/// from the JSON when unset so legacy callers keep the exact same wire
/// shape as before.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatCompletionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "is_false")]
    pub stream: bool,
}

/// A message in the chat completion request (mirrors `ChatMessage` but with
/// the OpenAI field names).
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionMessage {
    pub role: String,
    pub content: String,
}

/// Build the request body for a chat completion. Pure function — testable
/// without HTTP. Legacy shape: no temperature/max_tokens/json-mode.
#[must_use]
pub fn build_chat_request_body(model: &str, messages: &[ChatMessage]) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: messages
            .iter()
            .map(|m| ChatCompletionMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect(),
        temperature: None,
        max_tokens: None,
        response_format: None,
        stream: false,
    }
}

/// The full-options chat result (reference `lmStudioClient.chat` ChatResult:
/// content may be null — e.g. when the model called a tool instead).
#[derive(Debug, Clone)]
pub struct ChatOptsResult {
    /// The generated text (None when the model returned no content).
    pub content: Option<String>,
    /// The model that actually served the request.
    pub model: String,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
    /// Tool calls the model requested (empty when it answered directly —
    /// reference `ChatResult.toolCalls`).
    pub tool_calls: Vec<ToolCall>,
}

/// A tool definition for the chat API (reference `ChatTool`):
/// `{type:'function', function:{name, description, parameters}}`.
#[derive(Debug, Clone)]
pub struct ChatTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool call requested by the model (reference `ChatResult.toolCalls`
/// entry): the raw argument string is passed through verbatim and parsed
/// server-side by the registry.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A message in the Copilot tool loop (reference `ChatMessage` with
/// `tool_calls` / `tool_call_id` / `name`). Serialized with the exact
/// OpenAI field names; `null`-content assistant turns serialize as
/// `""` exactly like the reference (`content: res.content ?? ''`).
#[derive(Debug, Clone, Serialize)]
pub struct CopilotWireMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CopilotWireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The `tool_calls` entry on an assistant message.
#[derive(Debug, Clone, Serialize)]
pub struct CopilotWireToolCall {
    pub id: String,
    pub r#type: &'static str,
    pub function: CopilotWireToolFunction,
}

/// The `function` block of a tool call.
#[derive(Debug, Clone, Serialize)]
pub struct CopilotWireToolFunction {
    pub name: String,
    pub arguments: String,
}

/// Options for the full chat call (reference `lmStudioClient.chat(opts)`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatOpts {
    /// Sampling temperature (reference default 0.2).
    pub temperature: Option<f64>,
    /// Completion token cap (reference default 2048).
    pub max_tokens: Option<u32>,
    /// `response_format: {"type":"json_object"}` when true.
    pub json_mode: bool,
}

/// The response from `POST /chat/completions`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<OpenAiUsage>,
}

/// A choice in the chat completion response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    pub message: ChatChoiceMessage,
    pub finish_reason: Option<String>,
}

/// The message in a chat choice.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoiceMessage {
    /// The message content — null when the model returned only tool calls
    /// (reference `choice?.message?.content ?? null`).
    #[serde(default)]
    pub content: Option<String>,
    /// Tool calls the model requested (reference
    /// `choice?.message?.tool_calls`).
    #[serde(default)]
    pub tool_calls: Option<Vec<RawToolCall>>,
}

/// A raw tool call in the response (OpenAI shape).
#[derive(Debug, Clone, Deserialize)]
pub struct RawToolCall {
    pub id: String,
    #[serde(default)]
    pub r#type: Option<String>,
    pub function: RawToolCallFunction,
}

/// The `function` block of a raw tool call.
#[derive(Debug, Clone, Deserialize)]
pub struct RawToolCallFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

/// The usage object in an OpenAI response.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Parse a chat completion response into our `ChatResponse`. Pure function.
#[must_use]
pub fn parse_chat_response(resp: ChatCompletionResponse) -> ChatResponse {
    let content = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_else(|| "Unknown".into());
    let finish_reason = resp.choices.first().and_then(|c| c.finish_reason.clone());
    let usage = resp.usage.map(|u| TokenUsage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });
    ChatResponse {
        content,
        model: resp.model,
        usage,
        finish_reason,
    }
}

/// The request body for `POST /embeddings`.
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: String,
}

/// Build the request body for an embedding. Pure function.
#[must_use]
pub fn build_embed_request_body(model: &str, text: &str) -> EmbeddingRequest {
    EmbeddingRequest {
        model: model.to_string(),
        input: text.to_string(),
    }
}

/// The response from `POST /embeddings`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingResponse {
    pub model: String,
    pub data: Vec<EmbeddingData>,
    pub usage: Option<OpenAiUsage>,
}

/// An embedding entry in the response.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingData {
    pub embedding: Vec<f32>,
}

/// Parse an embedding response into our `EmbedResponse`. Pure function.
#[must_use]
pub fn parse_embed_response(resp: EmbeddingResponse) -> EmbedResponse {
    let vector = resp
        .data
        .first()
        .map(|d| d.embedding.clone())
        .unwrap_or_default();
    let dim = vector.len();
    let usage = resp.usage.map(|u| TokenUsage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });
    EmbedResponse {
        vector,
        dim,
        model: resp.model,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- infer_model_role --------------------------------------------------

    #[test]
    fn infer_model_role_embed_in_id() {
        let model = OpenAiModel {
            id: "text-embedding-3-small".into(),
        };
        let info = infer_model_role(model);
        assert_eq!(info.role, ModelRole::Embedding);
    }

    #[test]
    fn infer_model_role_chat_in_id() {
        let model = OpenAiModel {
            id: "llama-3.1-8b-instruct".into(),
        };
        let info = infer_model_role(model);
        assert_eq!(info.role, ModelRole::Chat);
    }

    #[test]
    fn infer_model_role_embed_case_insensitive() {
        let model = OpenAiModel {
            id: "BGE-Embedding-v1".into(),
        };
        let info = infer_model_role(model);
        assert_eq!(info.role, ModelRole::Embedding);
    }

    // ---- build_chat_request_body -------------------------------------------

    #[test]
    fn build_chat_request_body_serializes_messages() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
            },
        ];
        let body = build_chat_request_body("llama-3", &messages);
        assert_eq!(body.model, "llama-3");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].content, "Hello");
    }

    #[test]
    fn build_chat_request_body_with_empty_messages() {
        let body = build_chat_request_body("m", &[]);
        assert_eq!(body.model, "m");
        assert!(body.messages.is_empty());
    }

    // ---- build_embed_request_body ------------------------------------------

    #[test]
    fn build_embed_request_body_serializes_input() {
        let body = build_embed_request_body("embed-model", "hello world");
        assert_eq!(body.model, "embed-model");
        assert_eq!(body.input, "hello world");
    }

    // ---- parse_chat_response -----------------------------------------------

    #[test]
    fn parse_chat_response_with_content() {
        let resp = ChatCompletionResponse {
            model: "llama-3".into(),
            choices: vec![ChatChoice {
                message: ChatChoiceMessage {
                    content: Some("Hello!".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(OpenAiUsage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
            }),
        };
        let parsed = parse_chat_response(resp);
        assert_eq!(parsed.content, "Hello!");
        assert_eq!(parsed.model, "llama-3");
        assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn parse_chat_response_empty_choices_returns_unknown() {
        let resp = ChatCompletionResponse {
            model: "m".into(),
            choices: vec![],
            usage: None,
        };
        let parsed = parse_chat_response(resp);
        // Per spec: "'Unknown' is a legitimate answer."
        assert_eq!(parsed.content, "Unknown");
        assert!(parsed.usage.is_none());
    }

    #[test]
    fn parse_chat_response_missing_finish_reason() {
        let resp = ChatCompletionResponse {
            model: "m".into(),
            choices: vec![ChatChoice {
                message: ChatChoiceMessage {
                    content: Some("hi".into()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let parsed = parse_chat_response(resp);
        assert!(parsed.finish_reason.is_none());
    }

    // ---- parse_embed_response ----------------------------------------------

    #[test]
    fn parse_embed_response_with_vector() {
        let resp = EmbeddingResponse {
            model: "embed-m".into(),
            data: vec![EmbeddingData {
                embedding: vec![0.1, 0.2, 0.3],
            }],
            usage: Some(OpenAiUsage {
                prompt_tokens: 2,
                completion_tokens: 0,
                total_tokens: 2,
            }),
        };
        let parsed = parse_embed_response(resp);
        assert_eq!(parsed.dim, 3);
        assert_eq!(parsed.vector, vec![0.1, 0.2, 0.3]);
        assert_eq!(parsed.model, "embed-m");
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 2);
        assert_eq!(usage.completion_tokens, 0);
    }

    #[test]
    fn parse_embed_response_empty_data_returns_empty_vector() {
        let resp = EmbeddingResponse {
            model: "m".into(),
            data: vec![],
            usage: None,
        };
        let parsed = parse_embed_response(resp);
        assert!(parsed.vector.is_empty());
        assert_eq!(parsed.dim, 0);
    }

    // ---- OpenAiCompatibleClient config -------------------------------------

    #[test]
    fn client_trims_trailing_slash_from_base_url() {
        let client = OpenAiCompatibleClient::new("http://127.0.0.1:1234/v1/");
        assert_eq!(client.base_url(), "http://127.0.0.1:1234/v1");
    }

    // ---- AI-22: /v1 normalization on all paths -----------------------------

    #[test]
    fn client_normalizes_missing_v1_suffix() {
        // The settings default and user-entered URLs often omit /v1 — every
        // request path must still resolve against the /v1 root.
        let client = OpenAiCompatibleClient::new("http://127.0.0.1:1234");
        assert_eq!(client.base_url(), "http://127.0.0.1:1234/v1");
    }

    #[test]
    fn client_normalizes_trailing_slash_without_v1() {
        let client = OpenAiCompatibleClient::new("http://127.0.0.1:1234/");
        assert_eq!(client.base_url(), "http://127.0.0.1:1234/v1");
    }

    #[test]
    fn client_normalizes_doubled_v1() {
        let client = OpenAiCompatibleClient::new("http://127.0.0.1:1234/v1/");
        assert_eq!(client.base_url(), "http://127.0.0.1:1234/v1");
        // exactly one /v1, not two
        assert!(!client.base_url().ends_with("/v1/v1"));
    }

    #[test]
    fn client_with_api_key_stores_key() {
        let client =
            OpenAiCompatibleClient::new("http://localhost:1234/v1").with_api_key("sk-test");
        assert!(client.api_key.is_some());
    }

    #[test]
    fn with_timeout_preserves_base_url_and_api_key() {
        let client = OpenAiCompatibleClient::new("http://localhost:1234")
            .with_api_key("sk-test")
            .with_timeout_ms(2_500);
        assert_eq!(client.base_url(), "http://localhost:1234/v1");
        assert_eq!(client.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn lm_studio_provider_with_timeout_normalizes_base() {
        let provider = LmStudioProvider::with_base_url_and_timeout("http://localhost:9999", 5_000);
        assert_eq!(provider.client.base_url(), "http://localhost:9999/v1");
    }

    // ---- LmStudioProvider config -------------------------------------------

    #[test]
    fn lm_studio_default_base_url() {
        let provider = LmStudioProvider::new();
        assert_eq!(provider.client.base_url(), LM_STUDIO_BASE_URL);
    }

    #[test]
    fn lm_studio_custom_base_url() {
        let provider = LmStudioProvider::with_base_url("http://localhost:9999/v1");
        assert_eq!(provider.client.base_url(), "http://localhost:9999/v1");
    }

    // ---- serde round-trips (request bodies) --------------------------------

    #[test]
    fn chat_completion_request_serializes_to_json() {
        let body = build_chat_request_body(
            "m",
            &[ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
        );
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"model\":\"m\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"hi\""));
    }

    #[test]
    fn embedding_request_serializes_to_json() {
        let body = build_embed_request_body("embed-m", "hello");
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"model\":\"embed-m\""));
        assert!(json.contains("\"input\":\"hello\""));
    }

    // ---- ModelsResponse parsing --------------------------------------------

    #[test]
    fn models_response_parses() {
        let json = r#"{"data": [{"id": "llama-3"}, {"id": "text-embedding-3"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 2);
        assert_eq!(resp.data[0].id, "llama-3");
        assert_eq!(resp.data[1].id, "text-embedding-3");
    }

    #[test]
    fn models_response_empty_data_parses() {
        let json = r#"{"data": []}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.data.is_empty());
    }

    // ---- ChatCompletionResponse parsing -------------------------------------

    #[test]
    fn chat_completion_response_parses() {
        let json = r#"{
            "model": "llama-3",
            "choices": [{"message": {"content": "Hello!"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        }"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model, "llama-3");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        let usage = resp.usage.unwrap();
        assert_eq!(usage.total_tokens, 8);
    }

    // ---- EmbeddingResponse parsing -----------------------------------------

    #[test]
    fn embedding_response_parses() {
        let json = r#"{
            "model": "embed-m",
            "data": [{"embedding": [0.1, 0.2, 0.3]}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 0, "total_tokens": 2}
        }"#;
        let resp: EmbeddingResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model, "embed-m");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    // ---- Send + Sync -------------------------------------------------------

    #[test]
    fn lm_studio_provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LmStudioProvider>();
        assert_send_sync::<OpenAiCompatibleClient>();
    }

    // ---- AI-22: live wire tests against a mock LM Studio -------------------

    mod ai22_wire {
        use super::super::*;
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};

        /// A mock LM Studio that records every request path and serves
        /// canned OpenAI-shaped bodies (same pattern as the SY-06 wire tests).
        struct MockLmStudio {
            requests: Mutex<Vec<String>>,
        }

        async fn spawn_mock() -> (String, Arc<MockLmStudio>) {
            let api = Arc::new(MockLmStudio {
                requests: Mutex::new(Vec::new()),
            });
            let api_for_routes = api.clone();
            let app = axum::Router::new().fallback(
                move |method: axum::http::Method, uri: axum::http::Uri| {
                    let api = api_for_routes.clone();
                    async move {
                        {
                            let mut reqs = api.requests.lock().unwrap();
                            reqs.push(format!("{method} {}", uri.path()));
                        }
                        match uri.path() {
                            "/v1/models" => axum::Json(serde_json::json!({
                                "object": "list",
                                "data": [
                                    { "id": "qwen-chat", "object": "model" },
                                    { "id": "text-embedding", "object": "model" }
                                ]
                            }))
                            .into_response(),
                            "/v1/chat/completions" => axum::Json(serde_json::json!({
                                "model": "qwen-chat",
                                "choices": [{
                                    "message": { "role": "assistant", "content": "hello there" },
                                    "finish_reason": "stop"
                                }],
                                "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }
                            }))
                            .into_response(),
                            "/v1/embeddings" => axum::Json(serde_json::json!({
                                "model": "text-embedding",
                                "data": [{ "embedding": [0.1, 0.2, 0.3] }],
                                "usage": { "prompt_tokens": 2, "completion_tokens": 0, "total_tokens": 2 }
                            }))
                            .into_response(),
                            _ => (
                                axum::http::StatusCode::NOT_FOUND,
                                axum::Json(serde_json::json!({"error": "unknown path"})),
                            )
                                .into_response(),
                        }
                    }
                },
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock");
            let addr = listener.local_addr().expect("mock addr");
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            (format!("http://{addr}"), api)
        }

        fn recorded(api: &MockLmStudio) -> Vec<String> {
            api.requests.lock().unwrap().clone()
        }

        #[tokio::test]
        async fn all_request_paths_hit_the_v1_root_from_a_bare_base() {
            let (base, api) = spawn_mock().await;
            // A base URL WITHOUT /v1 (the stored-settings form) — the client
            // must normalize so every endpoint resolves under /v1.
            let client = OpenAiCompatibleClient::new(&base);
            assert_eq!(client.base_url(), format!("{base}/v1"));

            let models = client.list_models().await.expect("models");
            assert_eq!(models.len(), 2);
            assert_eq!(models[0].id, "qwen-chat");

            let chat = client
                .chat(
                    "qwen-chat",
                    &[ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                    }],
                )
                .await
                .expect("chat");
            assert_eq!(chat.content, "hello there");

            let embed = client.embed("text-embedding", "hi").await.expect("embed");
            assert_eq!(embed.vector, vec![0.1, 0.2, 0.3]);

            let requests = recorded(&api);
            assert_eq!(
                requests,
                vec![
                    "GET /v1/models",
                    "POST /v1/chat/completions",
                    "POST /v1/embeddings",
                ]
            );
        }

        #[tokio::test]
        async fn v1_suffixed_base_hits_the_same_paths() {
            let (base, api) = spawn_mock().await;
            // The already-suffixed form must behave identically (no /v1/v1).
            let client = OpenAiCompatibleClient::new(format!("{base}/v1/"));
            assert!(client.list_models().await.is_ok());
            assert_eq!(recorded(&api), vec!["GET /v1/models"]);
        }

        #[tokio::test]
        async fn timeout_bounds_a_hanging_lm_studio() {
            // A server that accepts the connection and never answers —
            // without a request timeout this hangs forever (the AI-22 gap:
            // one hung LM Studio call froze the status route / AI runs).
            let app = axum::Router::new().fallback(|| async {
                futures_util::future::pending::<()>().await;
                #[allow(unreachable_code)]
                axum::Json(serde_json::json!({}))
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind hanging mock");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });

            let client = OpenAiCompatibleClient::new_with_timeout(format!("http://{addr}"), 150);
            let started = std::time::Instant::now();
            let result = client.list_models().await;
            let elapsed = started.elapsed();
            assert!(result.is_err(), "a hanging server must fail, not hang");
            assert!(
                elapsed < std::time::Duration::from_secs(5),
                "the 150 ms timeout must bound the call (took {elapsed:?})"
            );
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("LM Studio list_models failed"),
                "error should name the failing call: {err}"
            );
        }

        #[tokio::test]
        async fn is_available_is_bounded_by_the_timeout_too() {
            // The status-route probe must not hang either.
            let app = axum::Router::new().fallback(|| async {
                futures_util::future::pending::<()>().await;
                #[allow(unreachable_code)]
                axum::Json(serde_json::json!({}))
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind hanging mock");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let client = OpenAiCompatibleClient::new_with_timeout(format!("http://{addr}"), 150);
            let started = std::time::Instant::now();
            assert!(!client.is_available().await);
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
        }
    }
}
