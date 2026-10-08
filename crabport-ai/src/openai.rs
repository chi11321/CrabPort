//! OpenAI-compatible chat provider backed by the [`async_openai`] crate.
//!
//! Wire-format concerns are split by maturity: request serialization and
//! SSE stream parsing are delegated to `async-openai`, while streaming
//! chunk deserialization uses its `byot` (bring your own types) escape
//! hatch with tolerant local types — OpenAI-compatible servers routinely
//! deviate from the official schema (DeepSeek streams `reasoning_content`
//! and uses non-standard finish reasons), and the library's strict chunk
//! types would reject them. The async stream is driven to completion on a
//! current-thread tokio runtime owned by the calling worker thread (see
//! [`crate::spawn_chat_stream`]). GPUI never sees tokio.

use async_channel::Sender;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionTools,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs, FunctionCall, FunctionObject,
};
use futures_util::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use tokio::runtime::Builder as RuntimeBuilder;
use tracing::debug;

use crate::error::AiError;
use crate::provider::ChatProvider;
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, Role, StreamEvent, TokenUsage, ToolCall,
    ToolSpec,
};

/// Upper bound on distinct tool calls assembled within a single turn.
const MAX_TOOL_CALLS: usize = 64;

/// User agent sent with every request, identifying the client instead of
/// shipping a generic SDK name — what OpenCode's gateways ask for (see
/// <https://opencode.ai/docs/go/#where-can-i-use-it>).
const USER_AGENT: &str = concat!("CrabPort/", env!("CARGO_PKG_VERSION"));

/// Per-conversation routing header required by OpenCode's gateways: a chat
/// request without it is rejected with `MissingSessionID`. Other
/// OpenAI-compatible endpoints ignore it, so it is only attached when the
/// caller passes a session id.
pub const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// Blocking [`ChatProvider`] for any OpenAI-compatible endpoint.
///
/// Wraps an [`async_openai::Client`] configured with the endpoint's base
/// URL and API key; the model travels per-request via [`ChatRequest::model`]
/// so callers can switch models between turns without rebuilding the
/// provider.
pub struct OpenAiProvider {
    client: Client<OpenAIConfig>,
}

impl OpenAiProvider {
    /// Build a provider for any `/chat/completions`-speaking server.
    ///
    /// `base_url` may carry a path prefix (e.g.
    /// `https://api.deepseek.com/v1`); trailing slashes are normalized.
    ///
    /// Construction is cheap but not free: it builds a fresh
    /// `reqwest::Client` (and its connection pool), so hold the provider
    /// for the app's lifetime and reuse it across turns — don't rebuild per
    /// turn or per model switch (the model rides on [`ChatRequest`]).
    pub fn new(base_url: &str, api_key: SecretString) -> Result<Self, AiError> {
        Ok(Self {
            client: Client::with_config(build_config(base_url, &api_key, None)?),
        })
    }

    /// [`Self::new`] plus the conversation's session id, sent as
    /// [`OPENCODE_SESSION_HEADER`] on every request.
    ///
    /// OpenCode's gateways route chat traffic per conversation and reject
    /// requests without the header (`api error (400): MissingSessionID`).
    /// The id only has to be stable across one conversation's requests — a
    /// fresh one per conversation is exactly right.
    pub fn with_session(
        base_url: &str,
        api_key: SecretString,
        session_id: &str,
    ) -> Result<Self, AiError> {
        Ok(Self {
            client: Client::with_config(build_config(base_url, &api_key, Some(session_id))?),
        })
    }
}

/// Client config for an OpenAI-compatible endpoint: base URL, key, the
/// CrabPort user agent, and — when the caller has one — the conversation
/// session id. Split out from the constructors so the exact header set is
/// testable without a live server.
fn build_config(
    base_url: &str,
    api_key: &SecretString,
    session_id: Option<&str>,
) -> Result<OpenAIConfig, AiError> {
    let base = base_url.trim().trim_end_matches('/');
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(AiError::Network(format!(
            "base URL must start with http:// or https://: {base_url:?}"
        )));
    }
    let mut config = OpenAIConfig::new()
        .with_api_base(base)
        .with_api_key(api_key.expose_secret())
        .with_header("user-agent", USER_AGENT)
        .map_err(|err| AiError::Network(format!("user-agent header rejected: {err}")))?;
    if let Some(session_id) = session_id {
        config = config
            .with_header(OPENCODE_SESSION_HEADER, session_id)
            .map_err(|err| AiError::Network(format!("session header rejected: {err}")))?;
    }
    Ok(config)
}

impl ChatProvider for OpenAiProvider {
    fn complete_stream(
        &self,
        request: &ChatRequest,
        events: &Sender<StreamEvent>,
    ) -> Result<ChatResponse, AiError> {
        // Each call already runs on its own worker thread
        // (`spawn_chat_stream`), so a private current-thread runtime is the
        // cheapest way to drive the async HTTP + SSE machinery without
        // leaking a tokio runtime into the UI layer.
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| AiError::Network(format!("tokio runtime: {err}")))?;
        runtime.block_on(self.stream_turn(request, events))
    }
}

impl OpenAiProvider {
    /// Fetch the endpoint's model list (`GET {base}/models`).
    ///
    /// Blocking: drives the async request on a private current-thread
    /// runtime — the same contract as [`ChatProvider::complete_stream`],
    /// so call it from a background task (`cx.spawn`), never the UI thread.
    ///
    /// Uses the `byot` (bring your own type) escape hatch instead of the
    /// library's strict `ListModelResponse`: OpenAI-compatible endpoints
    /// frequently deviate from the official schema (e.g. DeepSeek omits
    /// `Model::created` and adds `name` / `context_window` fields), so only
    /// `data[].id` and an optional context window are extracted from the raw
    /// JSON and everything else is ignored.
    pub fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| AiError::Network(format!("tokio runtime: {err}")))?;
        runtime.block_on(async {
            let value: serde_json::Value = self
                .client
                .models()
                .list_byot()
                .await
                .map_err(map_openai_error)?;
            Ok(extract_models(&value))
        })
    }

    async fn stream_turn(
        &self,
        request: &ChatRequest,
        events: &Sender<StreamEvent>,
    ) -> Result<ChatResponse, AiError> {
        let wire = wire_request(request)?;
        debug!(
            model = %request.model,
            messages = wire.messages.len(),
            tools = wire.tools.as_ref().map_or(0, Vec::len),
            "crabport-ai: streaming chat completion"
        );
        let body = serde_json::to_value(&wire)
            .map_err(|err| AiError::Protocol(format!("failed to serialize request: {err}")))?;

        // `create_stream_byot` takes the raw JSON body and yields our own
        // `ByotChunk` type; the SSE framing ([DONE], eventsource parsing)
        // stays inside the library.
        let mut stream = self
            .client
            .chat()
            .create_stream_byot(body)
            .await
            .map_err(map_openai_error)?;

        let mut acc = TurnAccumulator::default();
        while let Some(item) = stream.next().await {
            let chunk: ByotChunk = item.map_err(map_openai_error)?;
            handle_chunk(chunk, events, &mut acc)?;
        }
        acc.finish()
    }
}

// ---------------------------------------------------------------------------
// Streaming wire format (`byot`)
// ---------------------------------------------------------------------------
//
// Our own tolerant chunk types, deserialized through the library's `byot`
// escape hatch. Every field is optional so vendor extensions pass:
//
// - DeepSeek streams `reasoning_content` (chain-of-thought) alongside
//   `content` — surfaced as [`StreamEvent::Reasoning`].
// - `finish_reason` stays a raw string and is mapped leniently; unknown
//   values become [`FinishReason::Other`] instead of failing the stream.
// - Missing `id`/`object`/`created`/`model` fields (which the library's
//   strict types require) don't matter here.

#[derive(serde::Deserialize)]
struct ByotChunk {
    #[serde(default)]
    choices: Vec<ByotChoice>,
    usage: Option<ByotUsage>,
}

#[derive(serde::Deserialize)]
struct ByotChoice {
    #[serde(default)]
    delta: ByotDelta,
    finish_reason: Option<String>,
}

#[derive(Default, serde::Deserialize)]
struct ByotDelta {
    content: Option<String>,
    /// DeepSeek-style chain-of-thought.
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ByotToolCall>,
}

#[derive(serde::Deserialize)]
struct ByotToolCall {
    #[serde(default)]
    index: usize,
    id: Option<String>,
    function: Option<ByotFunction>,
}

#[derive(Default, serde::Deserialize)]
struct ByotFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(serde::Deserialize)]
struct ByotUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

// ---------------------------------------------------------------------------
// Chunk handling + turn assembly
// ---------------------------------------------------------------------------

fn handle_chunk(
    chunk: ByotChunk,
    events: &Sender<StreamEvent>,
    acc: &mut TurnAccumulator,
) -> Result<(), AiError> {
    if let Some(usage) = chunk.usage {
        let usage = TokenUsage {
            prompt_tokens: u64::from(usage.prompt_tokens),
            completion_tokens: u64::from(usage.completion_tokens),
            total_tokens: u64::from(usage.total_tokens),
        };
        // Several servers repeat zero-value usage heartbeats; only surface
        // real numbers.
        if usage.prompt_tokens + usage.completion_tokens + usage.total_tokens > 0 {
            acc.usage = Some(usage);
            send(events, StreamEvent::Usage(usage))?;
        }
    }

    for choice in &chunk.choices {
        accumulate_delta(&choice.delta, events, acc)?;
        if let Some(reason) = choice.finish_reason.as_deref() {
            acc.finish_reason = Some(map_finish_reason(reason));
        }
    }
    Ok(())
}

fn accumulate_delta(
    delta: &ByotDelta,
    events: &Sender<StreamEvent>,
    acc: &mut TurnAccumulator,
) -> Result<(), AiError> {
    if let Some(text) = delta.content.as_deref().filter(|t| !t.is_empty()) {
        acc.content.push_str(text);
        send(events, StreamEvent::Content(text.to_owned()))?;
    }
    if let Some(text) = delta.reasoning_content.as_deref().filter(|t| !t.is_empty()) {
        acc.reasoning.push_str(text);
        send(events, StreamEvent::Reasoning(text.to_owned()))?;
    }

    for tc in &delta.tool_calls {
        let idx = tc.index;
        if idx >= MAX_TOOL_CALLS {
            return Err(AiError::Protocol("too many tool calls in one turn".into()));
        }
        while acc.tool_calls.len() <= idx {
            acc.tool_calls.push(PartialToolCall::default());
        }
        let slot = &mut acc.tool_calls[idx];
        if let Some(id) = tc.id.as_deref() {
            slot.id = id.to_owned();
        }
        if let Some(name) = tc.function.as_ref().and_then(|f| f.name.as_deref())
            && !name.is_empty()
        {
            slot.name = name.to_owned();
        }
        // Announce as soon as we know what is being called, so the UI can
        // show "calling tool X" while arguments still stream in.
        if !slot.announced && (!slot.id.is_empty() || !slot.name.is_empty()) {
            slot.announced = true;
            send(
                events,
                StreamEvent::ToolCallStarted {
                    index: idx,
                    id: slot.id.clone(),
                    name: slot.name.clone(),
                },
            )?;
        }
        if let Some(args) = tc.function.as_ref().and_then(|f| f.arguments.as_deref())
            && !args.is_empty()
        {
            slot.arguments.push_str(args);
            send(
                events,
                StreamEvent::ToolCallArguments {
                    index: idx,
                    delta: args.to_owned(),
                },
            )?;
        }
    }
    Ok(())
}

fn send(events: &Sender<StreamEvent>, event: StreamEvent) -> Result<(), AiError> {
    events.send_blocking(event).map_err(|_| AiError::Cancelled)
}

#[derive(Default)]
struct TurnAccumulator {
    content: String,
    /// Chain-of-thought streamed ahead of the answer (display-only; never
    /// sent back in follow-up requests).
    reasoning: String,
    tool_calls: Vec<PartialToolCall>,
    finish_reason: Option<FinishReason>,
    usage: Option<TokenUsage>,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
    announced: bool,
}

impl TurnAccumulator {
    fn finish(self) -> Result<ChatResponse, AiError> {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_iter()
            .enumerate()
            .filter(|(_, partial)| partial.announced)
            .map(|(idx, partial)| {
                // Some servers never stream ids; synthesize stable ones so
                // tool-result messages can be paired deterministically.
                let id = if partial.id.is_empty() {
                    format!("call_{idx}")
                } else {
                    partial.id
                };
                ToolCall {
                    id,
                    name: partial.name,
                    arguments: partial.arguments,
                }
            })
            .collect();

        let finish_reason = self.finish_reason.unwrap_or(if tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        });

        Ok(ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: self.content,
                tool_calls,
                ..Default::default()
            },
            reasoning: self.reasoning,
            finish_reason,
            usage: self.usage,
        })
    }
}

// ---------------------------------------------------------------------------
// Type mapping (ours -> wire)
// ---------------------------------------------------------------------------

fn wire_request(request: &ChatRequest) -> Result<CreateChatCompletionRequest, AiError> {
    let messages = request
        .messages
        .iter()
        .map(wire_message)
        .collect::<Result<Vec<_>, AiError>>()?;

    let mut builder = CreateChatCompletionRequestArgs::default();
    builder
        .model(request.model.as_str())
        .messages(messages)
        .stream(true);
    if let Some(temperature) = request.temperature {
        builder.temperature(temperature);
    }
    if let Some(max_tokens) = request.max_tokens {
        builder.max_tokens(max_tokens);
    }
    if !request.tools.is_empty() {
        builder.tools(request.tools.iter().map(wire_tool).collect::<Vec<_>>());
    }
    builder
        .build()
        .map_err(|err| AiError::Protocol(format!("invalid chat request: {err}")))
}

fn wire_tool(spec: &ToolSpec) -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: spec.name.clone(),
            description: Some(spec.description.clone()),
            parameters: Some(spec.parameters.clone()),
            strict: None,
        },
    })
}

fn wire_message(message: &ChatMessage) -> Result<ChatCompletionRequestMessage, AiError> {
    match message.role {
        Role::System => Ok(ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(message.content.clone()),
                name: None,
            },
        )),
        Role::User => Ok(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(message.content.clone()),
                name: None,
            },
        )),
        Role::Assistant => {
            // Built via builder because the struct carries a deprecated
            // `function_call` field (kept None) that we must not touch.
            let mut builder = ChatCompletionRequestAssistantMessageArgs::default();
            if !message.content.is_empty() {
                builder.content(ChatCompletionRequestAssistantMessageContent::Text(
                    message.content.clone(),
                ));
            }
            if !message.tool_calls.is_empty() {
                builder.tool_calls(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| {
                            ChatCompletionMessageToolCalls::Function(
                                ChatCompletionMessageToolCall {
                                    id: call.id.clone(),
                                    function: FunctionCall {
                                        name: call.name.clone(),
                                        arguments: call.arguments.clone(),
                                    },
                                },
                            )
                        })
                        .collect::<Vec<_>>(),
                );
            }
            let built = builder
                .build()
                .map_err(|err| AiError::Protocol(format!("invalid assistant message: {err}")))?;
            Ok(ChatCompletionRequestMessage::Assistant(built))
        }
        Role::Tool => Ok(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(message.content.clone()),
                tool_call_id: message.tool_call_id.clone().unwrap_or_default(),
            },
        )),
    }
}

// ---------------------------------------------------------------------------
// Type mapping (wire -> ours) + error mapping
// ---------------------------------------------------------------------------

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" | "max_tokens" => FinishReason::Length,
        _ => FinishReason::Other,
    }
}

fn map_openai_error(err: OpenAIError) -> AiError {
    match err {
        OpenAIError::ApiError(response) => AiError::Api {
            status: response.status_code.as_u16(),
            message: response.api_error.to_string(),
        },
        OpenAIError::Reqwest(err) => AiError::Network(err.to_string()),
        OpenAIError::StreamError(err) => AiError::Protocol(format!("stream error: {err}")),
        OpenAIError::JSONDeserialize(err, _) => {
            AiError::Protocol(format!("failed to parse response: {err}"))
        }
        other => AiError::Protocol(other.to_string()),
    }
}

/// One entry from `GET {base}/models`.
///
/// Only what the app uses is kept: the id (picker + requests) and, when the
/// endpoint advertises it, the context window — OpenAI-compatible vendors
/// disagree on the field name, so `context_window` and `context_length` are
/// both read. `None` means the endpoint didn't say, and the caller falls
/// back to its own default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub context_window: Option<usize>,
}

/// Extract models from a raw `/models` response.
///
/// Tolerant by design — see [`OpenAiProvider::list_models`]: only
/// `data[].id` (and a context window, when present) is read, and
/// missing/malformed shapes degrade to an empty list.
fn extract_models(value: &serde_json::Value) -> Vec<ModelInfo> {
    value
        .get("data")
        .and_then(|data| data.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("id").and_then(|id| id.as_str())?;
                    let context_window = entry
                        .get("context_window")
                        .or_else(|| entry.get("context_length"))
                        .and_then(|window| window.as_u64())
                        .map(|window| window as usize)
                        .filter(|window| *window > 0);
                    Some(ModelInfo {
                        id: id.to_owned(),
                        context_window,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests — all offline: type mapping + stream assembly only, no network
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_chunk(json: &str) -> ByotChunk {
        serde_json::from_str(json).expect("chunk should parse")
    }

    fn run_chunks(chunks: &[&str]) -> (Vec<StreamEvent>, Result<ChatResponse, AiError>) {
        let (tx, rx) = async_channel::unbounded();
        let mut acc = TurnAccumulator::default();
        for chunk in chunks {
            handle_chunk(parse_chunk(chunk), &tx, &mut acc).expect("chunk should handle");
        }
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        (events, acc.finish())
    }

    /// Every request identifies CrabPort and carries the auth header; a
    /// conversation's session id is attached only when the caller supplies
    /// one (OpenCode's gateways require it, other endpoints don't want it).
    #[test]
    fn client_headers_identify_crabport_and_carry_the_session() {
        use async_openai::config::Config as _;

        let key = SecretString::from("sk-test");

        let config = build_config("https://opencode.ai/zen/go/v1/", &key, Some("sess-1"))
            .expect("session config");
        let headers = config.headers();
        assert_eq!(
            headers
                .get(OPENCODE_SESSION_HEADER)
                .expect("session header")
                .to_str()
                .unwrap(),
            "sess-1"
        );
        assert_eq!(
            headers
                .get("user-agent")
                .expect("user agent")
                .to_str()
                .unwrap(),
            USER_AGENT
        );
        assert_eq!(
            headers
                .get("authorization")
                .expect("authorization")
                .to_str()
                .unwrap(),
            "Bearer sk-test"
        );
        // Trailing slashes are normalized off the base URL.
        assert_eq!(config.api_base(), "https://opencode.ai/zen/go/v1");

        // Model listing (and non-OpenCode endpoints) sends no session
        // header at all.
        let config = build_config("https://api.deepseek.com/v1", &key, None).expect("plain config");
        assert!(config.headers().get(OPENCODE_SESSION_HEADER).is_none());
        assert!(config.headers().get("user-agent").is_some());

        // A session id that can't be a header value is a clean error, not a
        // panic, and a non-HTTP base URL is still rejected up front.
        assert!(build_config("https://opencode.ai/zen/v1", &key, Some("bad\nid")).is_err());
        assert!(build_config("ftp://nope", &key, None).is_err());
    }

    #[test]
    fn wire_messages_match_openai_shape() {
        let request = ChatRequest::new("test-model")
            .with_messages(vec![
                ChatMessage::system("be brief"),
                ChatMessage::user("hi"),
                ChatMessage {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_9".into(),
                        name: "ls".into(),
                        arguments: "{}".into(),
                    }],
                    ..Default::default()
                },
                ChatMessage::tool_result("call_9", "ls", "a.txt"),
            ])
            .with_tools(vec![ToolSpec::new(
                "ls",
                "list dir",
                json!({"type": "object"}),
            )])
            .with_temperature(0.5);

        let wire = wire_request(&request).expect("request builds");
        let value = serde_json::to_value(&wire).expect("serializes");
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(value["stream"], true);
        assert_eq!(value["temperature"], 0.5);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "be brief");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert!(messages[2]["content"].is_null());
        assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_9");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_9");
        assert_eq!(value["tools"][0]["function"]["name"], "ls");
        assert_eq!(
            value["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
    }

    #[test]
    fn content_and_usage_flow_through_events() {
        let chunks = [
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"lo"}}]}"#,
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#,
        ];
        let (events, result) = run_chunks(&chunks);

        let resp = result.expect("finish ok");
        assert_eq!(resp.message.content, "Hello");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(
            resp.usage,
            Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12
            })
        );

        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Content(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.join(""), "Hello");
    }

    #[test]
    fn streamed_tool_calls_are_assembled() {
        let chunks = [
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"list_hosts"}}]}}]}"#,
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"group\":"}}]}}]}"#,
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ssh\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ];
        let (events, result) = run_chunks(&chunks);

        let resp = result.expect("finish ok");
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.message.tool_calls.len(), 1);
        let call = &resp.message.tool_calls[0];
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "list_hosts");
        assert_eq!(call.arguments, r#"{"group":"ssh"}"#);

        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::ToolCallStarted { index: 0, name, .. } if name == "list_hosts"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::ToolCallArguments { index: 0, delta } if delta == "\"ssh\"}"
        )));
    }

    #[test]
    fn synthetic_ids_fill_missing_call_ids() {
        let chunks = [
            r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"ping","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        ];
        let (_, result) = run_chunks(&chunks);

        let resp = result.expect("finish ok");
        assert_eq!(resp.message.tool_calls[0].id, "call_0");
    }

    #[test]
    fn json_errors_map_to_protocol() {
        let err = serde_json::from_str::<i32>("nope").expect_err("parse fails");
        let mapped = map_openai_error(OpenAIError::JSONDeserialize(err, String::new()));
        assert!(matches!(mapped, AiError::Protocol(_)));
    }

    /// DeepSeek-style reasoning streams as `StreamEvent::Reasoning` and is
    /// carried on the final response separately from the answer.
    #[test]
    fn reasoning_content_streams_separately_from_the_answer() {
        let chunks = [
            r#"{"choices":[{"delta":{"reasoning_content":"Let me think"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"…done.","content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ];
        let (events, result) = run_chunks(&chunks);

        let resp = result.expect("finish ok");
        assert_eq!(resp.reasoning, "Let me think…done.");
        assert_eq!(resp.message.content, "Hi");

        let reasoning: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Reasoning(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, "Let me think…done.");
    }

    /// Vendor quirks no longer break the stream: unknown finish reasons map
    /// to `Other`, and chunks missing every official field parse cleanly.
    #[test]
    fn vendor_quirks_survive_streaming() {
        let chunks = [
            r#"{}"#,
            r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"weird_vendor_reason"}]}"#,
        ];
        let (_, result) = run_chunks(&chunks);
        let resp = result.expect("finish ok");
        assert_eq!(resp.finish_reason, FinishReason::Other);
        assert_eq!(resp.message.content, "x");
    }

    /// The exact shape that broke the strict `Model` deserialization: a
    /// DeepSeek `/models` response omits `created` and adds extra fields
    /// (`name`, `context_window`, …). Ids must still come through, the
    /// advertised window is picked up, and malformed shapes must degrade to
    /// an empty list, not an error.
    #[test]
    fn model_list_parses_vendor_extensions() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"object":"list","data":[{"id":"deepseek-flash","object":"model","owned_by":"deepseek","name":"DeepSeek-V4.1-Flash","context_window":1048576,"max_output_tokens":393216},{"id":"deepseek-v4-pro","object":"model","owned_by":"deepseek"}]}"#,
        )
        .expect("payload parses as json");
        assert_eq!(
            extract_models(&payload),
            vec![
                ModelInfo {
                    id: "deepseek-flash".to_string(),
                    context_window: Some(1_048_576),
                },
                ModelInfo {
                    id: "deepseek-v4-pro".to_string(),
                    context_window: None,
                }
            ]
        );

        assert!(extract_models(&serde_json::json!({})).is_empty());
        assert!(extract_models(&serde_json::json!({"data": "nope"})).is_empty());
        assert!(extract_models(&serde_json::json!({"data": [{"name": "no-id"}]})).is_empty());
        // A zero window is treated as "not reported".
        assert_eq!(
            extract_models(&serde_json::json!({"data": [{"id": "m", "context_window": 0}]}))[0]
                .context_window,
            None
        );
    }
}
