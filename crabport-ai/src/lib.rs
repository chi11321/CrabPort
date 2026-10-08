//! crabport-ai — LLM chat/streaming layer for CrabPort.
//!
//! Architecture:
//!
//! * [`types`] — provider-neutral conversation types ([`ChatMessage`],
//!   [`ToolCall`], [`ChatRequest`], …) plus the [`StreamEvent`] deltas a
//!   completion emits while it runs.
//! * [`provider`] — the [`ChatProvider`] contract. Blocking by design:
//!   implementations pump events into an `async_channel::Sender` until the
//!   stream ends, then return the fully-assembled [`ChatResponse`].
//! * [`openai`] — OpenAI-compatible provider built on the
//!   [`async-openai`](https://docs.rs/async-openai) crate: request
//!   serialization, SSE stream parsing and API error mapping are delegated
//!   to the library; this module only translates between its chat types and
//!   the provider-neutral ones above.
//! * [`spawn_chat_stream`] — runs a provider call on a dedicated worker
//!   thread and exposes the event flow as plain async channels, so GPUI
//!   views can await it from `cx.spawn` without importing a tokio runtime.
//!
//! The crate deliberately stays UI-free and storage-free: provider
//! credentials and settings live in `crabport-core` / the settings view;
//! this crate only knows how to talk to an API once handed a key.

pub mod error;
pub mod openai;
pub mod provider;
pub mod types;

use std::sync::Arc;

use async_channel::Receiver;

pub use error::AiError;
pub use openai::{ModelInfo, OpenAiProvider};
pub use provider::ChatProvider;
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, MESSAGE_OVERHEAD_TOKENS, Role,
    StreamEvent, TOOL_CALL_OVERHEAD_TOKENS, TokenUsage, ToolCall, ToolSpec, estimate_tokens,
};

/// Live handle to a completion running on the worker thread.
///
/// Cloning shares the same channels: any clone can [`ChatStream::cancel`]
/// the stream, and awaiting through one clone while another cancels is the
/// intended split (the chat panel keeps a handle for its Stop button while
/// the pump task awaits a clone).
///
/// Dropping the handle does not abort the worker immediately; call
/// [`ChatStream::cancel`] to close the event channel — the worker detects
/// it on its next send and unwinds with [`AiError::Cancelled`].
#[derive(Clone)]
pub struct ChatStream {
    events: Receiver<StreamEvent>,
    done: Receiver<Result<ChatResponse, AiError>>,
}

impl ChatStream {
    /// Await the next [`StreamEvent`]; returns `None` once the stream has
    /// ended (then read the final outcome via [`Self::result`]).
    pub async fn next_event(&self) -> Option<StreamEvent> {
        self.events.recv().await.ok()
    }

    /// Await the final outcome. Resolves when the worker finished;
    /// typically called right after [`Self::next_event`] returned `None`.
    pub async fn result(&self) -> Result<ChatResponse, AiError> {
        self.done.recv().await.unwrap_or(Err(AiError::Cancelled))
    }

    /// Cancel the stream early. Closes both channels: the worker observes
    /// the closed event channel on its next send and stops with
    /// [`AiError::Cancelled`], and its final-result send resolves instantly
    /// instead of blocking a worker thread on an unread outcome.
    pub fn cancel(&self) {
        self.events.close();
        self.done.close();
    }
}

/// Run `provider.complete_stream(&request)` on a dedicated worker thread.
///
/// Returns immediately. Streamed deltas arrive via
/// [`ChatStream::next_event`]; the assembled response via
/// [`ChatStream::result`]. Events are buffered (capacity 64) so short
/// token bursts don't stall the network reader.
pub fn spawn_chat_stream(provider: Arc<dyn ChatProvider>, request: ChatRequest) -> ChatStream {
    let (events_tx, events_rx) = async_channel::bounded(64);
    let (done_tx, done_rx) = async_channel::bounded(1);

    std::thread::Builder::new()
        .name("crabport-ai-stream".into())
        .spawn(move || {
            let outcome = provider.complete_stream(&request, &events_tx);
            events_tx.close();
            let _ = done_tx.send_blocking(outcome);
        })
        .expect("failed to spawn crabport-ai worker thread");

    ChatStream {
        events: events_rx,
        done: done_rx,
    }
}
