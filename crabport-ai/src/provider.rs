//! Provider abstraction.

use async_channel::Sender;

use crate::error::AiError;
use crate::types::{ChatRequest, ChatResponse, StreamEvent};

/// A chat-completion backend.
///
/// Implementations are blocking by design: [`ChatProvider::complete_stream`]
/// pumps [`StreamEvent`]s into `events` until the stream ends, then returns
/// the fully-assembled response. Callers run it on their own worker thread
/// — see [`crate::spawn_chat_stream`] for the ready-made wrapper that pairs
/// the worker with async channels usable from GPUI's executor.
///
/// Closing `events` from the consumer side is the cancellation protocol:
/// the implementation should observe the failed send and unwind with
/// [`AiError::Cancelled`].
pub trait ChatProvider: Send + Sync + 'static {
    fn complete_stream(
        &self,
        request: &ChatRequest,
        events: &Sender<StreamEvent>,
    ) -> Result<ChatResponse, AiError>;
}
