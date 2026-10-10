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
//! The agent itself — the system prompt, the tool specs and every tool
//! implementation (captured/visible execution, local file reads, HTTP fetch,
//! tunnel lifecycle, SFTP), plus the compaction rules — lives in
//! `crabport-agent`. This panel adds the streaming pump, the approval cards,
//! and [`UiAgentSession`]: the cx-free adapter that hands the agent a snapshot
//! of this pane's terminal (the crate cannot depend on GPUI, so the UI
//! implements its `AgentSession` trait).
//!
//! Model lists: fetched per provider from each endpoint's `/models` API
//! (via [`crabport_ai::OpenAiProvider::list_models`], offloaded to a
//! background thread with `smol::unblock`), auto-fetched once per endpoint
//! (editing an endpoint's URL produces a new key and re-fetches).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_broadcast::Receiver;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Sizable as _, Size};
use rust_i18n::t;

use crabport_agent::limits::EXEC_CAPTURE_TIMEOUT;
use crabport_agent::{
    Agent, AgentSession, COMPACTED_HEADER, COMPACTION_PROMPT, CompactCall, CompactTurn, Executed,
    RegistryTunnel, ToolKind, ToolOutcome, ToolTable, TurnRole, agent_tools, compaction_cut,
    compaction_transcript, system_prompt,
};
use crabport_ai::{
    AiError, ChatMessage, ChatRequest, ChatResponse, ChatStream, StreamEvent, ToolCall,
};
use crabport_core::config::{self, ToolPermission};
use crabport_sftp::FileEntry;
use crabport_ssh::TunnelManager;
use crabport_terminal::terminal::{BackendEvent, ExecCallback, ExecCancel};
use gpui_component::ActiveTheme as _;
use gpui_component::text::{TextView, TextViewStyle};

use crate::ai;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::motion::RADIUS_MD;
use crate::views::terminal::{AgentTerminalHandles, TerminalView};
use crate::views::tunnels::TunnelRegistry;

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

/// The card's title for one tool.
pub(crate) fn tool_title(name: &str) -> String {
    match name {
        "terminal_read" => t!("ai_panel.tool_read_title").to_string(),
        "terminal_exec" | "terminal_run" => t!("ai_panel.tool_exec_title").to_string(),
        "fetch" => t!("ai_panel.tool_fetch_title").to_string(),
        "read_file" => t!("ai_panel.tool_read_file_title").to_string(),
        "read_directory" => t!("ai_panel.tool_read_dir_title").to_string(),
        "tunnel_create" => t!("ai_panel.tool_tunnel_create_title").to_string(),
        "tunnel_open" => t!("ai_panel.tool_tunnel_open_title").to_string(),
        "tunnel_close" => t!("ai_panel.tool_tunnel_close_title").to_string(),
        "tunnel_delete" => t!("ai_panel.tool_tunnel_delete_title").to_string(),
        "tunnel_list" => t!("ai_panel.tool_tunnel_list_title").to_string(),
        "sftp_list" => t!("ai_panel.tool_sftp_list_title").to_string(),
        "sftp_download" => t!("ai_panel.tool_sftp_download_title").to_string(),
        "sftp_upload" => t!("ai_panel.tool_sftp_upload_title").to_string(),
        other => t!("ai_panel.tool_unknown", name = other).to_string(),
    }
}

/// The card's accent color per tool: blue for reads, yellow for anything
/// that runs a command, hits the network or moves files, magenta for tunnel
/// lifecycle, muted for the unknown.
fn tool_accent(name: &str) -> u32 {
    match ToolKind::of(name) {
        ToolKind::TerminalRead | ToolKind::LocalFs => term_blue(),
        ToolKind::ExecCapture | ToolKind::Run | ToolKind::Fetch => term_yellow(),
        ToolKind::Tunnel => term_magenta(),
        ToolKind::Sftp => match name {
            "sftp_list" => term_blue(),
            _ => term_yellow(),
        },
        ToolKind::Unknown => text_muted(),
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
    /// Native table the card renders instead of the code fence, when the
    /// result is an enumerable list (tunnel list, directory listing). The
    /// same cells went to the model as `text` in JSON.
    table: Option<ToolTable>,
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
    /// Token that stops the running execution — the card's stop button
    /// flips it. A fresh token per call, harmless once the call is done.
    cancel: ExecCancel,
    /// Whether the card shows its body (fields, result) or folds to just the
    /// header line, like the thinking section. Ignored while the card needs
    /// attention (pending approval, running) — those always stay expanded.
    expanded: bool,
    /// When execution started; feeds the card's elapsed readout while the
    /// call runs. `None` for a call that never got to run (refused, denied).
    started_at: Option<Instant>,
    /// How long the run took, once it settled — shown on the card when the
    /// wait was nontrivial. `None` for refusals, denials and instant results.
    ran_for: Option<Duration>,
    /// True when the user stopped this call while it was running.
    cancelled: bool,
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
        if self.cancelled {
            let note = match ToolKind::of(&self.call.name) {
                ToolKind::ExecCapture => "[stopped by the user; output so far]",
                _ => "[stopped waiting; the operation may still be running in the background]",
            };
            return format!("{note}\n{body}");
        }
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

/// Context window assumed when the endpoint's `/models` response doesn't
/// advertise one. Most current endpoints are at least this large, and
/// compacting a little early is much better than overflowing the window.
const FALLBACK_CONTEXT_WINDOW: usize = 128 * 1024;

/// Share of the window at which the next request triggers compaction
/// (automatic, before the request is sent).
const COMPACT_AT: f32 = 0.75;

/// Share of the window kept verbatim as the recent tail after compaction —
/// the summary replaces everything older.
const COMPACT_KEEP: f32 = 0.4;

/// Terminal session an AI panel is bound to.
///
/// One panel instance exists per terminal pane (see
/// `CrabportApp::ai_panels`), so every terminal keeps its own conversation —
/// and the agent's terminal tools act on *this* session, never on another
/// tab's. The handle is weak so a closed pane doesn't keep its view alive;
/// tool calls on a dead session report "terminal closed".
#[derive(Clone)]
pub struct AiSession {
    /// The pane's terminal view. Tool calls take a cx-free snapshot of it
    /// (see [`UiAgentSession`]) so the agent can work off the UI thread.
    pub terminal: WeakEntity<TerminalView>,
    /// The app's tunnel registry — lets `tunnel_list` also report the
    /// tunnels the Tunnels page started *borrowing this terminal's*
    /// connection. Read-only here: the agent manages only the tunnels it
    /// created itself.
    pub tunnels: Arc<TunnelRegistry>,
    /// Where registry-tunnel mutations go: the UI thread drains it and calls
    /// the app's own start/stop/delete machinery, so the store and the
    /// Tunnels page stay consistent. A channel rather than the app entity
    /// because the agent's tools call this from background threads, where no
    /// `cx` exists.
    pub registry_commands: async_channel::Sender<RegistryCommand>,
}

/// A Tunnels-page mutation an agent tool asked for.
///
/// The registry itself is context-free, but starting / stopping / deleting a
/// config goes through [`crate::app::CrabportApp`]'s methods, which own the
/// store and notify the Tunnels page; the panel drains these on the UI
/// thread and calls the app there.
#[derive(Clone, Copy, Debug)]
pub enum RegistryCommand {
    /// Start `config_id`, borrowing the terminal pane `tab_id`'s connection.
    Open { config_id: i64, tab_id: u64 },
    /// Stop a running config (owned or borrowed).
    Close(i64),
    /// Delete a config, stopping it first when it runs.
    Delete(i64),
}

/// The UI side of the agent's session boundary: one terminal pane's shared
/// handles plus the panel's tunnel plumbing, as the agent's tools see them.
///
/// `crabport-agent` defines its `AgentSession` trait free of GPUI — that is
/// what lets the tools (command capture, output polling, SFTP waits, tunnel
/// lifecycle) run on background threads while this side only awaits their
/// result — and this implements it over handles the terminal view hands out
/// without an entity read. Two things cannot travel through those handles,
/// because they are panel state rather than terminal state:
///
/// - the agent's *own* [`TunnelManager`], kept on the panel so `tunnel_open`
///   and `tunnel_close` hit the same manager across calls (and so the
///   panel's `Drop` can stop everything it started);
/// - the [`RegistryCommand`] channel, because registry mutations must run on
///   the UI thread (see the enum's doc).
struct UiAgentSession {
    handles: AgentTerminalHandles,
    /// The manager the agent's own tunnels live in. `None` on terminals with
    /// no SSH source, which is what makes `allow_tunnels` false and blocks
    /// the tunnel tools.
    manager: Option<Arc<TunnelManager>>,
    /// The app's tunnel registry, read live (cx-free) so `tunnel_list` and
    /// the `t…` handlers see the current state, not the panel's last render.
    registry: Arc<TunnelRegistry>,
    /// Where registry mutations are sent; the panel's drain task owns the
    /// receiving end.
    commands: async_channel::Sender<RegistryCommand>,
}

impl UiAgentSession {
    fn new(
        handles: AgentTerminalHandles,
        registry: Arc<TunnelRegistry>,
        commands: async_channel::Sender<RegistryCommand>,
        manager: Option<Arc<TunnelManager>>,
    ) -> Self {
        Self {
            handles,
            manager,
            registry,
            commands,
        }
    }
}

impl AgentSession for UiAgentSession {
    fn allow_exec_capture(&self) -> bool {
        self.handles.session.allow_exec_capture()
    }

    fn allow_sftp(&self) -> bool {
        self.handles.session.allow_sftp()
    }

    fn allow_tunnels(&self) -> bool {
        self.manager.is_some()
    }

    fn cwd(&self) -> Option<String> {
        self.handles.cwd.clone()
    }

    fn host_id(&self) -> Option<i64> {
        self.handles.host_id
    }

    fn dump_text(&self, lines: usize) -> String {
        self.handles.session.dump_text(lines)
    }

    fn try_dump_text(&self, lines: usize) -> Option<String> {
        self.handles.session.try_dump_text(lines)
    }

    fn write_raw(&self, data: &[u8]) {
        self.handles.session.write_raw(data);
    }

    fn exec_capture(
        &self,
        command: &str,
        timeout: Duration,
        cancel: ExecCancel,
        done: ExecCallback,
    ) {
        self.handles
            .backend
            .exec_capture(command, timeout, cancel, done);
    }

    fn subscribe_events(&self) -> Option<Receiver<BackendEvent>> {
        Some(self.handles.session.subscribe_backend())
    }

    fn sftp_listing(&self) -> Option<(String, Arc<Vec<FileEntry>>)> {
        let cwd = self.handles.session.sftp_cwd()?;
        let entries = self.handles.session.sftp_entries()?;
        Some((cwd.as_str().to_string(), entries))
    }

    fn sftp_navigate(&self, path: &str) {
        self.handles.session.sftp_navigate(path);
    }

    fn sftp_download(&self, remote_path: &str, local_path: &str) {
        self.handles.session.sftp_download(remote_path, local_path);
    }

    fn sftp_upload(&self, local_path: &str, remote_path: &str) {
        self.handles.session.sftp_upload(local_path, remote_path);
    }

    fn tunnel_manager(&self) -> Option<Arc<TunnelManager>> {
        self.manager.clone()
    }

    fn registry_tunnels(&self) -> Vec<RegistryTunnel> {
        let Some(host_id) = self.handles.host_id else {
            return Vec::new();
        };
        self.registry
            .list()
            .into_iter()
            .filter(|view| view.host_id == host_id)
            .map(|view| RegistryTunnel {
                id: view.id,
                name: view.name,
                kind: view.kind,
                bind_addr: view.bind_addr,
                bind_port: view.bind_port,
                target_host: view.target_host,
                target_port: view.target_port,
                running: view.running,
                borrowed: view.borrowed_tab_id.is_some(),
            })
            .collect()
    }

    fn registry_is_running(&self, config_id: i64) -> bool {
        self.registry.is_running(config_id)
    }

    fn registry_live(&self, config_id: i64) -> Option<(String, u16, String, u16)> {
        let manager = self.registry.manager_for(config_id)?;
        let info = manager.list().into_iter().next()?;
        Some((
            info.bind_addr,
            info.bind_port,
            info.target_host,
            info.target_port,
        ))
    }

    fn registry_open(&self, config_id: i64) {
        let _ = self.commands.try_send(RegistryCommand::Open {
            config_id,
            tab_id: self.handles.pane_id,
        });
    }

    fn registry_close(&self, config_id: i64) {
        let _ = self.commands.try_send(RegistryCommand::Close(config_id));
    }

    fn registry_delete(&self, config_id: i64) {
        let _ = self.commands.try_send(RegistryCommand::Delete(config_id));
    }
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
    /// One summary turn standing in for everything older that was compacted
    /// away (see [`COMPACTED_HEADER`]). Replayed to the model as a user
    /// message so every OpenAI-compatible gateway accepts it.
    Summary,
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

/// One in-flight compaction round.
struct CompactionPlan {
    /// Index of the first message to keep verbatim; `0..cut` become the
    /// summary.
    cut: usize,
    /// The summarization stream — cancelling it (the composer's stop
    /// button) skips compaction and lets the held-up turn run as-is.
    stream: ChatStream,
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
    /// Whether the message input is empty — the send button stays disabled
    /// until it isn't. Cached rather than recomputed per frame: the
    /// library's `value()` copies the whole draft, and this only changes on
    /// [`InputEvent::Change`].
    input_empty: bool,
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
    /// This panel's agent: the tools' own state (the session-local tunnels
    /// it defined, with their never-reused ids). The terminal it acts on is
    /// supplied per call as a cx-free snapshot, so a reconnect or split
    /// cannot leave the agent holding a stale session.
    agent: Agent,
    /// This terminal's tunnel manager — created from the terminal's SSH
    /// source on first use, so every tunnel tool acts only on tunnels this
    /// panel started. `None` on terminals without an SSH source.
    tunnel_manager: Option<Arc<TunnelManager>>,
    /// Context windows learned from the endpoints' `/models` responses,
    /// keyed `"{provider_id}|{model}"`. A missing entry falls back to
    /// [`FALLBACK_CONTEXT_WINDOW`].
    model_windows: HashMap<String, usize>,
    /// Cached token estimate for the next request, shown in the composer's
    /// context chip. Recomputed when the conversation changes — building it
    /// walks the whole history, so it is deliberately not per-frame work.
    context_used: usize,
    /// In-flight compaction round, if any. Non-`None` means "busy": the
    /// held-up turn starts as soon as the summary lands.
    compaction: Option<CompactionPlan>,
    /// Transient status line above the composer (currently only
    /// "compacting"), cleared when the round ends.
    status: Option<String>,
    /// Set when the conversation was rewritten from the top (compaction):
    /// the next render, after re-splicing the row count, scrolls to the end
    /// so the user lands on the new content — scrolling at the moment of the
    /// rewrite would be a no-op, the list still holds the old row count.
    scroll_to_end_pending: bool,
    /// Received-but-not-yet-revealed content / reasoning: the reveal ticker
    /// types these out into `stream_buf` / `stream_reasoning`. Endpoints
    /// deliver tokens in bursts (sometimes one big chunk), and dumping a
    /// burst into the tail all at once is the "it just pops in" feel this
    /// layer avoids.
    stream_pending: String,
    stream_pending_reasoning: String,
    /// A finished turn whose reveal is still draining; `(generation,
    /// result)`. The markdown commit waits for the drain so the final swap
    /// doesn't flash a wall of text.
    finish_pending: Option<(u64, Result<ChatResponse, AiError>)>,
    /// One reveal ticker at a time (same pattern as the elapsed ticker).
    reveal_ticking: bool,
    /// Messages typed while the assistant was busy (streaming, compacting
    /// or awaiting tool decisions), oldest first. Drained one per idle
    /// break — when a reply finishes, the head goes out on its own. The tail
    /// (last entry) is the only one the user can edit; any entry can be
    /// deleted; pressing Enter on an empty input while a message is queued
    /// cancels what is in flight and sends the head immediately.
    queue: Vec<String>,
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
            input_empty: true,
            error: None,
            models_by_provider: HashMap::new(),
            fetch_attempts: HashSet::new(),
            fetching_count: 0,
            combo_open: false,
            session_id: new_session_id(),
            session,
            agent: Agent::new(),
            tunnel_manager: None,
            model_windows: HashMap::new(),
            context_used: 0,
            compaction: None,
            status: None,
            scroll_to_end_pending: false,
            stream_pending: String::new(),
            stream_pending_reasoning: String::new(),
            finish_pending: None,
            reveal_ticking: false,
            queue: Vec::new(),
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
            // The placeholder is the library's own: it is laid out and painted
            // with the text (same origin and font), so it lines up with the
            // caret instead of sitting under it, and it disappears the moment
            // the box holds anything — including IME composition text, which
            // lands in the editor's text while it is being composed. Keep it
            // short: the library never wraps a placeholder.
            InputState::new(window, cx)
                .auto_grow(6, 12)
                .placeholder(t!("ai_panel.placeholder").to_string())
        });
        cx.subscribe(&state, |this, input, event: &InputEvent, cx| {
            match event {
                // Plain Enter sends; Cmd/Ctrl+Enter (`secondary`) keeps
                // the newline the multi-line editor just inserted.
                InputEvent::PressEnter { secondary } => {
                    if !*secondary {
                        this.submit(cx);
                    }
                }
                // Keep the emptiness cache in step: it drives the send
                // button's disabled state, and this is the only place the
                // draft can change (typing, paste, IME composition,
                // programmatic clears).
                InputEvent::Change => {
                    this.input_empty = input.read(cx).value().trim().is_empty();
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
    /// send would leave that newline sitting there. With messages queued,
    /// the bare Enter instead means "stop what is running, send the queue"
    /// (see [`Self::send_queued_cancel_run`]).
    fn submit(&mut self, cx: &mut Context<Self>) {
        let empty = self
            .input
            .as_ref()
            .map(|state| state.read(cx).value().trim().is_empty())
            .unwrap_or(true);
        if empty {
            if self.queue.is_empty() {
                self.pending_clear = true;
                cx.notify();
            } else {
                self.send_queued_cancel_run(cx);
            }
            return;
        }
        // The agent is busy working (streaming, compacting, or waiting on
        // tools): the draft joins the queue instead of interrupting anything,
        // and goes out on its own once the panel is idle again.
        if self.is_busy() {
            let text = self
                .input
                .as_ref()
                .map(|state| state.read(cx).value().trim().to_string())
                .unwrap_or_default();
            if !text.is_empty() {
                self.queue.push(text);
                self.pending_clear = true;
                cx.notify();
            }
            return;
        }
        self.send(cx);
    }

    /// Whether the agent is mid-task: streaming, compacting, or waiting for
    /// a tool's approval or result — the states into which a fresh send
    /// either queues or is refused.
    fn is_busy(&self) -> bool {
        self.stream.is_some()
            || self.finish_pending.is_some()
            || self.compaction.is_some()
            || self.pending_tool_calls() > 0
    }

    fn ensure_combo_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.combo_search.is_some() {
            return;
        }
        self.combo_search = Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("ai_panel.search_models").to_string())
        }));
    }

    /// Send the current input text. No-op while a reply streams or a
    /// compaction round is in flight.
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.stream.is_some() || self.compaction.is_some() {
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
        if ai::resolve_provider_session(cx, &self.session_id).is_none() {
            self.error = Some(t!("ai_panel.not_configured").to_string());
            cx.notify();
            return;
        }
        // Commit the user turn for display (and history) before building
        // the wire request, so it is included exactly once. Sending always
        // lands the view on the newest content — their message and the reply
        // streaming in under it — even if they had scrolled up to read.
        self.commit_and_send(text, cx);

        self.pending_clear = true;
    }

    /// Commit a user turn and start the turn for it — the identical path the
    /// first send and every queued hand-off walk, compaction included.
    /// A send always lands the view on the newest content (the user's own
    /// message), even if they had scrolled up to read.
    fn commit_and_send(&mut self, text: String, cx: &mut Context<Self>) {
        self.messages.push(DisplayMessage {
            role: DisplayRole::User,
            content: text,
            reasoning: String::new(),
            reasoning_expanded: false,
            tool_calls: Vec::new(),
        });
        // Deferred to render (see `settle_tool`): the new row's height is
        // measured next layout; scrolling now would land just short.
        self.scroll_to_end_pending = true;
        // Compacts first when the request would be too large, then sends.
        self.continue_after_tools(cx);
    }

    /// Send one queued message when the panel is idle again — the normal
    /// hand-off after a reply (or a turn's tool chain) completes. One at a
    /// time: the next goes out when the reply to this one finishes.
    fn send_queued(&mut self, cx: &mut Context<Self>) {
        if self.queue.is_empty() || self.is_busy() {
            return;
        }
        let text = self.queue.remove(0);
        self.commit_and_send(text, cx);
        cx.notify();
    }

    /// Enter on an empty input while messages are queued: cancel everything
    /// in flight — the reply stream, a compaction, and every running or
    /// still-unapproved tool call — then send the queue head immediately.
    /// The queue keeps its remaining entries; they go out one per turn as
    /// usual.
    fn send_queued_cancel_run(&mut self, cx: &mut Context<Self>) {
        if self.queue.is_empty() {
            return;
        }
        if let Some(stream) = &self.stream {
            stream.cancel();
        }
        if let Some(plan) = &self.compaction {
            plan.stream.cancel();
        }
        // The cancelled pieces finalize through generation-guarded updates
        // that get dropped once the new turn starts; close them out in the
        // panel state here so the send below is not blocked.
        self.stream = None;
        self.compaction = None;
        self.status = None;
        self.stream_pending.clear();
        self.stream_pending_reasoning.clear();
        self.finish_pending = None;
        self.generation += 1;
        // Every unresolved tool call is skipped: running ones record
        // "stopped waiting" (what the card's stop button does), unapproved
        // ones are refused with a note — the wire history can only carry
        // calls with resolved results.
        for msg_index in 0..self.messages.len() {
            for call_index in 0..self.messages[msg_index].tool_calls.len() {
                let Some(state) = self.tool_state_mut(msg_index, call_index) else {
                    continue;
                };
                let changed = state.running
                    || (state.decision == ToolDecision::Pending && state.result.is_none());
                if state.running {
                    state.cancel.cancel();
                    state.cancelled = true;
                    state.table = None;
                    state.result = Some(t!("ai_panel.tool_wait_stopped").to_string());
                    state.running = false;
                    state.ran_for = state.started_at.map(|started| started.elapsed());
                } else if state.decision == ToolDecision::Pending && state.result.is_none() {
                    state.decision = ToolDecision::Denied;
                    state.result = Some(t!("ai_panel.tool_cancelled_by_run").to_string());
                }
                if changed {
                    // The card's look changed (buttons or loader gone);
                    // let the list re-measure it.
                    let row = self.list_row_of_tool_call(msg_index, call_index);
                    if row < self.list_state.item_count() {
                        self.list_state.splice(row..row + 1, 1);
                    }
                }
            }
        }
        self.refresh_context_used();
        let text = self.queue.remove(0);
        self.commit_and_send(text, cx);
        cx.notify();
    }

    /// Put a queued entry back into the input for editing; only the queue's
    /// tail offers this (the entries go out head-first, so the tail is the
    /// one still waiting). The entry leaves the queue while edited; a
    /// re-submit while busy re-queues it.
    fn edit_queued(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.queue.len() {
            return;
        }
        let text = self.queue.remove(index);
        if let Some(input) = self.input.clone()
            && !text.is_empty()
        {
            input.update(cx, |state, cx| state.set_value(text.clone(), window, cx));
            input.update(cx, |state, cx| state.focus(window, cx));
            self.input_empty = text.trim().is_empty();
        }
        cx.notify();
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
        self.stream_pending.clear();
        self.stream_pending_reasoning.clear();
        self.finish_pending = None;
        self.error = None;
        // The "responding" row appears now; when the user was already pinned
        // to the bottom, keep them there. The scroll must wait for render's
        // splice (the row count only changes there) — `scroll_to_end_pending`
        // is exactly that deferred scroll. It only ever *sets* the flag: a
        // send just forced it, and clearing it here would undo the "sending
        // always follows the message" rule for a scrolled-up user.
        if self.is_list_at_bottom() {
            self.scroll_to_end_pending = true;
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next_event().await {
                let _ = this.update(cx, |panel, cx| {
                    if panel.generation != generation {
                        return;
                    }
                    // Buffer the chunk; the reveal ticker types it into the
                    // visible tail at a smooth pace (stick-to-bottom happens
                    // per reveal step, in `reveal_step`).
                    match event {
                        StreamEvent::Content(chunk) => panel.stream_pending.push_str(&chunk),
                        StreamEvent::Reasoning(chunk) => {
                            panel.stream_pending_reasoning.push_str(&chunk)
                        }
                        _ => return,
                    }
                    panel.ensure_reveal_ticker(cx);
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
    /// task observes it and finishes the turn with `Cancelled`. A compaction
    /// in flight is cancelled too — the held-up turn then runs without it.
    fn stop(&mut self) {
        if let Some(stream) = &self.stream {
            stream.cancel();
        }
        if let Some(plan) = &self.compaction {
            plan.stream.cancel();
        }
    }

    /// The stream ended: commit the turn — after the reveal has drained
    /// whatever is still buffered, so a burst that arrived in one final
    /// chunk still types out instead of popping in whole. Cancellation
    /// finalizes at once: the user asked it to stop, and anything not yet
    /// revealed is dropped (it never appeared on screen).
    fn finish_turn(&mut self, outcome: Result<ChatResponse, AiError>, cx: &mut Context<Self>) {
        if matches!(outcome, Err(AiError::Cancelled)) {
            self.stream_pending.clear();
            self.stream_pending_reasoning.clear();
            self.finalize_turn(outcome, cx);
            return;
        }
        if self.stream_pending.is_empty() && self.stream_pending_reasoning.is_empty() {
            self.finalize_turn(outcome, cx);
        } else {
            self.finish_pending = Some((self.generation, outcome));
            self.ensure_reveal_ticker(cx);
        }
    }

    /// One tick of the reveal: type out buffered content (faster when the
    /// backlog is large — catching up beats literal typing). Returns whether
    /// the ticker should keep running.
    fn reveal_step(&mut self, cx: &mut Context<Self>) -> bool {
        fn reveal_into(pending: &mut String, shown: &mut String) -> bool {
            if pending.is_empty() {
                return false;
            }
            let steps = (pending.chars().count() / 6).clamp(3, 240);
            let split = pending
                .char_indices()
                .nth(steps)
                .map(|(index, _)| index)
                .unwrap_or(pending.len());
            let head: String = pending.drain(..split).collect();
            shown.push_str(&head);
            true
        }

        // Pin first (reads the last rendered frame), then grow the tail.
        let stick = self.is_list_at_bottom();
        let moved = reveal_into(&mut self.stream_pending, &mut self.stream_buf)
            | reveal_into(
                &mut self.stream_pending_reasoning,
                &mut self.stream_reasoning,
            );
        if moved {
            if stick {
                self.scroll_to_end_pending = true;
            }
            cx.notify();
        }
        let draining = !self.stream_pending.is_empty() || !self.stream_pending_reasoning.is_empty();
        if draining {
            return true;
        }
        // Drained: commit the finished turn, if one is waiting.
        if let Some((generation, outcome)) = self.finish_pending.take()
            && self.generation == generation
        {
            self.finalize_turn(outcome, cx);
        }
        false
    }

    /// Start the reveal ticker unless one is running. The task stops itself
    /// once both buffers are drained (and after committing a waiting turn).
    fn ensure_reveal_ticker(&mut self, cx: &mut Context<Self>) {
        if self.reveal_ticking {
            return;
        }
        self.reveal_ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(16)).await;
                let keep = this
                    .update(cx, |panel, cx| panel.reveal_step(cx))
                    .unwrap_or(false);
                if !keep {
                    let _ = this.update(cx, |panel, _cx| panel.reveal_ticking = false);
                    return;
                }
            }
        })
        .detach();
    }

    /// Commit the finished turn (the reveal has drained or was cancelled).
    fn finalize_turn(&mut self, outcome: Result<ChatResponse, AiError>, cx: &mut Context<Self>) {
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
        // The turn changed the history; keep the context chip honest.
        self.refresh_context_used();
        cx.notify();
        // The panel is idle again (unless tool calls just opened): hand off
        // the queue head now, or leave it for the next idle break.
        self.send_queued(cx);
    }

    /// Attach the tool calls from one assistant turn to the conversation.
    ///
    /// The assistant's own text (if any) is already committed by
    /// [`Self::push_partial`]; when the model answered with *only* tool calls
    /// that leaves nothing to show, so this pushes an empty assistant turn to
    /// carry the calls — the card is the turn.
    fn push_tool_calls(&mut self, calls: Vec<ToolCall>, cx: &mut Context<Self>) {
        // A call that cannot run on this terminal is refused right here,
        // without a click: nothing would happen on approval, and the model is
        // better off hearing "use a tool that works" immediately so it can
        // retry while the user watches (terminal_run on a Serial connection,
        // the tunnel/SFTP tools on a local terminal, …).
        let session = self.agent_session(cx);
        let blocked = |name: &str| -> Option<String> {
            let Some(session) = session.as_ref() else {
                return Some(t!("ai_panel.tool_terminal_closed").to_string());
            };
            self.agent.blocked_reason(session.as_ref(), name)
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
                    table: None,
                    running: false,
                    exit_code: None,
                    timed_out: false,
                    cancel: ExecCancel::default(),
                    started_at: None,
                    ran_for: None,
                    expanded: true,
                    cancelled: false,
                };
                if let Some(reason) = blocked(&state.call.name) {
                    state.decision = ToolDecision::Unavailable;
                    state.result = Some(reason);
                    auto_resolved = true;
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
        // re-measures it, and bring it into view when the user is at the
        // bottom — there is something to approve. Scrolled-up readers keep
        // their place. Deferred to render (see `settle_tool`), so the scroll
        // lands after the new card rows are measured.
        let stick = self.is_list_at_bottom();
        self.remeasure_row(self.list_row_of_message(self.messages.len() - 1));
        if stick {
            self.scroll_to_end_pending = true;
        }

        // Calls the Agent settings already decided: `Allow` runs them now,
        // `Deny` refuses them now, both without a click — the card still
        // records what happened, and the loop continues once every call of
        // the turn has a result. `Confirm` (the default, and anything not
        // configured) leaves the card waiting for the user.
        let msg_index = self.messages.len() - 1;
        let agent_cfg = config::snapshot().ai.agent;
        let preset: Vec<(usize, ToolPermission)> = self.messages[msg_index]
            .tool_calls
            .iter()
            .enumerate()
            .filter(|(_, state)| state.decision == ToolDecision::Pending)
            .map(|(call_ix, state)| (call_ix, agent_cfg.permission(&state.call.name)))
            .filter(|(_, permission)| *permission != ToolPermission::Confirm)
            .collect();
        for (call_ix, permission) in preset {
            auto_resolved = true;
            match permission {
                ToolPermission::Allow => self.run_tool(msg_index, call_ix, cx),
                _ => self.settle_tool_result(
                    msg_index,
                    call_ix,
                    t!("ai_panel.tool_denied_policy").to_string(),
                    ToolDecision::Denied,
                    cx,
                ),
            }
        }

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
    /// A [`DisplayRole::Summary`] turn (automatic compaction) is sent as one
    /// user message carrying the summarized history, so every
    /// OpenAI-compatible gateway accepts it.
    ///
    /// Turns with unresolved calls are skipped entirely: they cannot be
    /// replayed, and [`Self::send`] refuses to build a request while any call
    /// is pending.
    fn wire_history(&self) -> Vec<ChatMessage> {
        let mut wire: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 1);
        wire.push(ChatMessage::system(system_prompt()));
        for msg in &self.messages {
            match msg.role {
                DisplayRole::User => wire.push(ChatMessage::user(msg.content.clone())),
                DisplayRole::Summary => wire.push(ChatMessage::user(format!(
                    "{COMPACTED_HEADER}{}",
                    msg.content
                ))),
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
        // The model's arguments were unparseable: nothing may run, and it must
        // be told that instead of assuming success.
        if state.args.is_none() {
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

        self.run_tool(msg_index, call_index, cx);
    }

    /// Run one approved call through the agent and settle its card with the
    /// outcome.
    ///
    /// The agent owns the work — including the polling loops, which is why it
    /// can live off the UI thread at all. It answers either a finished result
    /// ([`Executed::Done`]), a running one on a channel this side awaits
    /// ([`Executed::Pending`]), or a refusal when the call cannot start
    /// ([`Executed::Refused`]: missing arguments at execution time, or a
    /// capability the session turned out not to have).
    fn run_tool(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        let Some(state) = self.tool_state_mut(msg_index, call_index) else {
            return;
        };
        let name = state.call.name.clone();
        let Some(args) = state.args.clone() else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_bad_args").to_string(),
                ToolDecision::Denied,
                cx,
            );
            return;
        };
        let Some(session) = self.agent_session(cx) else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_terminal_closed").to_string(),
                ToolDecision::Allowed,
                cx,
            );
            return;
        };
        // The card's elapsed readout starts with the run (also applies to
        // synchronously-completing calls, so long waits like `tunnel_list`'s
        // network work read honestly).
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.started_at = Some(Instant::now());
        }
        match self.agent.run(&session, &name, &args) {
            Executed::Done(outcome) => self.finish_tool(msg_index, call_index, outcome, cx),
            Executed::Refused(reason) => {
                self.settle_tool_result(msg_index, call_index, reason, ToolDecision::Denied, cx)
            }
            Executed::Pending { outcome, cancel } => {
                if let Some(state) = self.tool_state_mut(msg_index, call_index) {
                    state.decision = ToolDecision::Allowed;
                    state.running = true;
                    state.cancel = cancel;
                }
                // The card is shrinking from buttons to a running state;
                // keep the view pinned when it was at the bottom (the same
                // rule `settle_tool` applies when the result lands).
                let stick = self.is_list_at_bottom();
                self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
                if stick {
                    self.scroll_to_end_pending = true;
                }
                cx.notify();
                // The card now shows a running state with an elapsed readout
                // that has to tick.
                self.spawn_elapsed_ticker(cx);
                let entity = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    // The agent sends exactly once; a closed channel without
                    // an outcome means its worker went away mid-call.
                    let outcome = outcome.recv().await.unwrap_or_else(|_| {
                        ToolOutcome::text(t!("ai_panel.tool_exec_capture_failed").to_string())
                    });
                    let _ = entity.update(cx, |panel, cx| {
                        panel.finish_tool(msg_index, call_index, outcome, cx);
                    });
                })
                .detach();
            }
        }
    }

    /// Store one finished call's outcome on its card and continue the loop.
    ///
    /// A late arrival after the user already stopped waiting for the call is
    /// ignored — the "stopped waiting" record must not be overwritten, and
    /// the loop already moved on.
    fn finish_tool(
        &mut self,
        msg_index: usize,
        call_index: usize,
        outcome: ToolOutcome,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            if state.cancelled {
                return;
            }
            state.result = Some(outcome.text);
            state.table = outcome.table;
            state.exit_code = outcome.exit_code;
            state.timed_out = outcome.timed_out;
            state.cancelled = outcome.cancelled;
            state.running = false;
            state.ran_for = state.started_at.map(|started| started.elapsed());
            // Synchronously-settling calls finish through here without any
            // click when the permission preset is `Allow` — the decision must
            // leave `Pending` or the approval buttons would stay on a card
            // that already has its result.
            if state.decision == ToolDecision::Pending {
                state.decision = ToolDecision::Allowed;
            }
        }
        self.settle_tool(msg_index, call_index, cx);
    }

    /// Notify once every 500ms while any card is running, so the elapsed
    /// readouts tick; the task per run self-terminates in the next tick
    /// after the last card settles (or when the panel dies). Overlapping
    /// tasks from concurrent runs no-op onto each other.
    fn spawn_elapsed_ticker(&self, cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(500)).await;
                let alive = this
                    .update(cx, |panel, cx| {
                        if panel.running_tool_count() == 0 {
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }
            }
        })
        .detach();
    }

    /// Tool calls currently awaiting their result (the running cards the
    /// elapsed readout ticks for).
    fn running_tool_count(&self) -> usize {
        self.messages
            .iter()
            .flat_map(|msg| msg.tool_calls.iter())
            .filter(|call| call.running)
            .count()
    }

    /// A cx-free snapshot of this panel's terminal for the agent's tools,
    /// plus the panel-scoped tunnel plumbing: the manager the agent's own
    /// tunnels live in (created on first use from the terminal's SSH source)
    /// and the registry-command channel. `None` when the terminal is gone.
    fn agent_session(&mut self, cx: &mut Context<Self>) -> Option<Arc<dyn AgentSession>> {
        let handles = self.session.terminal.upgrade()?.read(cx).agent_handles();
        if self.tunnel_manager.is_none()
            && let Some(source) = handles.tunnel_source.clone()
        {
            self.tunnel_manager = Some(Arc::new(TunnelManager::new(source, Arc::new(|| {}))));
        }
        Some(Arc::new(UiAgentSession::new(
            handles,
            self.session.tunnels.clone(),
            self.session.registry_commands.clone(),
            self.tunnel_manager.clone(),
        )))
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
    ///
    /// When the view was pinned to the bottom, pin it again: the card's new
    /// height would otherwise leave a gap below (a shrunk card) or push the
    /// fresh result out of view (a grown one).
    fn settle_tool(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        // A settled call folds its card to the header line (title + elapsed
        // + status icon), like the thinking section once a turn commits —
        // the result is a record now, not the focus; the user expands it
        // when they need to read the output.
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.expanded = false;
        }
        // `is_list_at_bottom` reads the bounds the last frame rendered, i.e.
        // the position the user actually had before this change.
        let stick = self.is_list_at_bottom();
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        // Deferred, not immediate: the row's new height is only measured
        // during the next layout pass, and a scroll issued now would land at
        // the *old* bottom — just short of the fresh result. The render
        // applies the scroll after re-splicing, against the new measurements.
        if stick {
            self.scroll_to_end_pending = true;
        }
        self.refresh_context_used();
        self.maybe_continue_after_tools(cx);
        cx.notify();
    }

    /// Send the resolved tool results back to the model, so it can act on
    /// them — compacting the history first when the next request would eat
    /// too much of the model's context window.
    ///
    /// There is deliberately no cap on how many rounds a turn may take: every
    /// call is approved by the user before it runs, so the loop only continues
    /// while they keep saying yes — and they can stop it at any point with the
    /// composer's stop button.
    fn continue_after_tools(&mut self, cx: &mut Context<Self>) {
        // One turn at a time. Settling a chain of calls asks to continue once
        // per call, and the first request already carries every result that
        // exists — starting a second stream here would send a duplicate.
        if self.stream.is_some() || self.compaction.is_some() {
            return;
        }
        self.refresh_context_used();
        if self.needs_compaction() && self.start_compaction(cx) {
            // The compaction round sends the held-up request itself once the
            // summary lands.
            return;
        }
        self.start_turn(cx);
    }

    /// Build the request from the current history and start streaming it.
    fn start_turn(&mut self, cx: &mut Context<Self>) {
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

    /// The context window to budget against: what the endpoint advertised
    /// for the active model, or [`FALLBACK_CONTEXT_WINDOW`].
    fn context_window(&self) -> usize {
        let ai = config::snapshot().ai;
        ai.active_provider()
            .and_then(|entry| {
                self.model_windows
                    .get(&format!("{}|{}", entry.id, ai.model))
            })
            .copied()
            .unwrap_or(FALLBACK_CONTEXT_WINDOW)
    }

    /// Recompute [`Self::context_used`] — walks the whole history, so it runs
    /// when the conversation changes (per turn / per tool result), never per
    /// frame.
    fn refresh_context_used(&mut self) {
        let model = config::snapshot().ai.model;
        let request = ChatRequest::new(model)
            .with_messages(self.wire_history())
            .with_tools(agent_tools());
        self.context_used = request.estimated_tokens();
    }

    /// Whether the next request would eat too much of the model's window.
    fn needs_compaction(&self) -> bool {
        self.context_used as f32 >= self.context_window() as f32 * COMPACT_AT
    }

    /// Summarize the older part of the conversation in place, then send the
    /// request that was waiting on it. This is the automatic context
    /// compaction that lets a long session keep going without the user ever
    /// managing history.
    ///
    /// Returns `true` once a round is in flight; `false` when there is
    /// nothing worth compacting (the caller should just run the turn).
    fn start_compaction(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(provider) = ai::resolve_provider_session(cx, &self.session_id) else {
            self.error = Some(t!("ai_panel.not_configured").to_string());
            cx.notify();
            return false;
        };
        let keep = (self.context_window() as f32 * COMPACT_KEEP) as usize;
        let cut = compaction_cut(&self.compact_turns(self.messages.len()), keep);
        if cut == 0 {
            // Everything is recent enough to keep — nothing to summarize.
            return false;
        }
        let transcript = compaction_transcript(&self.compact_turns(cut));
        let model = config::snapshot().ai.model;
        let request = ChatRequest::new(model).with_messages(vec![
            ChatMessage::system(COMPACTION_PROMPT),
            ChatMessage::user(transcript),
        ]);
        let stream = crabport_ai::spawn_chat_stream(Arc::new(provider), request);

        self.generation += 1;
        let generation = self.generation;
        self.compaction = Some(CompactionPlan {
            cut,
            stream: stream.clone(),
        });
        self.status = Some(t!("ai_panel.compacting").to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            // Drain the events: the summary is in the final response, and an
            // unread channel would stall the worker thread.
            while stream.next_event().await.is_some() {}
            let outcome = stream.result().await;
            let _ = this.update(cx, |panel, cx| {
                if panel.generation == generation {
                    panel.finish_compaction(outcome, cx);
                }
            });
        })
        .detach();
        true
    }

    /// Apply a finished compaction round and start the turn it was holding
    /// up. A failed or stopped round leaves the history as it was — the turn
    /// then runs (and fails loudly) rather than being silently dropped.
    fn finish_compaction(
        &mut self,
        outcome: Result<ChatResponse, AiError>,
        cx: &mut Context<Self>,
    ) {
        let Some(plan) = self.compaction.take() else {
            return;
        };
        self.status = None;
        match outcome {
            Ok(response) => {
                let summary = response.message.content.trim().to_string();
                if summary.is_empty() {
                    self.error = Some(t!("ai_panel.compact_failed").to_string());
                } else {
                    self.apply_summary(plan.cut, summary, cx);
                }
            }
            Err(AiError::Cancelled) => {
                // A stop while compacting cancels the round *and* the turn it
                // was holding up: the user asked for nothing to happen. Their
                // message stays in the transcript, unanswered.
                cx.notify();
                return;
            }
            Err(err) => {
                self.error = Some(format!("{}: {err}", t!("ai_panel.compact_failed")));
            }
        }
        self.start_turn(cx);
    }

    /// The conversation as the compaction helpers see it: the first `count`
    /// turns, borrowed for the duration of one compaction decision.
    fn compact_turns(&self, count: usize) -> Vec<CompactTurn<'_>> {
        self.messages[..count]
            .iter()
            .map(|msg| CompactTurn {
                role: match msg.role {
                    DisplayRole::User => TurnRole::User,
                    DisplayRole::Assistant => TurnRole::Assistant,
                    DisplayRole::Summary => TurnRole::Summary,
                },
                content: &msg.content,
                tool_calls: msg
                    .tool_calls
                    .iter()
                    .map(|call| CompactCall {
                        name: &call.call.name,
                        arguments: &call.call.arguments,
                        result: call.result.as_deref(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// Replace `messages[..cut]` with one summary turn. The transcript keeps
    /// its place — as a collapsed block at the top of the conversation — so
    /// the user can always see that history was compacted, and what it became.
    fn apply_summary(&mut self, cut: usize, summary: String, cx: &mut Context<Self>) {
        self.messages.drain(..cut);
        self.messages.insert(
            0,
            DisplayMessage {
                role: DisplayRole::Summary,
                content: summary,
                reasoning: String::new(),
                // Reused as the block's disclosure flag.
                reasoning_expanded: false,
                tool_calls: Vec::new(),
            },
        );
        // Every row shifted and the first one changed identity: drop the
        // cached measurements and let render re-splice to the new count.
        self.list_state.reset(0);
        self.scroll_to_end_pending = true;
        self.refresh_context_used();
        cx.notify();
    }

    /// Flip the compacted-history block's disclosure.
    fn toggle_summary(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(msg) = self.messages.get_mut(index) {
            msg.reasoning_expanded = !msg.reasoning_expanded;
            cx.notify();
        }
    }

    /// Stop a running call — the card's stop button. The command executors
    /// stop for real through their cancel token: a captured command is closed
    /// down (its output so far is kept) and a visible run stops being waited
    /// on, the command itself keeps running in the terminal for the user to
    /// interrupt. Tools without a stop token (fetch, SFTP waits, tunnel
    /// lifecycle) settle right here as "stopped waiting" — like a deny, the
    /// model hears about it and the loop moves on; a late outcome arriving
    /// through [`Self::finish_tool`]'s guard must not overwrite the record.
    fn stop_tool_call(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        let Some(state) = self.tool_state_mut(msg_index, call_index) else {
            return;
        };
        if !state.running {
            return;
        }
        let cancellable = matches!(
            ToolKind::of(&state.call.name),
            ToolKind::ExecCapture | ToolKind::Run
        );
        state.cancel.cancel();
        if cancellable {
            // The executors' worker settles the card itself (kept output, the
            // cancelled marker); the card just shows its stop until then.
            cx.notify();
            return;
        }
        state.cancelled = true;
        state.table = None;
        state.result = Some(t!("ai_panel.tool_wait_stopped").to_string());
        state.running = false;
        state.ran_for = state.started_at.map(|started| started.elapsed());
        self.settle_tool(msg_index, call_index, cx);
    }

    /// Fold/unfold one tool card (the header row is the click target, like
    /// the thinking block). A card that still needs attention — pending
    /// approval or running — cannot fold: its buttons and status are the
    /// whole point.
    fn toggle_tool_card(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        let Some(state) = self.tool_state_mut(msg_index, call_index) else {
            return;
        };
        if state.running || state.decision == ToolDecision::Pending {
            return;
        }
        state.expanded = !state.expanded;
        let stick = self.is_list_at_bottom();
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        if stick {
            self.scroll_to_end_pending = true;
        }
        cx.notify();
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
        // drop the cached row measurement so the list re-measures it, and
        // keep the view pinned when the user was at the bottom (deferred to
        // render — see `settle_tool`).
        let stick = self.is_list_at_bottom();
        self.remeasure_row(self.list_row_of_message(self.messages.len() - 1));
        if stick {
            self.scroll_to_end_pending = true;
        }
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
            // One row for the whole live stream — the "responding" spinner
            // before the first token, the streaming tail after it (and while
            // a finished turn's reveal is still draining).
            + usize::from(self.stream.is_some() || self.finish_pending.is_some())
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
                DisplayRole::Summary => MessageItem::Summary {
                    message_ix,
                    text: msg.content.clone(),
                    expanded: msg.reasoning_expanded,
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
                let tunnel = if ToolKind::of(&call.call.name) == ToolKind::Tunnel {
                    call.arg_str("tunnel_id").map(|token| {
                        // `a…` ids resolve against this panel's own agent (the
                        // tunnels it defined); `t…` ids against the Tunnels-page
                        // registry (host-scoped; anything else shows the token
                        // untouched).
                        self.agent
                            .describe_tunnel_token(self.tunnel_manager.as_ref(), token)
                            .unwrap_or_else(|| {
                                let known = token
                                    .strip_prefix('t')
                                    .and_then(|number| number.parse::<i64>().ok())
                                    .and_then(|config_id| {
                                        self.session
                                            .tunnels
                                            .list()
                                            .into_iter()
                                            .find(|view| view.id == config_id)
                                    });
                                match known {
                                    Some(view) => {
                                        format!("{} {} ({})", token, view.name, view.kind.as_str())
                                    }
                                    None => token.to_string(),
                                }
                            })
                    })
                } else {
                    None
                };
                items.push(MessageItem::Tool {
                    message_ix,
                    call_ix,
                    state: call.clone(),
                    cwd: cwd.map(str::to_string),
                    tunnel,
                });
            }
        }
        if self.stream.is_some() || self.finish_pending.is_some() {
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
                    Ok(models) => {
                        // Remember each model's advertised context window —
                        // the budget automatic compaction runs against.
                        for model in &models {
                            if let Some(window) = model.context_window {
                                panel
                                    .model_windows
                                    .insert(format!("{provider_id}|{}", model.id), window);
                            }
                        }
                        let mut ids: Vec<String> =
                            models.into_iter().map(|model| model.id).collect();
                        ids.sort();
                        ids.dedup();
                        panel.models_by_provider.insert(provider_id, ids);
                        panel.refresh_context_used();
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
        // Compaction counts as busy: the held-up turn can only start once it
        // lands, and the stop button must be able to call it off.
        let busy = self.stream.is_some() || self.compaction.is_some();

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
            || self.stream.is_some()
            || self.finish_pending.is_some()
            || !self.queue.is_empty();
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
        // A compaction rewrote the conversation from the top; now that the
        // row count matches the new history, put the view at the end.
        if self.scroll_to_end_pending {
            self.scroll_to_end_pending = false;
            self.scroll_list_to_end();
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
        // The input draws its own placeholder at the text origin (see
        // `ensure_input`), so the panel adds no overlay here: anything drawn
        // separately would be a few pixels off the caret, and would have to
        // duplicate the "hide while composing" rule by hand.
        let input_empty = self.input_empty;
        // Queued messages between the controls and the input: one muted row
        // each, in send order. The tail (last entry) gets an edit button;
        // every row can be dropped.
        let queue_rows: AnyElement = if self.queue.is_empty() {
            div().into_any_element()
        } else {
            let count = self.queue.len();
            let mut col = div().flex().flex_col().gap_0p5();
            for (index, text) in self.queue.iter().enumerate() {
                // The tail (last entry — the one that can be edited back into
                // the input) sits right over the input; every row can be
                // dropped from the queue.
                let is_tail = index + 1 == count;
                let h_edit = handle.clone();
                let h_del = handle.clone();
                let mut row = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        svg()
                            .path("icons/clock.svg")
                            .size(px(12.0))
                            .text_color(rgb(text_muted()))
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(CONVERSATION_TEXT_SIZE - 1.0))
                            .text_color(rgb(text_primary()))
                            .child(text.clone()),
                    );
                if is_tail {
                    row = row.child(
                        Button::new(ElementId::Name(format!("ai-queue-edit-{index}").into()))
                            .icon("icons/square-pen.svg")
                            .icon_color(text_muted())
                            .flex_shrink_0()
                            .size_6()
                            .centered(true)
                            .on_click(move |_e, win, cx| {
                                h_edit.update(cx, |panel, cx| panel.edit_queued(index, win, cx));
                            }),
                    );
                }
                row = row.child(
                    Button::new(ElementId::Name(format!("ai-queue-del-{index}").into()))
                        .icon("icons/x.svg")
                        .icon_color(text_muted())
                        .flex_shrink_0()
                        .size_6()
                        .centered(true)
                        .on_click(move |_e, _win, cx| {
                            h_del.update(cx, |panel, cx| {
                                if index < panel.queue.len() {
                                    panel.queue.remove(index);
                                    cx.notify();
                                }
                            });
                        }),
                );
                col = col.child(row);
            }
            col.into_any_element()
        };
        let input_area = div()
            // With no conversation the input area *is* the panel: it takes
            // every pixel the controls row doesn't need, and the editor
            // element (`height: 100%` + `flex_grow` in the library) fills
            // it. Once there are messages it goes back to its natural
            // height — 6 rows, auto-growing to 12.
            .when(!has_conversation, |el| el.flex_1().min_h_0())
            .child(input_el);
        // Send / Stop. One slot, icon-only, three states:
        //
        // - idle → send (disabled until there is something to send);
        // - busy with no messages queued → stop (the only way to stop — Esc
        //   isn't wired);
        // - busy with a queue → send-as-advance: the click cancels what is
        //   running and sends the queue head, mirroring the bare-Enter rule.
        //   Typing while busy queues via Enter; getting the stop slot back
        //   is a matter of emptying the queue.
        // A busy agent with pending tool calls but nothing queued still gets
        // the stop slot — `busy_tools` doesn't change the button, the queue
        // head cannot go out while a card waits anyway.
        let action_btn: AnyElement = if busy && self.queue.is_empty() {
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
        } else if busy {
            let h = handle.clone();
            Button::new("ai-panel-send-queued")
                .icon("icons/send-horizontal.svg")
                .primary()
                .size_6()
                .flex_shrink_0()
                .centered(true)
                .on_click(move |_e, _w, cx| {
                    h.update(cx, |panel, cx| panel.send_queued_cancel_run(cx));
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

        // Context accounting is deliberately invisible: the panel compacts
        // on its own and never asks the user to watch a number. The estimate
        // still feeds `needs_compaction` — it just isn't rendered.

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
            .when_some(self.status.clone(), |el, status| {
                el.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(status),
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
                    .when(!has_conversation, |el| el.flex_1().min_h_0())
                    .when(!has_conversation && not_configured, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(t!("ai_panel.not_configured").to_string()),
                        )
                    })
                    .child(queue_rows)
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

impl Drop for AiPanel {
    /// Agent tunnels ride this terminal's connection; when the panel goes
    /// away they must go too, or their accept loops would outlive the
    /// conversation and keep a port bound on the user's machine. The stop is
    /// a few aborts (plus one round trip for a remote forward), bounded, and
    /// only blocks when something is actually running.
    fn drop(&mut self) {
        if let Some(manager) = self.tunnel_manager.take()
            && !manager.list().is_empty()
        {
            crabport_ssh::TOKIO.block_on(manager.stop_all());
        }
    }
}

/// One row of the virtualized conversation list. Snapshotted from the
/// panel's messages once per frame: `List`'s render closure gets no access
/// to the view, so rows must be self-contained.
///
/// The `Tool` row deliberately carries the whole [`ToolCallState`] — the
/// snapshot must be self-contained, so the size warning is by design.
#[allow(clippy::large_enum_variant)]
enum MessageItem {
    User {
        content: String,
    },
    /// The compacted-history marker: one dim block where the summarized turns
    /// used to be, clicked to show the summary text.
    Summary {
        message_ix: usize,
        text: String,
        expanded: bool,
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
        /// The tunnel a tunnel_* call refers to, described at snapshot time
        /// (`#2 socks — active on 127.0.0.1:1080`). `None` for every other
        /// tool.
        tunnel: Option<String>,
    },
    Streaming {
        reasoning: String,
        content: String,
        streaming: bool,
    },
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
        Some(MessageItem::Summary {
            message_ix,
            text,
            expanded,
        }) => row
            .child(summary_block(
                *message_ix,
                text.clone(),
                *expanded,
                entity,
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
            tunnel,
        }) => row
            .child(tool_card(
                *message_ix,
                *call_ix,
                state.clone(),
                cwd.as_deref(),
                tunnel.as_deref(),
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
    tunnel: Option<&str>,
    entity: &WeakEntity<AiPanel>,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let name = state.call.name.clone();
    let kind = ToolKind::of(&name);
    let title = tool_title(&name);
    let accent = tool_accent(&name);
    let is_execute = matches!(kind, ToolKind::ExecCapture | ToolKind::Run);
    // The elapsed readout: live while running (the card ticks), frozen to
    // the final duration once settled — but only when the wait was long
    // enough to mean something ("0.0s" on instant calls is noise).
    let elapsed: Option<String> = if state.running {
        state
            .started_at
            .map(|started| format_elapsed(started.elapsed()))
    } else {
        state
            .ran_for
            .filter(|ran| *ran >= Duration::from_secs(1))
            .map(format_elapsed)
    };

    // Collapsible like the thinking section: while the card needs attention
    // (awaiting a click, or running) it stays expanded no matter what the
    // flag says; once the result has settled the header folds the rest away.
    let collapsible = !state.running && state.decision != ToolDecision::Pending;
    let expanded = state.expanded || !collapsible;
    let chevron_group: SharedString = format!("ai-tool-group-{message_ix}-{call_ix}").into();
    let header = div()
        .id(ElementId::Name(
            format!("ai-tool-header-{message_ix}-{call_ix}").into(),
        ))
        .group(chevron_group.clone())
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .when(collapsible, |el| {
            el.cursor_pointer().on_click({
                let entity = entity.clone();
                move |_event, _window, cx| {
                    let _ = entity.update(cx, |panel, cx| {
                        panel.toggle_tool_card(message_ix, call_ix, cx)
                    });
                }
            })
        })
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
        // The elapsed readout ticks while the call runs (a 500ms ticker
        // notifies the panel) and freezes at the final duration once
        // settled; long waits — the kind a user asks "is it stuck?" about —
        // are what makes it worth having.
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .children(elapsed.map(|label| {
                    div()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .font_family(TerminalView::mono_font_family())
                        .child(label)
                }))
                .when(!state.running, |el| {
                    el.when_some(state_icon(&state.decision, false), |el, (path, color)| {
                        el.child(svg().path(path).size(px(14.0)).text_color(rgb(color)))
                    })
                })
                .when(state.running, |el| {
                    el.child(loading_spinner(
                        ElementId::Name(format!("ai-tool-loader-{message_ix}-{call_ix}").into()),
                        14.0,
                        text_muted(),
                    ))
                })
                .when(collapsible, |el| {
                    el.child(
                        div()
                            .id(ElementId::Name(
                                format!("ai-tool-chevron-{message_ix}-{call_ix}").into(),
                            ))
                            .opacity(0.5)
                            .group_hover(chevron_group, |style| style.opacity(1.0))
                            .child(
                                svg()
                                    .path("icons/chevron-down.svg")
                                    .size(px(12.0))
                                    .text_color(rgb(text_muted()))
                                    .with_transformation(Transformation::rotate(radians(
                                        if expanded { std::f32::consts::PI } else { 0.0 },
                                    ))),
                            ),
                    )
                }),
        );
    let card = div()
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
        .child(header);
    let mut body = div().flex().flex_col().gap_1().w_full();

    // Body: the fields the user asked for, per tool — the arguments spell out
    // exactly what a click would do.
    if is_execute {
        let command = state.arg_str("command").unwrap_or("");
        let expected = state.arg_str("expected").unwrap_or("");
        body = body
            .child(tool_card_field(
                t!("ai_panel.tool_field_path").as_ref(),
                cwd.map(str::to_string)
                    .unwrap_or_else(|| t!("ai_panel.tool_field_path_unknown").to_string()),
                false,
            ))
            .child(command_block(
                format!("ai-tool-{message_ix}-{call_ix}"),
                command.to_string(),
            ));
        if !expected.is_empty() {
            body = body.child(tool_card_field(
                t!("ai_panel.tool_field_expected").as_ref(),
                expected.to_string(),
                false,
            ));
        }
    } else {
        let purpose = state.arg_str("purpose").unwrap_or("");
        match kind {
            ToolKind::TerminalRead => {
                let lines = state.arg_u64("lines").unwrap_or(200);
                body = body.child(tool_card_field(
                    t!("ai_panel.tool_field_read").as_ref(),
                    t!("ai_panel.tool_field_read_lines", lines = lines).to_string(),
                    false,
                ));
            }
            ToolKind::Fetch => {
                let url = state.arg_str("url").unwrap_or("");
                body = body.child(tool_card_field(
                    t!("ai_panel.tool_field_url").as_ref(),
                    url.to_string(),
                    true,
                ));
                if let Some(proxy) = state.arg_str("proxy") {
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_proxy").as_ref(),
                        proxy.to_string(),
                        true,
                    ));
                }
                if let Some(method) = state.arg_str("method")
                    && !method.eq_ignore_ascii_case("GET")
                {
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_method").as_ref(),
                        method.to_string(),
                        false,
                    ));
                }
                if let Some(request_body) = state.arg_str("body") {
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_body").as_ref(),
                        request_body.to_string(),
                        true,
                    ));
                }
                if let Some(expected) = state.arg_str("expected")
                    && !expected.is_empty()
                {
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_expected").as_ref(),
                        expected.to_string(),
                        false,
                    ));
                }
            }
            ToolKind::LocalFs => {
                let label = if name == "read_file" {
                    t!("ai_panel.tool_field_local_file")
                } else {
                    t!("ai_panel.tool_field_local_dir")
                };
                body = body.child(tool_card_field(
                    label.as_ref(),
                    state.arg_str("path").unwrap_or("").to_string(),
                    true,
                ));
            }
            ToolKind::Tunnel => {
                if let Some(tunnel) = tunnel {
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_tunnel").as_ref(),
                        tunnel.to_string(),
                        false,
                    ));
                }
                if name == "tunnel_create" {
                    let kind_arg = state.arg_str("kind").unwrap_or("dynamic");
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_tunnel_kind").as_ref(),
                        kind_arg.to_string(),
                        false,
                    ));
                    let bind = state
                        .arg_str("bind_addr")
                        .unwrap_or("127.0.0.1")
                        .to_string()
                        + ":"
                        + &state.arg_u64("bind_port").unwrap_or(0).to_string();
                    body = body.child(tool_card_field(
                        t!("ai_panel.tool_field_bind").as_ref(),
                        bind,
                        true,
                    ));
                    if kind_arg != "dynamic" {
                        let target = format!(
                            "{}:{}",
                            state.arg_str("target_host").unwrap_or("?"),
                            state.arg_u64("target_port").unwrap_or(0)
                        );
                        body = body.child(tool_card_field(
                            t!("ai_panel.tool_field_target").as_ref(),
                            target,
                            true,
                        ));
                    }
                    if let Some(expected) = state.arg_str("expected")
                        && !expected.is_empty()
                    {
                        body = body.child(tool_card_field(
                            t!("ai_panel.tool_field_expected").as_ref(),
                            expected.to_string(),
                            false,
                        ));
                    }
                }
            }
            ToolKind::Sftp => {
                let (label, value) = match name.as_str() {
                    "sftp_list" => (
                        t!("ai_panel.tool_field_remote_dir"),
                        state.arg_str("path").unwrap_or("").to_string(),
                    ),
                    "sftp_download" => (
                        t!("ai_panel.tool_field_remote_path"),
                        state.arg_str("remote_path").unwrap_or("").to_string(),
                    ),
                    _ => (
                        t!("ai_panel.tool_field_local_path"),
                        state.arg_str("local_path").unwrap_or("").to_string(),
                    ),
                };
                body = body.child(tool_card_field(label.as_ref(), value, true));
                let second = match name.as_str() {
                    "sftp_download" => Some((
                        t!("ai_panel.tool_field_local_path"),
                        state.arg_str("local_path").unwrap_or("").to_string(),
                    )),
                    "sftp_upload" => Some((
                        t!("ai_panel.tool_field_remote_path"),
                        state.arg_str("remote_path").unwrap_or("").to_string(),
                    )),
                    _ => None,
                };
                if let Some((label, value)) = second {
                    body = body.child(tool_card_field(label.as_ref(), value, true));
                }
            }
            ToolKind::ExecCapture | ToolKind::Run | ToolKind::Unknown => {}
        }
        if !purpose.is_empty() {
            body = body.child(tool_card_field(
                t!("ai_panel.tool_field_purpose").as_ref(),
                purpose.to_string(),
                false,
            ));
        }
    }

    // Malformed arguments: say so instead of offering a decision that would
    // do nothing.
    if state.args.is_none() {
        body = body.child(
            div()
                .text_xs()
                .text_color(rgb(input_border_error()))
                .child(t!("ai_panel.tool_bad_args").to_string()),
        );
    }

    // How the call ended, in the same muted register as the field labels:
    // the green check in the corner already says "it ran", so only a stop, a
    // timeout or a non-zero status is worth spelling out.
    if state.cancelled {
        let caption = match kind {
            ToolKind::ExecCapture => t!("ai_panel.tool_capture_cancelled").to_string(),
            _ => t!("ai_panel.tool_run_cancelled").to_string(),
        };
        body = body.child(div().text_xs().text_color(rgb(text_muted())).child(caption));
    } else if state.timed_out {
        body = body.child(
            div().text_xs().text_color(rgb(term_yellow())).child(
                t!(
                    "ai_panel.tool_capture_timed_out",
                    secs = EXEC_CAPTURE_TIMEOUT.as_secs()
                )
                .to_string(),
            ),
        );
    } else if let Some(code) = state.exit_code.filter(|code| *code != 0) {
        body = body.child(
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
            body = body.child(
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
            // A list-style result renders as the native table the outcome
            // carried (the model got the same cells as JSON); everything else
            // stays a code fence.
            let result_view: AnyElement = match &state.table {
                Some(table) if !table.rows.is_empty() => {
                    tool_result_table(table, format!("ai-tool-table-{message_ix}-{call_ix}"))
                }
                _ if result.trim().is_empty() => div().into_any_element(),
                _ => {
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
                }
            };
            body = body.child(result_view);
            if state.running {
                // Stop is on every running card now: the command executors
                // stop for real (their worker settles the card with the kept
                // output), tools without a stop token of their own — fetch,
                // SFTP waits, tunnel lifecycle — settle right here as
                // "stopped waiting", like a deny; the work may still finish
                // in the background.
                let red = term_red();
                let h = entity.clone();
                body = body.child(
                    div().flex().flex_row().justify_end().child(
                        Button::new(ElementId::Name(
                            format!("ai-tool-stop-{message_ix}-{call_ix}").into(),
                        ))
                        .icon("icons/square.svg")
                        .icon_color(red)
                        .bg((red << 8) | 0x33)
                        .bg_hover((red << 8) | 0x4d)
                        .bg_selected((red << 8) | 0x66)
                        .border_color((red << 8) | 0x99)
                        .size_6()
                        .centered(true)
                        .on_click(move |_e, _w, cx| {
                            let _ = h.update(cx, |panel, cx| {
                                panel.stop_tool_call(message_ix, call_ix, cx)
                            });
                        }),
                    ),
                );
            }
        }
    }

    card.when(expanded, |el| el.child(body)).into_any_element()
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

/// A rotating loader icon — the shared "this is working" glyph for the
/// streaming-tail's responding row and a running tool card's corner. gpui's
/// animation keeps it spinning (one full turn per 0.9s).
fn loading_spinner(id: impl Into<ElementId>, size: f32, color: u32) -> AnyElement {
    svg()
        .path("icons/loader-circle.svg")
        .size(px(size))
        .text_color(rgb(color))
        .with_animation(
            id.into(),
            Animation::new(Duration::from_millis(900)).repeat(),
            move |icon, delta| {
                icon.with_transformation(Transformation::rotate(radians(
                    delta * std::f32::consts::PI * 2.0,
                )))
            },
        )
        .into_any_element()
}

/// Elapsed-time label for a running/settled tool card: `3.2s`, `1m 05s`.
fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f32();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{:02}s", secs as u64 / 60, (secs % 60.0) as u64)
    }
}

/// The command an approval card asks about, as a mono block with a copy
/// button — the one thing the user might want verbatim (paste it into the
/// terminal themselves, or hand it to a snippet).
fn command_block(base_id: String, command: String) -> AnyElement {
    if command.trim().is_empty() {
        return div().into_any_element();
    }
    let h = command.clone();
    div()
        .flex()
        .flex_col()
        .w_full()
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("ai_panel.tool_field_command").to_string()),
        )
        .child(
            div()
                .w_full()
                .mt_0p5()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .rounded(RADIUS_MD)
                .border_1()
                .border_color(rgb(border()))
                .bg(rgb(surface_hover()))
                .pl_2()
                .pr_1()
                .py_1p5()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(TerminalView::mono_font_family())
                        .text_size(px(12.0))
                        .text_color(rgb(text_primary()))
                        .child(command),
                )
                .child(
                    Button::new(ElementId::Name(format!("{base_id}-copy").into()))
                        .icon("icons/copy.svg")
                        .icon_color(text_muted())
                        .flex_shrink_0()
                        .size_5()
                        .centered(true)
                        .on_click(move |_e, _w, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(h.clone()));
                        }),
                ),
        )
        .into_any_element()
}

/// How many rows of a result table the card shows before the "…more" note —
/// the card sits inside a scrollable list; five hundred rows would push
/// everything else out of the viewport.
const TOOL_TABLE_MAX_ROWS: usize = 60;

/// A native table for a list-style tool result. Header row muted; cells in
/// the mono face (ids, paths and addresses are what these tables carry);
/// overflowing rows collapse into a muted "…more" note.
fn tool_result_table(table: &ToolTable, base_id: String) -> AnyElement {
    let mut body = div().flex().flex_col().gap_1().w_full().child(
        div()
            .flex()
            .flex_row()
            .gap_2()
            .pb_1()
            .border_b_1()
            .border_color(rgb(border()))
            .children(table.headers.iter().map(|header| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(header.clone())
            })),
    );
    for (ix, row) in table.rows.iter().take(TOOL_TABLE_MAX_ROWS).enumerate() {
        body = body.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .id(ElementId::Name(format!("{base_id}-row-{ix}").into()))
                .children(row.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(rgb(text_primary()))
                        .font_family(TerminalView::mono_font_family())
                        .child(cell.clone())
                })),
        );
    }
    let more = table.rows.len().saturating_sub(TOOL_TABLE_MAX_ROWS);
    if more > 0 {
        body = body.child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("ai_panel.table_more", more = more.to_string()).to_string()),
        );
    }
    div()
        .w_full()
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(rgb(border()))
        .bg(rgb(surface_hover()))
        .p_2()
        .child(body)
        .into_any_element()
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

/// The compacted-history marker: a dim one-liner where the summarized turns
/// used to be. The whole header row is the click target (same affordance as
/// the thinking block) and the summary itself is one click away, so the
/// transcript stays honest about what happened to the history.
fn summary_block(
    ix: usize,
    text: String,
    expanded: bool,
    entity: &WeakEntity<AiPanel>,
    md_style: &TextViewStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let h = entity.clone();
    let mut col = div().flex().flex_col().gap_1().w_full().child(
        div()
            .id(ElementId::Name(format!("ai-summary-header-{ix}").into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .on_click(move |_e, _w, cx| {
                let _ = h.update(cx, |panel, cx| panel.toggle_summary(ix, cx));
            })
            .child(
                svg()
                    .path("icons/history.svg")
                    .size(px(12.0))
                    .text_color(rgb(text_muted())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(t!("ai_panel.context_summary").to_string()),
            ),
    );
    if expanded {
        col = col.child(
            div()
                .ml_1p5()
                .pl_3()
                .border_l_1()
                .border_color(rgb(border()))
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(
                    TextView::markdown(
                        ElementId::Name(format!("ai-summary-{ix}").into()),
                        text,
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
    if reasoning.is_empty() && content.is_empty() {
        // Nothing streamed yet — a lone spinning loader, so the turn visibly
        // starts moving from the moment the request goes out (and not only
        // when the first token lands).
        return col
            .child(loading_spinner("ai-respond-spinner", 12.0, text_muted()))
            .into_any_element();
    }
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
