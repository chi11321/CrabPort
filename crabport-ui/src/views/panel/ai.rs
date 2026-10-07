//! AI assistant panel — chat with the configured provider.
//!
//! Sibling of [`super::sftp::SftpPanel`] / [`super::history_command_panel`]:
//! renders inside the right-hand panel strip's "AI" tab (see
//! `crabport-ui/src/layouts/panel.rs`). Visible whenever AI is enabled in
//! settings (`config.ai.enabled`), independent of the terminal backend.
//!
//! Layout (top → bottom): scrollable message list, inline error line, a
//! multi-line input area sitting directly on the panel background, and a
//! bottom row holding the combined provider·model picker (one searchable
//! dropdown listing every configured provider's models) plus a stop button
//! while a reply streams. Assistant turns show a reasoning model's
//! chain-of-thought as a dimmed block above the answer.
//!
//! Streaming model: [`AiPanel::send`] spawns a [`ChatStream`] (which runs
//! its HTTP work on its own worker thread) and pumps [`StreamEvent`]s into
//! the message list via `cx.spawn`; the Stop button cancels through a
//! `ChatStream` clone — clones share the same channels. A `generation`
//! counter drops late events from superseded streams.
//!
//! Model lists: fetched per provider from each endpoint's `/models` API
//! (via [`crabport_ai::OpenAiProvider::list_models`], offloaded to a
//! background thread with `smol::unblock`), auto-fetched once per endpoint
//! (editing an endpoint's URL produces a new key and re-fetches).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Sizable as _, Size};
use rust_i18n::t;

use crabport_ai::{
    AiError, ChatMessage, ChatRequest, ChatResponse, ChatStream, StreamEvent, ToolCall, ToolSpec,
};
use crabport_core::config;
use crabport_terminal::terminal::ExecOutput;
use gpui_component::ActiveTheme as _;
use gpui_component::text::{TextView, TextViewStyle};

use crate::ai;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::motion::RADIUS_MD;
use crate::views::terminal::TerminalView;

/// Pinned ahead of every conversation.
///
/// The agent prompt: the model may inspect and drive *its own* terminal, but
/// nothing happens without the user approving it. Keep the rules short and
/// concrete — the tools themselves carry the detail.
const SYSTEM_PROMPT: &str = "\
You are the built-in AI assistant of CrabPort, an SSH/SFTP client, working \
inside one terminal session. Be concise and practical.\n\n\
You can use three tools on that terminal:\n\
- terminal_read: read the most recent output lines. Use it before asking \
questions the screen can answer, and after a visible command to see what \
happened.\n\
- terminal_exec (your default): run one command out of band and get its \
captured stdout, stderr and exit code directly. It does not appear in the \
user's terminal and cannot answer prompts. Reach for this by default.\n\
- terminal_run: type one command into the user's live terminal, exactly as \
if they typed it, so its output streams where they can watch it. Use it only \
when the output must be followed while it runs (dev servers, log tails, slow \
builds, progress output) or the command is interactive (REPLs, anything that \
may prompt for input such as sudo).\n\n\
Every tool call is shown to the user for approval, so call a tool only when \
it earns its place: say why you are reading, and what you expect a command \
to do. Never batch speculative commands — one command, then read the result. \
When the user asks for something you can answer without touching the \
terminal, just answer.\n\n\
Safety: when a command could have destructive or otherwise high-risk \
effects — deleting or overwriting data, changing system or service \
configuration, touching production, anything hard to undo — repeat it in \
your reply **in bold** and say plainly what could go wrong, so the user \
cannot miss it while approving.";

/// Tools advertised to the model. Names are stable — the wire history replays
/// them, and the panel dispatches on them in [`ToolKind::of`].
fn agent_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            "terminal_read",
            "Read the most recent lines of this terminal's output (scrollback + \
             visible screen). Use it to see command results, error messages, or \
             what the user is looking at before answering.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "integer",
                        "description": "How many of the most recent lines to read (1-500)."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why you need this output — shown to the user for approval."
                    }
                },
                "required": ["lines", "purpose"]
            }),
        ),
        ToolSpec::new(
            "terminal_exec",
            "Run one command out of band and return its captured stdout + stderr \
             and exit code. This is the default execution tool: the command does \
             not appear in the user's terminal and has no tty, so anything \
             interactive (prompts, pagers, editors) will fail or time out \
             instead of waiting forever. Use terminal_run only when the output \
             must stream where the user can watch it, or the command needs to \
             prompt for input.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The exact command line to run."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect this command to do — shown to the user for approval."
                    }
                },
                "required": ["command", "expected"]
            }),
        ),
        ToolSpec::new(
            "terminal_run",
            "Type one command line into the user's live terminal, exactly as if \
             the user typed it and pressed Enter, and return the output that \
             appears. Use it only for long-running, streaming or interactive \
             commands (dev servers, log tails, builds, REPLs, anything that \
             may prompt for input such as sudo) — otherwise prefer \
             terminal_exec.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The exact command line to run."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect this command to do — shown to the user for approval."
                    }
                },
                "required": ["command", "expected"]
            }),
        ),
    ]
}

/// The user's decision on one tool call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolDecision {
    /// Waiting for the user; the turn does not continue until it is resolved.
    Pending,
    Allowed,
    Denied,
    /// The call cannot run on this connection — resolved without a click
    /// (e.g. captured execution on a backend with a single byte stream).
    /// The result explains why, and the loop continues so the model can
    /// retry with a tool that works.
    Unavailable,
}

/// Which agent tool a call names. The names are the wire contract with the
/// model, so they are mapped in exactly one place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToolKind {
    /// `terminal_read` — snapshot of the recent screen, answered inline.
    Read,
    /// `terminal_exec` — run out of band, output captured directly.
    ExecCapture,
    /// `terminal_run` — typed into the live terminal, output read from the
    /// screen once it settles.
    Run,
    /// Anything else the model invented; refused with a readable error.
    Unknown,
}

impl ToolKind {
    fn of(name: &str) -> Self {
        match name {
            "terminal_read" => Self::Read,
            "terminal_exec" => Self::ExecCapture,
            "terminal_run" => Self::Run,
            _ => Self::Unknown,
        }
    }
}

/// One tool call the assistant asked for, together with its decision and —
/// once allowed — its result. Lives on the [`DisplayMessage`] that requested
/// it, so the conversation list can render the approval card and the wire
/// history can replay the call and its result.
#[derive(Clone)]
struct ToolCallState {
    call: ToolCall,
    /// Parsed `call.arguments`; `None` when the model sent invalid JSON (the
    /// card then shows the raw text and approving executes nothing).
    args: Option<serde_json::Value>,
    decision: ToolDecision,
    /// Result text: tool output, or why it was refused. `None` while the call
    /// is still running (or still waiting for a decision).
    result: Option<String>,
    /// True between approving an execution and its output settling — the card
    /// shows a running state and the loop waits instead of continuing.
    running: bool,
    /// Exit status of a captured (`terminal_exec`) command, when one was
    /// reported. `None` for reads, visible runs, and commands whose status
    /// never arrived.
    exit_code: Option<u32>,
    /// True when a captured command outlived its deadline; the card says so
    /// and the model is told the output was cut short.
    timed_out: bool,
}

impl ToolCallState {
    /// Human-readable view of the card's arguments, falling back to the raw
    /// JSON text when the model sent malformed arguments.
    fn arg_str(&self, key: &str) -> Option<&str> {
        match self.args.as_ref()?.get(key)?.as_str() {
            Some(value) => Some(value),
            None => None,
        }
    }

    fn arg_u64(&self, key: &str) -> Option<u64> {
        self.args.as_ref()?.get(key)?.as_u64()
    }

    /// The result as the model sees it. An exit status or the timeout
    /// marker is prefixed so even an empty output carries the outcome —
    /// "it ran and printed nothing" and "it never finished" must not look
    /// the same.
    fn result_for_model(&self) -> String {
        let body = self.result.clone().unwrap_or_default();
        if self.timed_out {
            return format!("[timed out; output so far]\n{body}");
        }
        match self.exit_code {
            Some(code) => format!("[exit code: {code}]\n{body}"),
            None => body,
        }
    }
}

/// Body text size for the conversation (user bubbles, assistant answers and
/// the live streaming tail), in px. Deliberately below the app default
/// (16px — gpui-component's `Theme::font_size`) so the panel reads as a
/// compact chat sidebar instead of a document. Markdown headings and code
/// blocks are scaled from this too.
const CONVERSATION_TEXT_SIZE: f32 = 13.0;

/// How many lines of terminal output a `terminal_read` (and the execute
/// output wait) samples. Plenty for a screenful plus recent scrollback.
const AGENT_READ_LINES: usize = 400;

/// How often the execute wait re-samples the terminal's output.
const OUTPUT_POLL: std::time::Duration = std::time::Duration::from_millis(120);
/// How long the output must stop changing before a command counts as done.
const OUTPUT_QUIET: std::time::Duration = std::time::Duration::from_millis(360);
/// Upper bound on the wait: a command that keeps printing (a log tail, a
/// build) gets this long before its output-so-far is handed over.
const OUTPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// Cap on the text one tool result may carry back to the model, in bytes. A
/// command that prints a whole file would otherwise fill the conversation's
/// context with one answer; the *tail* is kept, which is where a command's
/// interesting output (errors, the final summary) usually is.
const MAX_TOOL_RESULT_BYTES: usize = 16 * 1024;

/// How long an implicit (`terminal_exec`) command may run before its backend
/// gives up and hands over the output so far. Generous enough for a normal
/// non-interactive command, bounded so a `tail -f`-style slip-up cannot
/// wedge the conversation forever.
const EXEC_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Terminal session an AI panel is bound to.
///
/// One panel instance exists per terminal pane (see
/// `CrabportApp::ai_panels`), so every terminal keeps its own conversation —
/// and the agent's terminal tools act on *this* session, never on another
/// tab's. The handle is weak so a closed pane doesn't keep its view alive;
/// tool calls on a dead session report "terminal closed".
#[derive(Clone)]
pub struct AiSession {
    /// Pane id — the key of `CrabportApp::pane_views`.
    pub pane_id: u64,
    /// The pane's terminal view. Tools read output and send input through
    /// this handle.
    pub terminal: WeakEntity<TerminalView>,
}

/// One entry in the combined provider·model picker.
#[derive(Clone)]
struct ComboItem {
    provider_id: String,
    model: String,
}

/// Committed display role.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayRole {
    User,
    Assistant,
}

/// One committed chat turn for display. `reasoning` (reasoning models'
/// chain-of-thought) is display-only and never sent back in follow-up
/// requests, which is why the panel keeps its own shape instead of reusing
/// the wire `ChatMessage`. `reasoning_expanded` drives the thinking
/// section's disclosure (collapsed by default once a turn completes; the
/// live streaming tail always shows it expanded).
struct DisplayMessage {
    role: DisplayRole,
    content: String,
    reasoning: String,
    reasoning_expanded: bool,
    /// Tool calls this turn asked for. Empty for plain turns; the assistant
    /// message is only sent back to the model with its calls and results
    /// once every call has been resolved by the user.
    tool_calls: Vec<ToolCallState>,
}

/// AI assistant panel view.
pub struct AiPanel {
    /// Committed turns, oldest first (the system prompt is prepended at
    /// request time, not stored).
    messages: Vec<DisplayMessage>,
    /// Live assistant text streaming in right now.
    stream_buf: String,
    /// Chain-of-thought streaming in right now (reasoning models).
    stream_reasoning: String,
    /// State for the virtualized message list (gpui's `List`): only items
    /// inside the viewport are rendered each frame. Stick-to-bottom is
    /// derived from the last item's rendered bounds — gpui 0.2.2's
    /// `ListState` predates the follow-mode API Zed's vendored gpui has.
    list_state: ListState,
    /// In-flight stream. The pump task owns a clone; this handle is what
    /// the Stop button cancels through.
    stream: Option<ChatStream>,
    /// Bumped on every send; late events from an older stream are dropped.
    generation: u64,
    /// Lazily-created message input area. Auto-grow: starts at 3 visible
    /// rows, expands with the draft up to 6 (then scrolls internally).
    input: Option<Entity<InputState>>,
    /// Lazily-created search box for the combined picker.
    combo_search: Option<Entity<InputState>>,
    /// Set by `send`, consumed by the next `set_state` (which has a
    /// `Window`) to clear the input box.
    pending_clear: bool,
    /// Whether the message input currently has keyboard focus. Drives the
    /// custom placeholder overlay, which is hidden while focused so the
    /// caret sits on clean space instead of on top of the hint text.
    input_focused: bool,
    /// Inline error line (not-configured, API errors, fetch failures).
    error: Option<String>,
    /// Model ids per provider entry id, fetched from `{base}/models`
    /// (memory only).
    models_by_provider: HashMap<String, Vec<String>>,
    /// `"{provider_id}|{base_url}"` keys already fetched (or attempted), so
    /// each endpoint is fetched exactly once per session — editing the URL
    /// produces a new key and re-fetches.
    fetch_attempts: HashSet<String>,
    /// Number of model-list fetches in flight (drives the picker's
    /// "fetching" placeholder).
    fetching_count: usize,
    /// Combined picker open flag.
    combo_open: bool,
    /// Session id for this conversation, sent as `x-opencode-session` to
    /// gateways that route by session (OpenCode's Zen / Go endpoints reject
    /// chat requests without one). Generated once per panel — the panel
    /// holds a single conversation, and the id only has to be stable across
    /// that conversation's requests.
    session_id: String,
    /// The terminal this panel (and its agent) belongs to.
    session: AiSession,
}

impl AiPanel {
    /// Build the panel for one terminal session.
    pub fn new(session: AiSession) -> Self {
        Self {
            messages: Vec::new(),
            stream_buf: String::new(),
            stream_reasoning: String::new(),
            list_state: ListState::new(0, ListAlignment::Top, px(1000.)),
            stream: None,
            generation: 0,
            input: None,
            combo_search: None,
            pending_clear: false,
            input_focused: false,
            error: None,
            models_by_provider: HashMap::new(),
            fetch_attempts: HashSet::new(),
            fetching_count: 0,
            combo_open: false,
            session_id: new_session_id(),
            session,
        }
    }

    /// The terminal this panel belongs to.
    pub fn session(&self) -> &AiSession {
        &self.session
    }

    /// Called by the content layout every render.
    pub fn set_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_input(window, cx);
        self.ensure_combo_search(window, cx);

        if self.pending_clear {
            if let Some(input) = self.input.clone() {
                input.update(cx, |state, cx| state.set_value("", window, cx));
            }
            self.pending_clear = false;
        }

        // Auto-fetch each provider's model list once per endpoint so the
        // picker has options without a manual refresh. Scheduled via
        // `defer_in` — this runs inside the app's render pass, and spawning
        // network work must not happen there.
        let ai_cfg = config::snapshot().ai;
        let mut to_fetch: Vec<String> = Vec::new();
        for entry in &ai_cfg.providers {
            if entry.base_url.trim().is_empty() {
                continue;
            }
            let key = format!("{}|{}", entry.id, entry.base_url);
            if self.fetch_attempts.insert(key) {
                to_fetch.push(entry.id.clone());
            }
        }
        if !to_fetch.is_empty() {
            cx.defer_in(window, move |panel, _w, cx| {
                for provider_id in to_fetch {
                    panel.fetch_models(provider_id, cx);
                }
            });
        }
    }

    fn ensure_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
            return;
        }
        let state = cx.new(|cx| {
            // Placeholder intentionally left empty: the library renders
            // placeholders without wrapping, so the panel draws its own
            // wrapping overlay instead (see `render`).
            InputState::new(window, cx).auto_grow(6, 12)
        });
        cx.subscribe(&state, |this, _input, event: &InputEvent, cx| {
            match event {
                // Plain Enter sends; Cmd/Ctrl+Enter (`secondary`) keeps
                // the newline the multi-line editor just inserted.
                InputEvent::PressEnter { secondary } => {
                    if !*secondary {
                        this.submit(cx);
                    }
                }
                InputEvent::Focus => {
                    this.input_focused = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.input_focused = false;
                    cx.notify();
                }
                _ => {}
            }
        })
        .detach();
        self.input = Some(state);
    }

    /// A plain Enter in the input area. The editor inserts the newline
    /// *before* emitting `PressEnter`, so Enter on a box with nothing to
    /// send would leave that newline sitting there — a real send clears the
    /// box anyway, but an empty one has nothing to clear, so drop it here
    /// (`pending_clear` is applied on the next `set_state`, which has the
    /// window the clear needs).
    fn submit(&mut self, cx: &mut Context<Self>) {
        let empty = self
            .input
            .as_ref()
            .map(|state| state.read(cx).value().trim().is_empty())
            .unwrap_or(true);
        if empty {
            self.pending_clear = true;
            cx.notify();
            return;
        }
        self.send(cx);
    }

    fn ensure_combo_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.combo_search.is_some() {
            return;
        }
        self.combo_search = Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("ai_panel.search_models").to_string())
        }));
    }

    /// Send the current input text. No-op while a reply streams.
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.stream.is_some() {
            return;
        }
        let Some(input) = self.input.clone() else {
            return;
        };
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        // A turn that asked for tools is not over until every call has been
        // approved or refused: the wire history can't carry an unanswered
        // call, so hold the draft instead of sending a broken request.
        if self.pending_tool_calls() > 0 {
            self.error = Some(t!("ai_panel.tools_pending").to_string());
            cx.notify();
            return;
        }
        let Some(provider) = ai::resolve_provider_session(cx, &self.session_id) else {
            self.error = Some(t!("ai_panel.not_configured").to_string());
            cx.notify();
            return;
        };

        // Commit the user turn for display (and history) before building
        // the wire request, so it is included exactly once. Follow it down
        // if the view was already at the bottom.
        let stick = self.is_list_at_bottom();
        self.messages.push(DisplayMessage {
            role: DisplayRole::User,
            content: text,
            reasoning: String::new(),
            reasoning_expanded: false,
            tool_calls: Vec::new(),
        });
        if stick {
            self.scroll_list_to_end();
        }

        let model = config::snapshot().ai.model;
        let wire = self.wire_history();
        let request = ChatRequest::new(model)
            .with_messages(wire)
            .with_tools(agent_tools());

        self.pending_clear = true;
        self.start_stream(provider, request, cx);
    }

    /// Spawn the worker for one request and pump its events into the panel.
    ///
    /// Shared by the first turn ([`Self::send`]) and every follow-up turn the
    /// agent loop starts after tools ran ([`Self::continue_after_tools`]) —
    /// both want the same streaming tail, cancellation and generation guard.
    fn start_stream(
        &mut self,
        provider: crabport_ai::OpenAiProvider,
        request: ChatRequest,
        cx: &mut Context<Self>,
    ) {
        let stream = crabport_ai::spawn_chat_stream(Arc::new(provider), request);
        self.generation += 1;
        let generation = self.generation;
        self.stream = Some(stream.clone());
        self.stream_buf.clear();
        self.stream_reasoning.clear();
        self.error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next_event().await {
                let _ = this.update(cx, |panel, cx| {
                    if panel.generation != generation {
                        return;
                    }
                    // Stick to the bottom only while the user is already
                    // there; scrolling up mid-stream keeps their view put.
                    let stick = panel.is_list_at_bottom();
                    match event {
                        StreamEvent::Content(chunk) => panel.stream_buf.push_str(&chunk),
                        StreamEvent::Reasoning(chunk) => panel.stream_reasoning.push_str(&chunk),
                        _ => return,
                    }
                    if stick {
                        panel.scroll_list_to_end();
                    }
                    cx.notify();
                });
            }
            let outcome = stream.result().await;
            let _ = this.update(cx, |panel, cx| {
                if panel.generation == generation {
                    panel.finish_turn(outcome, cx);
                }
            });
        })
        .detach();
    }

    /// Called from the Stop button: closes the stream's channels; the pump
    /// task observes it and finishes the turn with `Cancelled`.
    fn stop(&mut self) {
        if let Some(stream) = &self.stream {
            stream.cancel();
        }
    }

    fn finish_turn(&mut self, outcome: Result<ChatResponse, AiError>, cx: &mut Context<Self>) {
        self.stream = None;
        let partial = std::mem::take(&mut self.stream_buf);
        let partial_reasoning = std::mem::take(&mut self.stream_reasoning);
        match outcome {
            Ok(response) => {
                let calls = response.message.tool_calls.clone();
                let text = if response.message.content.trim().is_empty() {
                    partial
                } else {
                    response.message.content
                };
                let reasoning = if response.reasoning.trim().is_empty() {
                    partial_reasoning
                } else {
                    response.reasoning
                };
                self.push_partial(text, reasoning, Vec::new());
                // Tool calls requested by this turn are attached now — the
                // card rows are what the user approves before anything runs.
                if !calls.is_empty() {
                    self.push_tool_calls(calls, cx);
                }
            }
            Err(AiError::Cancelled) => {
                // Keep whatever streamed in before the stop.
                self.push_partial(partial, partial_reasoning, Vec::new());
            }
            Err(err) => {
                self.push_partial(partial, partial_reasoning, Vec::new());
                self.error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    /// Attach the tool calls from one assistant turn to the conversation.
    ///
    /// The assistant's own text (if any) is already committed by
    /// [`Self::push_partial`]; when the model answered with *only* tool calls
    /// that leaves nothing to show, so this pushes an empty assistant turn to
    /// carry the calls — the card is the turn.
    fn push_tool_calls(&mut self, calls: Vec<ToolCall>, cx: &mut Context<Self>) {
        // An implicit execution on a backend that cannot run one is refused
        // right here, without a click: nothing would happen on approval, and
        // the model is better off hearing "use terminal_run" immediately so
        // it can retry while the user watches.
        let capture_block = match self.session.terminal.upgrade() {
            None => Some(t!("ai_panel.tool_terminal_closed").to_string()),
            Some(view) if !view.read(cx).allow_exec_capture() => {
                Some(t!("ai_panel.tool_exec_unsupported").to_string())
            }
            Some(_) => None,
        };

        let mut auto_resolved = false;
        let states: Vec<ToolCallState> = calls
            .into_iter()
            .map(|call| {
                let args = serde_json::from_str::<serde_json::Value>(&call.arguments).ok();
                let mut state = ToolCallState {
                    call,
                    args,
                    decision: ToolDecision::Pending,
                    result: None,
                    running: false,
                    exit_code: None,
                    timed_out: false,
                };
                if ToolKind::of(&state.call.name) == ToolKind::ExecCapture {
                    if let Some(reason) = &capture_block {
                        state.decision = ToolDecision::Unavailable;
                        state.result = Some(reason.clone());
                        auto_resolved = true;
                    }
                }
                state
            })
            .collect();

        let attach_to_last = self
            .messages
            .last()
            .is_some_and(|msg| msg.role == DisplayRole::Assistant && msg.tool_calls.is_empty());
        if attach_to_last {
            self.messages.last_mut().unwrap().tool_calls = states;
        } else {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                content: String::new(),
                reasoning: String::new(),
                reasoning_expanded: false,
                tool_calls: states,
            });
        }
        // The last row grew (or a new one appeared); make sure the list
        // re-measures it, and bring it into view — the user has something to
        // approve.
        self.remeasure_row(self.list_row_of_message(self.messages.len() - 1));
        self.scroll_list_to_end();
        cx.notify();
        // Some calls were answered without the user: let the model react to
        // the refusal instead of waiting for a click nobody needs to make.
        if auto_resolved {
            self.maybe_continue_after_tools(cx);
        }
    }

    /// Tool calls the loop is still waiting on: awaiting the user's decision,
    /// or executing with its output not settled yet. The panel refuses to
    /// send while any of these exist, and the agent loop only continues once
    /// there are none.
    fn pending_tool_calls(&self) -> usize {
        self.messages
            .iter()
            .flat_map(|msg| msg.tool_calls.iter())
            .filter(|call| call.decision == ToolDecision::Pending || call.running)
            .count()
    }

    /// The request history: the system prompt, every committed turn, and —
    /// for assistant turns that asked for tools — the calls plus their
    /// results, which is the shape OpenAI-compatible servers require (a
    /// `tool` message must answer the `tool_calls` of the message before it).
    ///
    /// Turns with unresolved calls are skipped entirely: they cannot be
    /// replayed, and [`Self::send`] refuses to build a request while any call
    /// is pending.
    fn wire_history(&self) -> Vec<ChatMessage> {
        let mut wire: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 1);
        wire.push(ChatMessage::system(SYSTEM_PROMPT));
        for msg in &self.messages {
            match msg.role {
                DisplayRole::User => wire.push(ChatMessage::user(msg.content.clone())),
                DisplayRole::Assistant => {
                    if msg.tool_calls.iter().any(|c| c.result.is_none()) {
                        continue;
                    }
                    let mut assistant = ChatMessage::assistant(msg.content.clone());
                    assistant.tool_calls = msg.tool_calls.iter().map(|c| c.call.clone()).collect();
                    wire.push(assistant);
                    for call in &msg.tool_calls {
                        wire.push(ChatMessage::tool_result(
                            call.call.id.clone(),
                            call.call.name.clone(),
                            call.result_for_model(),
                        ));
                    }
                }
            }
        }
        wire
    }

    /// Record the user's decision for one call and, once every call of the
    /// turn is resolved, send the results back to the model — that is what
    /// keeps the agent loop going, and it is why an approval is never a
    /// silent no-op.
    fn resolve_tool(
        &mut self,
        msg_index: usize,
        call_index: usize,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.tool_state_mut(msg_index, call_index) else {
            return;
        };
        if state.decision != ToolDecision::Pending {
            return;
        }
        let call = state.call.clone();
        let args = state.args.clone();
        // The model's arguments were unparseable: nothing may run, and it must
        // be told that instead of assuming success.
        if args.is_none() {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_bad_args").to_string(),
                ToolDecision::Denied,
                cx,
            );
            return;
        }
        if !allow {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_denied").to_string(),
                ToolDecision::Denied,
                cx,
            );
            return;
        }

        // Both execution tools take `command`; a missing one is malformed
        // even when the JSON itself parsed.
        let command = args
            .as_ref()
            .and_then(|args| args.get("command")?.as_str())
            .map(str::to_string);
        match ToolKind::of(&call.name) {
            ToolKind::Read => {
                let result = self.execute_tool(&call, cx);
                self.settle_tool_result(msg_index, call_index, result, ToolDecision::Allowed, cx);
            }
            ToolKind::ExecCapture => match command {
                Some(command) => self.start_capture(msg_index, call_index, command, cx),
                None => self.settle_tool_result(
                    msg_index,
                    call_index,
                    t!("ai_panel.tool_bad_args").to_string(),
                    ToolDecision::Denied,
                    cx,
                ),
            },
            ToolKind::Run => match command {
                Some(command) => self.start_visible_run(msg_index, call_index, command, cx),
                None => self.settle_tool_result(
                    msg_index,
                    call_index,
                    t!("ai_panel.tool_bad_args").to_string(),
                    ToolDecision::Denied,
                    cx,
                ),
            },
            ToolKind::Unknown => self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_unknown", name = call.name.as_str()).to_string(),
                ToolDecision::Denied,
                cx,
            ),
        }
    }

    /// The state of one tool call, if that call exists.
    fn tool_state_mut(
        &mut self,
        msg_index: usize,
        call_index: usize,
    ) -> Option<&mut ToolCallState> {
        self.messages
            .get_mut(msg_index)?
            .tool_calls
            .get_mut(call_index)
    }

    /// Store a decision plus its result, re-measure the card and continue
    /// the loop when nothing else is outstanding.
    fn settle_tool_result(
        &mut self,
        msg_index: usize,
        call_index: usize,
        result: String,
        decision: ToolDecision,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.decision = decision;
            state.result = Some(result);
            state.running = false;
        }
        self.settle_tool(msg_index, call_index, cx);
    }

    /// Re-measure one card's row — it just changed height, from buttons to a
    /// result or from a placeholder to real output — and let the model act on
    /// the results once every call of the turn has one.
    fn settle_tool(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        self.maybe_continue_after_tools(cx);
        cx.notify();
    }

    /// Start an approved implicit (`terminal_exec`) command: out of band on
    /// an extra channel/process, its output captured directly.
    ///
    /// The command is run in the directory the terminal last reported (when
    /// there is one) so it behaves like something typed there, but nothing is
    /// written to the shell — the session and its screen stay untouched.
    fn start_capture(
        &mut self,
        msg_index: usize,
        call_index: usize,
        command: String,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.session.terminal.upgrade() else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_terminal_closed").to_string(),
                ToolDecision::Allowed,
                cx,
            );
            return;
        };
        // Backends that can't run a command out of band: say so instead of
        // half-running it in the user's shell.
        if !terminal.read(cx).allow_exec_capture() {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_exec_unsupported").to_string(),
                ToolDecision::Unavailable,
                cx,
            );
            return;
        }
        let cwd = terminal.read(cx).cwd().map(str::to_string);
        let effective = with_cwd(&command, cwd.as_deref());
        let rx = terminal
            .read(cx)
            .exec_capture(&effective, EXEC_CAPTURE_TIMEOUT);

        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.decision = ToolDecision::Allowed;
            state.running = true;
        }
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        cx.notify();

        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            // The backend's completion callback fires exactly once; a closed
            // receiver without an outcome means the backend went away with
            // the session.
            let out = rx.recv().await.unwrap_or_else(|_| ExecOutput {
                output: t!("ai_panel.tool_exec_capture_failed").to_string(),
                ..Default::default()
            });
            let _ = entity.update(cx, |panel, cx| {
                panel.finish_tool_capture(msg_index, call_index, out, cx);
            });
        })
        .detach();
    }

    /// Start an approved explicit (`terminal_run`) command: type the line
    /// into the live shell — the user watches it there — then hand over the
    /// output the screen gained once it settles.
    fn start_visible_run(
        &mut self,
        msg_index: usize,
        call_index: usize,
        command: String,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.session.terminal.upgrade() else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_terminal_closed").to_string(),
                ToolDecision::Allowed,
                cx,
            );
            return;
        };
        // Sample the terminal *before* the command runs: the tool result is
        // what appears after this point, so the model gets the command's own
        // output instead of a round trip through `terminal_read`.
        let before = terminal.read(cx).dump_text(AGENT_READ_LINES);
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.decision = ToolDecision::Allowed;
            state.running = true;
        }
        // Same path the snippets panel uses: the line, then a carriage
        // return, so the shell runs it as if it had been typed.
        let mut bytes = command.into_bytes();
        bytes.push(b'\r');
        terminal.read(cx).write_raw(&bytes);
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        self.await_command_output(msg_index, call_index, before, cx);
        cx.notify();
    }

    /// Send the resolved tool results back to the model, so it can act on
    /// them.
    ///
    /// There is deliberately no cap on how many rounds a turn may take: every
    /// call is approved by the user before it runs, so the loop only continues
    /// while they keep saying yes — and they can stop it at any point with the
    /// composer's stop button.
    fn continue_after_tools(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = ai::resolve_provider_session(cx, &self.session_id) else {
            self.error = Some(t!("ai_panel.not_configured").to_string());
            cx.notify();
            return;
        };
        let model = config::snapshot().ai.model;
        let request = ChatRequest::new(model)
            .with_messages(self.wire_history())
            .with_tools(agent_tools());
        self.start_stream(provider, request, cx);
    }

    /// Run one approved tool call that answers inline against this panel's
    /// terminal and return the text to hand back to the model.
    ///
    /// Only `terminal_read` lands here — the two execution tools start async
    /// work instead (see [`Self::start_capture`] / [`Self::start_visible_run`])
    /// because the useful answer to "run this" is the command's output, not an
    /// acknowledgement.
    fn execute_tool(&mut self, call: &ToolCall, cx: &mut Context<Self>) -> String {
        let Some(terminal) = self.session.terminal.upgrade() else {
            return t!("ai_panel.tool_terminal_closed").to_string();
        };
        match ToolKind::of(&call.name) {
            ToolKind::Read => {
                let lines = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .ok()
                    .and_then(|args| args.get("lines")?.as_u64())
                    .unwrap_or(200) as usize;
                let text = terminal.read(cx).dump_text(lines);
                if text.trim().is_empty() {
                    t!("ai_panel.tool_read_empty").to_string()
                } else {
                    text
                }
            }
            _ => t!("ai_panel.tool_unknown", name = call.name.as_str()).to_string(),
        }
    }

    /// Wait for an approved command's output to settle and hand it to the
    /// model as the tool result.
    ///
    /// "Settled" is a heuristic, because a shell gives no signal that a
    /// command finished unless it is integrated (OSC 133): the output is
    /// sampled every [`OUTPUT_POLL`] and considered done after
    /// [`OUTPUT_QUIET`] without any change, or after [`OUTPUT_TIMEOUT`] when
    /// the command keeps printing. What the model gets is the text that
    /// appeared *since* the command was written — that is the answer to
    /// "run this", and it saves a round trip through `terminal_read`.
    fn await_command_output(
        &mut self,
        msg_index: usize,
        call_index: usize,
        before: String,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.session.terminal.upgrade() else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_terminal_closed").to_string(),
                ToolDecision::Allowed,
                cx,
            );
            return;
        };
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let started = std::time::Instant::now();
            let mut last = before.clone();
            let mut quiet_since = std::time::Instant::now();
            let mut timed_out = false;
            loop {
                cx.background_executor().timer(OUTPUT_POLL).await;
                let Ok(now) =
                    terminal.read_with(cx, |view, _cx| view.try_dump_text(AGENT_READ_LINES))
                else {
                    break; // terminal view went away
                };
                let Some(now) = now else {
                    continue; // lock held by the reader thread; try next tick
                };
                if now != last {
                    last = now;
                    quiet_since = std::time::Instant::now();
                } else if quiet_since.elapsed() >= OUTPUT_QUIET {
                    break;
                }
                if started.elapsed() >= OUTPUT_TIMEOUT {
                    timed_out = true;
                    break;
                }
            }
            let result = new_since(&before, &last);
            let result = if result.trim().is_empty() {
                if timed_out {
                    t!("ai_panel.tool_exec_no_output_timeout").to_string()
                } else {
                    t!("ai_panel.tool_exec_no_output").to_string()
                }
            } else {
                result
            };
            let _ = entity.update(cx, |panel, cx| {
                panel.settle_tool_result(msg_index, call_index, result, ToolDecision::Allowed, cx);
            });
        })
        .detach();
    }

    /// Store a captured (`terminal_exec`) result: the output, plus the exit
    /// status and timeout flag that let the card and the model-visible result
    /// be honest about how the command ended.
    fn finish_tool_capture(
        &mut self,
        msg_index: usize,
        call_index: usize,
        out: ExecOutput,
        cx: &mut Context<Self>,
    ) {
        let text = cap_tool_result(out.output.trim_matches('\n'));
        let text = if text.trim().is_empty() {
            t!("ai_panel.tool_exec_no_output").to_string()
        } else {
            text
        };
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.result = Some(text);
            state.exit_code = out.exit_code;
            state.timed_out = out.timed_out;
            state.running = false;
        }
        self.settle_tool(msg_index, call_index, cx);
    }

    /// Continue the loop when every tool call has produced a result; wait
    /// otherwise (a second call may still be running, or await the user).
    fn maybe_continue_after_tools(&mut self, cx: &mut Context<Self>) {
        if self
            .messages
            .iter()
            .flat_map(|msg| msg.tool_calls.iter())
            .any(|call| call.result.is_none())
        {
            return;
        }
        self.continue_after_tools(cx);
    }

    /// Commit an assistant turn unless both streams (answer and reasoning)
    /// are blank.
    fn push_partial(&mut self, content: String, reasoning: String, tool_calls: Vec<ToolCallState>) {
        if content.trim().is_empty() && reasoning.trim().is_empty() && tool_calls.is_empty() {
            return;
        }
        self.messages.push(DisplayMessage {
            role: DisplayRole::Assistant,
            content,
            reasoning,
            // Disclosure starts collapsed; the live streaming tail shows
            // reasoning in full while it streams.
            reasoning_expanded: false,
            tool_calls,
        });
        // The committed block (Markdown) replaces the streaming tail (plain
        // text) at the same list index, usually with a different height —
        // drop the cached row measurement so the list re-measures it.
        self.remeasure_row(self.list_row_of_message(self.messages.len() - 1));
    }

    /// Flip one turn's thinking disclosure (the whole header row is the
    /// click target, matching Zed's thinking block).
    fn toggle_thinking(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(msg) = self.messages.get_mut(index) {
            msg.reasoning_expanded = !msg.reasoning_expanded;
            cx.notify();
        }
    }

    /// Number of rows the conversation list holds right now: one per
    /// committed turn, one per tool call (cards sit between turns), and one
    /// for the live streaming tail while there is one.
    ///
    /// The list state is keyed by these rows, so **every** place that splices
    /// or measures a row has to agree with this count — a card that the count
    /// doesn't know about is a card the list never renders.
    fn list_row_count(&self) -> usize {
        self.messages.len()
            + self.total_tool_calls()
            + usize::from(!self.stream_reasoning.is_empty() || !self.stream_buf.is_empty())
    }

    /// Total tool calls across the conversation (one card row each).
    fn total_tool_calls(&self) -> usize {
        self.messages.iter().map(|msg| msg.tool_calls.len()).sum()
    }

    /// List row index of `messages[msg_index]`.
    fn list_row_of_message(&self, msg_index: usize) -> usize {
        msg_index
            + self
                .messages
                .iter()
                .take(msg_index)
                .map(|msg| msg.tool_calls.len())
                .sum::<usize>()
    }

    /// List row index of the card for one tool call.
    fn list_row_of_tool_call(&self, msg_index: usize, call_ix: usize) -> usize {
        self.list_row_of_message(msg_index) + 1 + call_ix
    }

    /// Re-measure one row: drop its cached measurement so the next frame lays
    /// it out again (used when a card grows from buttons into a result, or a
    /// turn's content changes identity).
    fn remeasure_row(&self, row: usize) {
        let known = self.list_state.item_count();
        if row < known {
            self.list_state.splice(row..row + 1, 1);
        }
    }

    /// Whether the message list is scrolled to (or within 4px of) the
    /// bottom. Derived from the last item's rendered bounds: when the last
    /// item is rendered and its bottom edge is at/below the viewport's, the
    /// view is pinned to the end.
    fn is_list_at_bottom(&self) -> bool {
        let count = self.list_state.item_count();
        if count == 0 {
            return true;
        }
        self.list_state
            .bounds_for_item(count - 1)
            .is_some_and(|bounds| {
                bounds.bottom() <= self.list_state.viewport_bounds().bottom() + px(4.0)
            })
    }

    /// Pin the virtualized list to the very end. A scroll target of
    /// `item_count` clamps to the bottom (`scroll_to` clamps the index and
    /// zeroes the in-item offset).
    fn scroll_list_to_end(&self) {
        self.list_state.scroll_to(ListOffset {
            item_ix: self.list_state.item_count(),
            offset_in_item: px(0.),
        });
    }

    /// Snapshot of the conversation for the virtualized list's render
    /// closure, which cannot borrow the view. Rebuilt each render; the
    /// string clones are cheap relative to element layout.
    /// Snapshot of the conversation for the virtualized list's render
    /// closure, which cannot borrow the view. `cwd` is the terminal's
    /// last-reported working directory (resolved by `render`, which has
    /// `cx`); tool cards show it on `execute` requests.
    ///
    /// Rows and messages are *different* indices: a message with tool calls
    /// is followed by one card row per call, so `messages[ix]` is not
    /// `items[ix]`. Cards carry the message index they belong to (for the
    /// approve/deny callbacks) and the assistant rows carry theirs (for the
    /// thinking toggle) — mixing the two is what made every approval after
    /// the first one land on a message that doesn't exist.
    fn message_items(&self, cwd: Option<&str>) -> Vec<MessageItem> {
        let mut items: Vec<MessageItem> = Vec::with_capacity(self.list_row_count());
        for (message_ix, msg) in self.messages.iter().enumerate() {
            items.push(match msg.role {
                DisplayRole::User => MessageItem::User {
                    content: msg.content.clone(),
                },
                DisplayRole::Assistant => MessageItem::Assistant {
                    message_ix,
                    reasoning: msg.reasoning.clone(),
                    content: msg.content.clone(),
                    reasoning_expanded: msg.reasoning_expanded,
                },
            });
            // One row per tool call, right after the turn that asked for it —
            // approvals are the only interactive thing in the conversation, so
            // they get their own row rather than being buried in the assistant
            // bubble.
            for (call_ix, call) in msg.tool_calls.iter().enumerate() {
                items.push(MessageItem::Tool {
                    message_ix,
                    call_ix,
                    state: call.clone(),
                    cwd: cwd.map(str::to_string),
                });
            }
        }
        if !self.stream_reasoning.is_empty() || !self.stream_buf.is_empty() {
            items.push(MessageItem::Streaming {
                reasoning: self.stream_reasoning.clone(),
                content: self.stream_buf.clone(),
                streaming: self.stream.is_some(),
            });
        }
        items
    }

    /// Fetch one provider's model list (blocking HTTP offloaded to a
    /// background thread), sorted + deduped into `models_by_provider`.
    fn fetch_models(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let ai_cfg = config::snapshot().ai;
        let Some(entry) = ai_cfg.providers.iter().find(|p| p.id == provider_id) else {
            return;
        };
        let Some(provider) = ai::resolve_entry(cx, entry) else {
            // Endpoint or key not usable (yet) — not an error for the
            // panel; the not-configured hint covers it.
            return;
        };
        self.fetching_count += 1;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || provider.list_models()).await;
            let _ = this.update(cx, |panel, cx| {
                panel.fetching_count = panel.fetching_count.saturating_sub(1);
                match result {
                    Ok(mut models) => {
                        models.sort();
                        models.dedup();
                        panel.models_by_provider.insert(provider_id, models);
                    }
                    Err(err) => {
                        panel.error = Some(format!("{}: {err}", t!("ai_panel.fetch_failed")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for AiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let ai_cfg = config::snapshot().ai;
        let busy = self.stream.is_some();

        // --- Combined provider·model items ---
        //
        // One flat list: every configured provider's fetched models as
        // `"{provider} · {model}"` rows; a provider whose list hasn't
        // arrived yet contributes its currently-selected model (if any) so
        // the picker can still show the active selection.
        let active_id = ai_cfg
            .active_provider()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let mut items: Vec<ComboItem> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        for entry in &ai_cfg.providers {
            let prefix = picker_prefix(entry);
            let models = self.models_by_provider.get(&entry.id);
            let mut pushed = false;
            if let Some(models) = models {
                for model in models {
                    labels.push(format!("{}: {}", prefix, model));
                    items.push(ComboItem {
                        provider_id: entry.id.clone(),
                        model: model.clone(),
                    });
                    pushed = true;
                }
            }
            // Keep the selection visible even before/without a fetch.
            if !pushed && entry.id == active_id && !ai_cfg.model.trim().is_empty() {
                labels.push(format!("{}: {}", prefix, ai_cfg.model));
                items.push(ComboItem {
                    provider_id: entry.id.clone(),
                    model: ai_cfg.model.clone(),
                });
            }
        }
        let selected_idx = items
            .iter()
            .position(|it| it.provider_id == active_id && it.model == ai_cfg.model);
        let mut combo = Dropdown::new("ai-panel-combo")
            .is_open(self.combo_open)
            // The picker sits at the window bottom — open upward so the
            // menu isn't clipped by the viewport edge — and uses the
            // compact trigger to match the input area's density.
            .open_upward()
            .compact()
            .placeholder(if self.fetching_count > 0 {
                t!("ai_panel.fetching").to_string()
            } else {
                t!("ai_panel.model").to_string()
            });
        if let Some(idx) = selected_idx {
            combo = combo.selected(idx);
        }
        if let Some(search) = self.combo_search.clone() {
            combo = combo.searchable(search);
        }
        for label in &labels {
            combo = combo.item(label.clone());
        }
        let combo = combo
            .on_toggle({
                let h = handle.clone();
                move |_w, cx| {
                    h.update(cx, |panel, cx| {
                        panel.combo_open = !panel.combo_open;
                        cx.notify();
                    });
                }
            })
            .on_change({
                let h = handle.clone();
                let items = items.clone();
                let search = self.combo_search.clone();
                move |idx, w, cx| {
                    if let Some(item) = items.get(idx) {
                        let provider_id = item.provider_id.clone();
                        let model = item.model.clone();
                        let _ = config::update(|cfg| {
                            cfg.ai.active = provider_id;
                            cfg.ai.model = model;
                        });
                    }
                    // The dropdown filters from this box but never clears
                    // it — reset so the next open starts unfiltered.
                    if let Some(search) = &search {
                        search.update(cx, |state, cx| state.set_value("", w, cx));
                    }
                    h.update(cx, |panel, cx| {
                        panel.combo_open = false;
                        cx.notify();
                    });
                }
            });

        // --- Conversation ---
        //
        // Per-message blocks, closely following Zed's agent panel layout:
        // user turns render as boxed bubbles, assistant turns as unlabeled
        // content with a thinking section (header + left-rail indent) above
        // the answer. Each block is its own selectable Markdown `TextView` —
        // like Zed, selection is per message — and unchanged text skips
        // re-parsing. A newly created TextView parses immediately (no
        // debounce on first paint), so committing a streamed turn has no
        // visible gap. The live streaming tail stays as plain elements:
        // the TextView coalesces changes behind a 200ms debounce, which
        // would hide tokens until the stream pauses.
        // Compact Markdown typography. The bare default would render body
        // text at 16px with the stock heading scale (h1 = 2rem ≈ 32px) and
        // 1rem paragraph gaps — a document, not a chat sidebar. Ambient
        // `text_size` on each row's wrapper pins the body size; the style
        // handles what ambient size can't reach.
        let md_style = TextViewStyle {
            highlight_theme: cx.theme().highlight_theme.clone(),
            is_dark: true,
            paragraph_gap: rems(0.6),
            heading_base_font_size: px(CONVERSATION_TEXT_SIZE),
            heading_font_size: Some(Arc::new(|level: u8, base: Pixels| match level {
                1 => base * 1.3,
                2 => base * 1.2,
                3 => base * 1.12,
                4 => base * 1.06,
                _ => base,
            })),
            code_block: StyleRefinement::default().p_2().text_size(px(12.)),
        };
        let has_conversation = !self.messages.is_empty()
            || !self.stream_reasoning.is_empty()
            || !self.stream_buf.is_empty();
        let not_configured = !ai::configured(cx);

        // --- Virtualized conversation ---
        //
        // Only the rows intersecting the viewport (plus overdraw) are
        // rendered and laid out each frame, so scrolling cost no longer
        // grows with the conversation. The row count is synced here, in
        // render: `splice` rather than `reset`, because `reset` drops the
        // logical scroll position (the view would jump back to the top).
        let item_count = self.list_row_count();
        let known = self.list_state.item_count();
        if known < item_count {
            self.list_state.splice(known..known, item_count - known);
        } else if known > item_count {
            self.list_state.splice(item_count..known, 0);
        }
        // The list's render closure cannot borrow the view, so rows are
        // built from a per-frame snapshot plus a weak handle (used by the
        // thinking disclosures).
        // The terminal's last-reported cwd, for the execute card's
        // "execution path" line. Read once per frame — the handle is weak, so
        // a closed pane simply yields `None`.
        let terminal_cwd = self
            .session
            .terminal
            .upgrade()
            .and_then(|view| view.read(cx).cwd().map(str::to_string));
        let items: Rc<Vec<MessageItem>> = Rc::new(self.message_items(terminal_cwd.as_deref()));
        let item_entity = cx.entity().downgrade();
        let item_style = md_style.clone();
        let list_el = list(self.list_state.clone(), move |ix, window, cx| {
            render_list_item(ix, items.as_slice(), &item_entity, &item_style, window, cx)
        });

        // --- Bottom controls ---
        // The input area sits directly on the panel background (no box
        // chrome, no padding) and uses a smaller text size than the default
        // input — `Size::XSmall` = text_xs at zero padding. The library
        // forces its own root to `height: auto` for multi-line inputs; our
        // refinements run after that, so `h_full` (empty state only) makes
        // the editor take the whole input area instead of hugging its rows.
        let input_el: AnyElement = match &self.input {
            Some(state) => Input::new(state)
                .appearance(false)
                .bordered(false)
                .with_size(Size::XSmall)
                .when(!has_conversation, |el| el.h_full())
                .into_any_element(),
            None => div().into_any_element(),
        };
        // The library's built-in placeholder never wraps in multi-line
        // inputs, so the panel overlays its own wrapping placeholder while
        // the box is empty and unfocused — while focused it hides so the
        // caret gets clean space. The overlay has no hitbox — clicks fall
        // through to the input underneath.
        let input_empty = self
            .input
            .as_ref()
            .map(|state| state.read(cx).value().trim().is_empty())
            .unwrap_or(true);
        let show_placeholder = !self.input_focused && input_empty;
        let input_area = div()
            .relative()
            // With no conversation the input area *is* the panel: it takes
            // every pixel the controls row doesn't need, and the editor
            // element (`height: 100%` + `flex_grow` in the library) fills
            // it. Once there are messages it goes back to its natural
            // height — 6 rows, auto-growing to 12.
            .when(!has_conversation, |el| el.flex_1().min_h_0())
            .child(input_el)
            .when(show_placeholder, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(t!("ai_panel.placeholder").to_string()),
                )
            });
        // Send / Stop. One slot, icon-only: while a reply streams the action
        // is cancel (the only way to stop — Esc isn't wired), otherwise
        // send, which stays disabled until there is something to send. Enter
        // in the input sends; Cmd/Ctrl+Enter inserts a newline.
        let action_btn: AnyElement = if busy {
            // Stop reads as a danger action: the theme's red (`term_red`,
            // the same accent the danger alerts use) as a translucent fill +
            // tinted border, mirroring the accent chips used by the list
            // rows (`(color << 8) | alpha`), but with enough alpha to read
            // as a red button rather than a quiet badge.
            let red = term_red();
            let h = handle.clone();
            Button::new("ai-panel-cancel")
                .icon("icons/square.svg")
                .icon_color(red)
                .bg((red << 8) | 0x33)
                .bg_hover((red << 8) | 0x4d)
                .bg_selected((red << 8) | 0x66)
                .border_color((red << 8) | 0x99)
                .size_6()
                .flex_shrink_0()
                .centered(true)
                .on_click(move |_e, _w, cx| {
                    h.update(cx, |panel, _cx| panel.stop());
                })
                .into_any_element()
        } else {
            let h = handle.clone();
            Button::new("ai-panel-send")
                .icon("icons/send-horizontal.svg")
                .primary()
                .size_6()
                .flex_shrink_0()
                .centered(true)
                .disabled(input_empty)
                .on_click(move |_e, _w, cx| {
                    h.update(cx, |panel, cx| panel.send(cx));
                })
                .into_any_element()
        };

        let mut root = div().size_full().flex().flex_col().min_h_0();
        if has_conversation {
            // --- Conversation viewport ---
            //
            // Structure mirrors gpui-component's `Scrollable`: the
            // scrollbar layer overlays the (non-scrolling) viewport wrapper
            // as a sibling of the list. Attaching it to the scrolling
            // element itself would make it part of the scrolled content —
            // it would scroll away with the content instead of tracking it.
            root = root.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(list_el.size_full())
                    .vertical_scrollbar(&self.list_state),
            );
        }
        root = root
            .when_some(self.error.clone(), |el, err| {
                el.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(input_border_error()))
                        .child(err),
                )
            })
            // --- Composer ---
            //
            // Zed's agent-panel pattern: the editor area sits on the toolbar
            // surface with the controls row underneath. While the conversation
            // is empty the composer (and with it the input area) fills the
            // panel; once there are messages it shrinks to its 6-row default.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .bg(rgb(bg_tab_bar()))
                    .border_t_1()
                    .border_color(rgb(border()))
                    .when(!has_conversation, |el| el.flex_1().min_h_0())
                    .when(!has_conversation && not_configured, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(t!("ai_panel.not_configured").to_string()),
                        )
                    })
                    .child(input_area)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .items_center()
                            .child(div().flex_1().min_w_0().child(combo))
                            .child(action_btn),
                    ),
            );
        root
    }
}

/// One row of the virtualized conversation list. Snapshotted from the
/// panel's messages once per frame: `List`'s render closure gets no access
/// to the view, so rows must be self-contained.
enum MessageItem {
    User {
        content: String,
    },
    Assistant {
        /// Index into `AiPanel::messages` — the thinking toggle needs the
        /// message, not the list row (they differ once cards exist).
        message_ix: usize,
        reasoning: String,
        content: String,
        reasoning_expanded: bool,
    },
    /// One tool call awaiting the user's approval — or showing what they
    /// decided. Carries everything the card needs so the list's render
    /// closure (which cannot borrow the panel) stays self-contained.
    Tool {
        /// Index of the assistant turn that requested it (for the resolve
        /// callback).
        message_ix: usize,
        /// Index within that turn's calls.
        call_ix: usize,
        state: ToolCallState,
        /// Directory the command would run in, when the shell reports one.
        cwd: Option<String>,
    },
    Streaming {
        reasoning: String,
        content: String,
        streaming: bool,
    },
}

/// The part of `after` that appeared since `before`, used to answer an
/// `execute` call with the command's own output instead of the whole screen.
///
/// Terminals scroll, so the interesting text is the suffix the two dumps
/// don't share; the common prefix is computed on bytes and then walked back
/// to a char boundary so the slice can't panic on multi-byte output. The
/// result is capped at [`MAX_TOOL_RESULT_BYTES`] (keeping the tail, which is
/// where the interesting part of a long output lives) so a chatty command
/// can't blow up the conversation's context.
fn new_since(before: &str, after: &str) -> String {
    let before = before.as_bytes();
    let after_bytes = after.as_bytes();
    let mut ix = 0;
    while ix < before.len() && ix < after_bytes.len() && before[ix] == after_bytes[ix] {
        ix += 1;
    }
    while ix > 0 && !after.is_char_boundary(ix) {
        ix -= 1;
    }
    cap_tool_result(after[ix..].trim_matches('\n'))
}

/// Cap a tool result at [`MAX_TOOL_RESULT_BYTES`], keeping the tail (where a
/// command's errors and summary usually are) and noting how much was dropped.
/// The cut is walked to a char boundary so the slice can't panic on
/// multi-byte output.
fn cap_tool_result(text: &str) -> String {
    if text.len() <= MAX_TOOL_RESULT_BYTES {
        return text.to_string();
    }
    let mut cut = text.len() - MAX_TOOL_RESULT_BYTES;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("(earlier output omitted — {} bytes)\n{}", cut, &text[cut..])
}

/// Prefix a captured command with `cd <dir> &&` when the shell has reported
/// its directory.
///
/// An out-of-band command starts in the login shell's directory (usually
/// `$HOME`), not where the user's terminal is — so without this a `ls` would
/// list the wrong directory. A visible run needs no prefix: it is typed into
/// the very shell the user is looking at.
fn with_cwd(command: &str, cwd: Option<&str>) -> String {
    match cwd {
        Some(dir) if !dir.is_empty() => format!("cd {} && {}", quote_shell(dir), command),
        _ => command.to_string(),
    }
}

/// Single-quote `text` for a POSIX shell, closing and reopening around
/// embedded quotes (`'` → `'\''`). Enough for the directory names
/// interpolated here, and the same quoting a user would expect from a shell.
fn quote_shell(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Fresh id for a conversation's session header. Only needs to be unique
/// among a gateway's clients and stable for the conversation's lifetime, so
/// process id + start time in hex is plenty — no RNG or uuid dependency.
fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("crabport-{:x}-{:x}", std::process::id(), nanos)
}

/// Label prefix for a provider in the combined model picker. OpenCode's
/// built-in gateways are one account serving two endpoints with different
/// model lists each, so they are labeled by endpoint (`Zen` / `Go`) rather
/// than by their full provider names; everything else uses its name,
/// falling back to the endpoint URL.
fn picker_prefix(entry: &config::AiProviderConfig) -> String {
    match entry.id.as_str() {
        config::OPENCODE_ZEN_PROVIDER_ID => "Zen".to_string(),
        config::OPENCODE_GO_PROVIDER_ID => "Go".to_string(),
        _ if entry.name.trim().is_empty() => entry.base_url.clone(),
        _ => entry.name.clone(),
    }
}

/// Render one row of the conversation list. `List` only calls this for rows
/// intersecting the viewport (plus overdraw), so a long conversation costs
/// the same as a short one; off-screen rows that were measured before are
/// simply re-emitted at their cached height.
#[allow(clippy::too_many_arguments)]
fn render_list_item(
    ix: usize,
    items: &[MessageItem],
    entity: &WeakEntity<AiPanel>,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    // Row padding lives here (the list itself is flush), so the scrollbar
    // hugs the panel edge.
    let row = div().w_full().px_2().py_1p5();
    match items.get(ix) {
        Some(MessageItem::User { content }) => row
            .child(user_bubble(
                content.clone(),
                format!("ai-user-{ix}"),
                md_style,
                window,
                cx,
            ))
            .into_any_element(),
        Some(MessageItem::Assistant {
            message_ix,
            reasoning,
            content,
            reasoning_expanded,
        }) => row
            .child(assistant_block(
                *message_ix,
                reasoning.clone(),
                content.clone(),
                *reasoning_expanded,
                entity,
                md_style,
                window,
                cx,
            ))
            .into_any_element(),
        Some(MessageItem::Tool {
            message_ix,
            call_ix,
            state,
            cwd,
        }) => row
            .child(tool_card(
                *message_ix,
                *call_ix,
                state.clone(),
                cwd.as_deref(),
                entity,
                md_style,
                window,
                cx,
            ))
            .into_any_element(),
        Some(MessageItem::Streaming {
            reasoning,
            content,
            streaming,
        }) => row
            .child(streaming_block(
                reasoning.clone(),
                content.clone(),
                *streaming,
            ))
            .into_any_element(),
        None => row.into_any_element(),
    }
}

/// The authorization card for one tool call.
///
/// Nothing runs without the user's click: the card spells out what the call
/// would do — the lines it would read and why, or the exact command, the
/// directory it runs in and what the model expects — then offers
/// deny/approve. Once decided it shows the outcome instead of the buttons, so
/// the transcript keeps a record of both the request and what came of it.
#[allow(clippy::too_many_arguments)]
fn tool_card(
    message_ix: usize,
    call_ix: usize,
    state: ToolCallState,
    cwd: Option<&str>,
    entity: &WeakEntity<AiPanel>,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let kind = ToolKind::of(&state.call.name);
    // Both execution tools share one card — the user is approving "run this
    // command", and whether it runs out of band or in their own terminal is
    // an implementation detail not worth telling apart at a glance. Reads
    // keep their own blue so they don't look like something that will run.
    let (title, accent) = match kind {
        ToolKind::Read => (t!("ai_panel.tool_read_title").to_string(), term_blue()),
        ToolKind::ExecCapture | ToolKind::Run => {
            (t!("ai_panel.tool_exec_title").to_string(), term_yellow())
        }
        ToolKind::Unknown => (
            t!("ai_panel.tool_unknown", name = state.call.name.as_str()).to_string(),
            text_muted(),
        ),
    };
    let is_execute = matches!(kind, ToolKind::ExecCapture | ToolKind::Run);

    let mut card = div()
        .w_full()
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(rgba((accent << 8) | 0x66))
        .bg(rgba((accent << 8) | 0x14))
        .p_2()
        .flex()
        .flex_col()
        .gap_1()
        .text_size(px(CONVERSATION_TEXT_SIZE))
        // Header: which terminal tool is asking, and — once the user has
        // decided — a check or a cross in the corner. The decision is not
        // spelled out in words: the card already carries the outcome below.
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(
                            svg()
                                .path("icons/sparkles.svg")
                                .size(px(12.0))
                                .text_color(rgb(accent)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(accent))
                                .child(title),
                        ),
                )
                .when_some(
                    state_icon(&state.decision, state.running),
                    |el, (path, color)| {
                        el.child(svg().path(path).size(px(14.0)).text_color(rgb(color)))
                    },
                ),
        );

    // Body: the fields the user asked for, per tool.
    if is_execute {
        let command = state.arg_str("command").unwrap_or("");
        let expected = state.arg_str("expected").unwrap_or("");
        card = card
            .child(tool_card_field(
                t!("ai_panel.tool_field_path").as_ref(),
                cwd.map(str::to_string)
                    .unwrap_or_else(|| t!("ai_panel.tool_field_path_unknown").to_string()),
                false,
            ))
            .child(tool_card_field(
                t!("ai_panel.tool_field_command").as_ref(),
                command.to_string(),
                true,
            ));
        if !expected.is_empty() {
            card = card.child(tool_card_field(
                t!("ai_panel.tool_field_expected").as_ref(),
                expected.to_string(),
                false,
            ));
        }
    } else if kind == ToolKind::Read {
        let lines = state.arg_u64("lines").unwrap_or(200);
        let purpose = state.arg_str("purpose").unwrap_or("");
        card = card.child(tool_card_field(
            t!("ai_panel.tool_field_read").as_ref(),
            t!("ai_panel.tool_field_read_lines", lines = lines).to_string(),
            false,
        ));
        if !purpose.is_empty() {
            card = card.child(tool_card_field(
                t!("ai_panel.tool_field_purpose").as_ref(),
                purpose.to_string(),
                false,
            ));
        }
    }

    // Malformed arguments: say so instead of offering a decision that would
    // do nothing.
    if state.args.is_none() {
        card = card.child(
            div()
                .text_xs()
                .text_color(rgb(input_border_error()))
                .child(t!("ai_panel.tool_bad_args").to_string()),
        );
    }

    // How a captured command ended, in the same muted register as the field
    // labels: the green check in the corner already says "it ran", so only a
    // non-zero status (and a timeout) is worth spelling out.
    if state.timed_out {
        card = card.child(
            div().text_xs().text_color(rgb(term_yellow())).child(
                t!(
                    "ai_panel.tool_capture_timed_out",
                    secs = EXEC_CAPTURE_TIMEOUT.as_secs()
                )
                .to_string(),
            ),
        );
    } else if let Some(code) = state.exit_code.filter(|code| *code != 0) {
        card = card.child(
            div()
                .text_xs()
                .text_color(rgb(term_red()))
                .child(t!("ai_panel.tool_exit_code", code = code.to_string()).to_string()),
        );
    }

    match state.decision {
        ToolDecision::Pending => {
            let h_allow = entity.clone();
            let h_deny = entity.clone();
            card = card.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new(ElementId::Name(
                            format!("ai-tool-deny-{message_ix}-{call_ix}").into(),
                        ))
                        .child(t!("ai_panel.tool_deny").to_string())
                        .w_auto()
                        .px_2()
                        .centered(true)
                        .on_click(move |_e, _w, cx| {
                            let _ = h_deny.update(cx, |panel, cx| {
                                panel.resolve_tool(message_ix, call_ix, false, cx)
                            });
                        }),
                    )
                    .child(
                        Button::new(ElementId::Name(
                            format!("ai-tool-allow-{message_ix}-{call_ix}").into(),
                        ))
                        .child(t!("ai_panel.tool_allow").to_string())
                        .primary()
                        .w_auto()
                        .px_2()
                        .centered(true)
                        .on_click(move |_e, _w, cx| {
                            let _ = h_allow.update(cx, |panel, cx| {
                                panel.resolve_tool(message_ix, call_ix, true, cx)
                            });
                        }),
                    ),
            )
        }
        ToolDecision::Allowed | ToolDecision::Denied | ToolDecision::Unavailable => {
            // The header icon already says what the user decided (and whether
            // a command is still running); here we only show what came back.
            // While an execution is in flight the result is not in yet, so the
            // card says so rather than looking empty.
            let result = state
                .result
                .clone()
                .unwrap_or_else(|| t!("ai_panel.tool_exec_running").to_string());
            let result_view: AnyElement = if result.trim().is_empty() {
                div().into_any_element()
            } else {
                let border_color = match state.decision {
                    ToolDecision::Allowed => rgb(border()),
                    _ => rgba((term_red() << 8) | 0x66),
                };
                div()
                    .w_full()
                    .text_size(px(CONVERSATION_TEXT_SIZE))
                    .border_l_1()
                    .border_color(border_color)
                    .pl_2()
                    .child(
                        TextView::markdown(
                            ElementId::Name(
                                format!("ai-tool-result-{message_ix}-{call_ix}").into(),
                            ),
                            {
                                let fence = code_fence(&result);
                                format!("{fence}\n{}\n{fence}", result.trim())
                            },
                            window,
                            cx,
                        )
                        .selectable(true)
                        .h_auto()
                        .style(md_style.clone()),
                    )
                    .into_any_element()
            };
            card = card.child(result_view);
        }
    }

    card.into_any_element()
}

/// The corner glyph for a card: a check once the call ran, a cross when the
/// user refused it, a loader while a command is still producing output, and
/// a muted alert when the connection can't run the call at all.
/// `None` while it is still waiting for a click.
fn state_icon(decision: &ToolDecision, running: bool) -> Option<(&'static str, u32)> {
    if running {
        return Some(("icons/loader-circle.svg", text_muted()));
    }
    match decision {
        ToolDecision::Pending => None,
        ToolDecision::Allowed => Some(("icons/check.svg", term_green())),
        ToolDecision::Denied => Some(("icons/x.svg", term_red())),
        ToolDecision::Unavailable => Some(("icons/circle-alert.svg", text_muted())),
    }
}

/// Fence long enough to survive the output itself containing backticks: a
/// fence closes only on a run at least as long as it is, so one backtick
/// longer than the longest run inside the text is always safe. Without this
/// a command that prints ``` would cut the card's code block short.
fn code_fence(text: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

/// One labelled field of a tool card: a muted caption over the value, the
/// value in the mono face when it is something that will be typed into a
/// shell.
fn tool_card_field(label: &str, value: String, monospace: bool) -> AnyElement {
    let mut value_el = div()
        .w_full()
        .whitespace_normal()
        .text_color(rgb(text_primary()))
        .child(value);
    if monospace {
        // The same family the terminal itself renders with, so a command on a
        // card looks like the command about to be typed into the shell.
        value_el = value_el.font_family(TerminalView::mono_font_family());
    }
    div()
        .flex()
        .flex_col()
        .w_full()
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(label.to_string()),
        )
        .child(value_el)
        .into_any_element()
}

/// Zed-style assistant turn: the thinking section (when present) sits above
/// the answer, both rendered as their own selectable Markdown views.
#[allow(clippy::too_many_arguments)]
fn assistant_block(
    ix: usize,
    reasoning: String,
    content: String,
    expanded: bool,
    entity: &WeakEntity<AiPanel>,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let mut col = div().flex().flex_col().gap_2().w_full();
    if !reasoning.is_empty() {
        col = col.child(thinking_section(
            ix, reasoning, expanded, entity, md_style, window, cx,
        ));
    }
    if !content.is_empty() {
        col = col.child(
            div().w_full().text_size(px(CONVERSATION_TEXT_SIZE)).child(
                TextView::markdown(
                    ElementId::Name(format!("ai-assistant-{ix}").into()),
                    content,
                    window,
                    cx,
                )
                .selectable(true)
                .h_auto()
                .style(md_style.clone()),
            ),
        );
    }
    col.into_any_element()
}

/// Thinking disclosure, matching Zed's thinking block: the whole header row
/// is the click target, and the trailing chevron hints at the state
/// (pointing down while collapsed, up while expanded; it brightens on
/// hover). The body renders its own selectable Markdown view.
#[allow(clippy::too_many_arguments)]
fn thinking_section(
    ix: usize,
    reasoning: String,
    expanded: bool,
    entity: &WeakEntity<AiPanel>,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let group: SharedString = format!("ai-think-group-{ix}").into();
    let header = div()
        .id(ElementId::Name(format!("ai-think-header-{ix}").into()))
        .group(group.clone())
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .cursor_pointer()
        .on_click({
            let entity = entity.clone();
            move |_event, _window, cx| {
                let _ = entity.update(cx, |panel, cx| panel.toggle_thinking(ix, cx));
            }
        })
        .child(thinking_header_label())
        .child(
            div()
                .id(ElementId::Name(format!("ai-think-chevron-{ix}").into()))
                .opacity(0.5)
                .group_hover(group, |style| style.opacity(1.0))
                .child(
                    svg()
                        .path("icons/chevron-down.svg")
                        .size(px(12.0))
                        .text_color(rgb(text_muted()))
                        .with_transformation(Transformation::rotate(radians(if expanded {
                            std::f32::consts::PI
                        } else {
                            0.0
                        }))),
                ),
        );
    let mut col = div().flex().flex_col().w_full().child(header);
    if expanded {
        col = col.child(thinking_body(
            reasoning,
            format!("ai-think-{ix}"),
            md_style,
            window,
            cx,
        ));
    }
    col.into_any_element()
}

/// Zed-style user bubble: a bordered box on the raised surface so user
/// turns read as distinct from the assistant's unlabeled content. Own
/// selectable Markdown view.
fn user_bubble(
    text: String,
    id: String,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    div()
        .w_full()
        .child(
            div()
                .w_full()
                .px_3()
                .py_2()
                .rounded(RADIUS_MD)
                .bg(rgb(surface_hover()))
                .border_1()
                .border_color(rgb(border()))
                .text_size(px(CONVERSATION_TEXT_SIZE))
                .child(
                    TextView::markdown(ElementId::Name(id.into()), text, window, cx)
                        .selectable(true)
                        .h_auto()
                        .style(md_style.clone()),
                ),
        )
        .into_any_element()
}

/// Thinking body: the reasoning Markdown view indented behind a left
/// rail, rendered smaller/muted via ambient style inheritance.
fn thinking_body(
    text: String,
    id: String,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    div()
        .mt_1()
        .ml_1p5()
        .pl_3()
        .border_l_1()
        .border_color(rgb(border()))
        .text_xs()
        .text_color(rgb(text_muted()))
        .child(
            TextView::markdown(ElementId::Name(id.into()), text, window, cx)
                .selectable(true)
                .h_auto()
                .style(md_style.clone()),
        )
        .into_any_element()
}

/// Icon + label shared by the committed disclosure header and the live
/// streaming header.
fn thinking_header_label() -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(
            svg()
                .path("icons/sparkles.svg")
                .size(px(12.0))
                .text_color(rgb(text_muted())),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("ai_panel.thinking").to_string()),
        )
        .into_any_element()
}

/// Live streaming tail: plain elements (instant per-token updates; the
/// Markdown views coalesce changes behind their 200ms debounce), styled to
/// match the committed blocks. Carets mark the growing end while the
/// stream is active.
fn streaming_block(reasoning: String, content: String, streaming: bool) -> AnyElement {
    let mut col = div().flex().flex_col().gap_2().w_full();
    if !reasoning.is_empty() {
        let text = if streaming && content.is_empty() {
            format!("{reasoning}▍")
        } else {
            reasoning
        };
        col = col.child(
            div()
                .flex()
                .flex_col()
                .child(thinking_header_label())
                .child(
                    div()
                        .mt_1()
                        .ml_1p5()
                        .pl_3()
                        .border_l_1()
                        .border_color(rgb(border()))
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(text),
                ),
        );
    }
    if !content.is_empty() {
        let text = if streaming {
            format!("{content}▍")
        } else {
            content
        };
        col = col.child(
            div()
                .text_size(px(CONVERSATION_TEXT_SIZE))
                .text_color(rgb(text_primary()))
                .child(text),
        );
    }
    col.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{MAX_TOOL_RESULT_BYTES, ToolKind, cap_tool_result, quote_shell, with_cwd};

    #[test]
    fn tool_kind_maps_wire_names() {
        assert_eq!(ToolKind::of("terminal_read"), ToolKind::Read);
        assert_eq!(ToolKind::of("terminal_exec"), ToolKind::ExecCapture);
        assert_eq!(ToolKind::of("terminal_run"), ToolKind::Run);
        assert_eq!(ToolKind::of("terminal_execute"), ToolKind::Unknown);
    }

    /// A captured command starts where the user's terminal is, not in the
    /// login shell's default directory — and paths with quotes/quotes-spaces
    /// must survive the interpolation.
    #[test]
    fn with_cwd_prefixes_quoted_directory() {
        assert_eq!(with_cwd("ls", None), "ls");
        assert_eq!(with_cwd("ls", Some("")), "ls");
        assert_eq!(with_cwd("ls", Some("/var/log")), "cd '/var/log' && ls");
        assert_eq!(
            with_cwd("cat file", Some("/tmp/it's here")),
            "cd '/tmp/it'\\''s here' && cat file"
        );
    }

    #[test]
    fn quote_shell_escapes_single_quotes() {
        assert_eq!(quote_shell("plain"), "'plain'");
        assert_eq!(quote_shell("a'b"), "'a'\\''b'");
    }

    #[test]
    fn cap_tool_result_keeps_the_tail_on_a_char_boundary() {
        let short = "hello";
        assert_eq!(cap_tool_result(short), short);

        // Multi-byte output longer than the cap: the cut must not split a
        // char, and it must land near the end.
        let long = "⇒".repeat(MAX_TOOL_RESULT_BYTES);
        let capped = cap_tool_result(&long);
        assert!(capped.len() < long.len());
        assert!(capped.starts_with("(earlier output omitted"));
        assert!(capped.ends_with("⇒"));
    }
}
