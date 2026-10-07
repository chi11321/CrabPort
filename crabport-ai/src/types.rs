//! Provider-neutral conversation types shared by every [`ChatProvider`]
//! implementation ([`crate::provider`]).

use serde::{Deserialize, Serialize};

/// Author of a [`ChatMessage`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// Standing instructions, pinned ahead of the conversation.
    System,
    /// End-user input.
    #[default]
    User,
    /// Model output (may carry [`ChatMessage::tool_calls`]).
    Assistant,
    /// Tool execution result answering a specific `tool_call_id`.
    Tool,
}

impl Role {
    /// Wire name used by OpenAI-compatible APIs.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// One message in the conversation history.
///
/// Construct via [`ChatMessage::system`] / [`ChatMessage::user`] /
/// [`ChatMessage::assistant`] / [`ChatMessage::tool_result`].
#[derive(Clone, Debug, Default)]
pub struct ChatMessage {
    pub role: Role,
    /// Text content. Empty for pure tool-call assistant turns.
    pub content: String,
    /// Tool calls requested by the assistant (only on `Role::Assistant`).
    pub tool_calls: Vec<ToolCall>,
    /// Which call this message answers (only on `Role::Tool`).
    pub tool_call_id: Option<String>,
    /// Function name of the answered call (only on `Role::Tool`; some
    /// servers echo it back).
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            ..Default::default()
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            ..Default::default()
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            ..Default::default()
        }
    }

    /// Result of executing the tool call `tool_call_id`.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
            content: output.into(),
            ..Default::default()
        }
    }
}

/// A function invocation requested by the model.
///
/// `arguments` is kept as the raw JSON text the model streamed — parse
/// lazily at the tool boundary so malformed arguments can be reported back
/// to the model as a tool error instead of aborting the whole turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A callable tool advertised to the model.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the arguments.
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// Token accounting reported by the server, usually once per turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// Why the model stopped generating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason {
    /// Model finished normally.
    Stop,
    /// Model wants the advertised tools executed.
    ToolCalls,
    /// Context/token limit hit.
    Length,
    /// Content filter or any other/unmapped reason.
    Other,
}

/// A full completion request: conversation so far + advertised tools.
///
/// `model` travels with the request rather than living on the provider, so
/// callers can switch models between turns without rebuilding the provider.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
        }
    }

    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// Final result of a streamed completion: the fully-assembled assistant
/// message plus bookkeeping info.
#[derive(Clone, Debug)]
pub struct ChatResponse {
    pub message: ChatMessage,
    /// Chain-of-thought streamed ahead of the answer (DeepSeek-style
    /// `reasoning_content`). Display-only — never included in follow-up
    /// requests. Empty when the model doesn't expose one.
    pub reasoning: String,
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
}

/// Incremental updates pumped out while a completion streams.
#[derive(Clone, Debug)]
pub enum StreamEvent {
    /// Plain text content chunk.
    Content(String),
    /// Chain-of-thought chunk (DeepSeek-style `reasoning_content`),
    /// streamed ahead of the answer by reasoning models. Display-only —
    /// never sent back in follow-up requests.
    Reasoning(String),
    /// A tool call started. `index` groups subsequent argument deltas.
    ToolCallStarted {
        index: usize,
        id: String,
        name: String,
    },
    /// Raw JSON fragment appended to the tool call's argument string.
    ToolCallArguments { index: usize, delta: String },
    /// Token accounting, usually emitted once at the very end.
    Usage(TokenUsage),
}
