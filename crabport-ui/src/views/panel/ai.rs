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
    AiError, ChatMessage, ChatRequest, ChatResponse, ChatStream, MESSAGE_OVERHEAD_TOKENS,
    StreamEvent, TOOL_CALL_OVERHEAD_TOKENS, ToolCall, ToolSpec, estimate_tokens,
};
use crabport_core::config;
use crabport_ssh::{TunnelId, TunnelKind, TunnelManager, TunnelStatus};
use crabport_terminal::terminal::{BackendEvent, ExecCancel, ExecOutput, SftpTransferKind};
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
/// The agent prompt: the model may inspect and drive *its own* terminal and
/// this machine, but nothing happens without the user approving it. Keep the
/// rules short and concrete — the tools themselves carry the detail.
const BASE_PROMPT: &str = "\
You are the built-in AI assistant of CrabPort, an SSH/SFTP client, working \
inside one terminal session. Be concise and practical.\n\n\
Every tool call is shown to the user for approval, so call a tool only when \
it earns its place: say why you are reading, and what you expect a call to \
do. Never batch speculative calls — one call, then read the result. When \
the user asks for something you can answer without a tool, just answer.\n\n\
Terminal tools — this session's shell only:\n\
- terminal_read: read the most recent output lines. Use it before asking \
questions the screen can answer, and after a visible command to see what \
happened.\n\
- terminal_exec (your default executor): run one command out of band and get \
its captured stdout, stderr and exit code directly. It does not appear in \
the user's terminal and cannot answer prompts.\n\
- terminal_run: type one command into the user's live terminal, exactly as \
if they typed it, so its output streams where they can watch it. Use it only \
when the output must be followed while it runs or the command is interactive \
(REPLs, anything that may prompt for input such as sudo).\n\n\
Local tools — the machine CrabPort itself runs on, never the remote host:\n\
- read_file: read one local file.\n\
- read_directory: list one local directory.\n\n\
Network:\n\
- fetch: send one HTTP request from inside CrabPort, returning status, \
headers and body. It runs on the local machine's network, not the remote \
host's; pass a proxy URL to route it elsewhere — typically a dynamic tunnel \
you created and opened on this session, as socks5h://127.0.0.1:<port> (the \
h matters: the hostname is then resolved on the remote side).\n\n\
Tunnel tools — the current tab's SSH connection only (tunnels ride it; no \
extra connection is opened; unavailable on local, telnet and serial \
terminals):\n\
- tunnel_list: see the tunnels available here - ids for tunnels you \
created here start with a-N, Tunnels-page configs on this host start \
with t-N; both work with open / close / delete.\n\
- tunnel_create: define a temporary, session-local tunnel (local / dynamic \
/ remote, not started; never persisted); \
tunnel_open starts it and reports the address it listens on, tunnel_close \
stops it, tunnel_delete removes it.\n\
SFTP tools — this session's connection only (unavailable without SFTP):\n\
- sftp_list: list a remote directory — the default way to look at the \
remote filesystem (this also moves the SFTP panel's view there). When this \
session has no SFTP, look at files with shell commands instead \
(terminal_exec: ls, find, du, …).\n\
- sftp_download / sftp_upload: transfer one file between the remote host \
and the local machine.\n\n\
Safety: when a call could have destructive or otherwise high-risk effects — \
deleting or overwriting data, changing system or service configuration, \
touching production, anything hard to undo — repeat it in your reply \
**in bold** and say plainly what could go wrong, so the user cannot miss it \
while approving.";

/// Platform note appended to the prompt, so the agent writes local paths the
/// way the machine CrabPort runs on expects — on Windows that means `C:\...`
/// style paths for read_file / read_directory and the local side of the SFTP
/// transfers. Conditionally compiled: the note exists only when it is true.
#[cfg(windows)]
const PLATFORM_NOTE: &str = "\n\nNote: CrabPort itself is running on Windows, so \
every *local* path you use — read_file, read_directory, and the local side \
of sftp_download / sftp_upload — must be Windows-style, e.g. C:\\Users\\... . \
Remote paths follow the remote host's operating system.";
#[cfg(not(windows))]
const PLATFORM_NOTE: &str = "\n\nNote: CrabPort itself is running on a Unix-like \
system, so every *local* path you use — read_file, read_directory, and the \
local side of sftp_download / sftp_upload — is POSIX-style, e.g. /home/... . \
Remote paths follow the remote host's operating system.";

/// The pinned system prompt: base rules plus the platform note.
fn system_prompt() -> String {
    format!("{BASE_PROMPT}{PLATFORM_NOTE}")
}

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
        ToolSpec::new(
            "read_file",
            "Read one file on the machine CrabPort itself runs on — the local \
             machine, never the remote host. Use it for files the user \
             mentions, or that a fetch/command produced locally.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Local path of the file to read."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why you need it — shown to the user for approval."
                    }
                },
                "required": ["path", "purpose"]
            }),
        ),
        ToolSpec::new(
            "read_directory",
            "List one directory on the machine CrabPort itself runs on — the \
             local machine, never the remote host. Returns names, types and \
             sizes, not file contents.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Local path of the directory to list."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why you need it — shown to the user for approval."
                    }
                },
                "required": ["path", "purpose"]
            }),
        ),
        ToolSpec::new(
            "fetch",
            "Send one HTTP request from inside CrabPort and return the status, \
             response headers and body. It runs on the local machine's \
             network, not the remote host's — pass a proxy URL to route it \
             elsewhere. To reach an address only the remote host can reach, \
             first create and open a dynamic tunnel on this session, then pass \
             its proxy as socks5h://127.0.0.1:<port> (the h matters: the \
             hostname is then resolved on the remote side).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Absolute URL to request."
                    },
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST", "PUT", "DELETE", "HEAD"],
                        "description": "HTTP method; defaults to GET."
                    },
                    "body": {
                        "type": "string",
                        "description": "Request body, for POST/PUT."
                    },
                    "proxy": {
                        "type": "string",
                        "description": "Optional proxy URL, e.g. socks5h://127.0.0.1:1080 from a dynamic tunnel."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect this request to do — shown to the user for approval."
                    }
                },
                "required": ["url", "expected"]
            }),
        ),
        ToolSpec::new(
            "tunnel_create",
            "Define a *temporary* tunnel on the current tab's SSH connection \
             (not started yet; no extra connection is opened — tunnels ride \
             the tab's session). It lives only in this conversation: it is \
             never persisted, the Tunnels page never shows it, and it is gone \
             when this terminal closes. For a persistent tunnel, ask the user \
             to create it in the Tunnels page instead. Kinds: local forwards a \
             local port to a remote target; dynamic opens a SOCKS5 proxy on a \
             local port (no target — each client picks the destination per \
             connection); remote asks the remote server to listen and forward \
             back to this machine.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["local", "dynamic", "remote"],
                        "description": "Tunnel type."
                    },
                    "name": {
                        "type": "string",
                        "description": "Short name for the tunnel."
                    },
                    "bind_port": {
                        "type": "integer",
                        "description": "Port to listen on; 0 picks a free one."
                    },
                    "target_host": {
                        "type": "string",
                        "description": "Forward target host — required for local/remote, ignored for dynamic."
                    },
                    "target_port": {
                        "type": "integer",
                        "description": "Forward target port — required for local/remote, ignored for dynamic."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect the tunnel to be used for — shown to the user for approval."
                    }
                },
                "required": ["kind", "name", "expected"]
            }),
        ),
        ToolSpec::new(
            "tunnel_list",
            "List the tunnels available on this tab: the ones you created here \
             (ids starting with a), plus the Tunnels-page configs bound to this \
             tab's host (ids starting with t) — running or stopped. Both id \
             kinds work with tunnel_open / tunnel_close / tunnel_delete.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        ),
        ToolSpec::new(
            "tunnel_open",
            "Start a tunnel and get back the address it listens on. Use an id \
             token from tunnel_list / tunnel_create: `a…` is a tunnel you \
             created in this session, `t…` is a Tunnels-page config bound to \
             this host (opening it borrows the tab's SSH connection).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tunnel_id": {
                        "type": "string",
                        "description": "Id token as returned by tunnel_list / tunnel_create (a… or t…)."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect this to do — shown to the user for approval."
                    }
                },
                "required": ["tunnel_id", "expected"]
            }),
        ),
        ToolSpec::new(
            "tunnel_close",
            "Stop a running tunnel. Its port stops listening; existing \
             connections are torn down. Id tokens from tunnel_list: `a…` for \
             tunnels you created, `t…` for Tunnels-page configs on this host.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tunnel_id": {
                        "type": "string",
                        "description": "Id token as returned by tunnel_list / tunnel_create (a… or t…)."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect this to do — shown to the user for approval."
                    }
                },
                "required": ["tunnel_id", "expected"]
            }),
        ),
        ToolSpec::new(
            "tunnel_delete",
            "Delete a tunnel — stopping it first when it is running. `a…` \
             tokens delete tunnels you created here; `t…` tokens delete the \
             Tunnels-page config itself.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tunnel_id": {
                        "type": "string",
                        "description": "Id token as returned by tunnel_list / tunnel_create (a… or t…)."
                    },
                    "expected": {
                        "type": "string",
                        "description": "What you expect this to do — shown to the user for approval."
                    }
                },
                "required": ["tunnel_id", "expected"]
            }),
        ),
        ToolSpec::new(
            "sftp_list",
            "List one directory on the remote host over this session's SFTP \
             connection: names, types, sizes. Note that this also moves the \
             SFTP panel's view to that directory.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Remote directory to list."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why you need it — shown to the user for approval."
                    }
                },
                "required": ["path", "purpose"]
            }),
        ),
        ToolSpec::new(
            "sftp_download",
            "Download one file from the remote host to a local path, over this \
             session's SFTP connection. The local path lives on the machine \
             CrabPort runs on.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "remote_path": {
                        "type": "string",
                        "description": "Path of the file on the remote host."
                    },
                    "local_path": {
                        "type": "string",
                        "description": "Destination path on the local machine."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why you need it — shown to the user for approval."
                    }
                },
                "required": ["remote_path", "local_path", "purpose"]
            }),
        ),
        ToolSpec::new(
            "sftp_upload",
            "Upload one local file to the remote host, over this session's \
             SFTP connection. The local path lives on the machine CrabPort \
             runs on.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "local_path": {
                        "type": "string",
                        "description": "Path of the file on the local machine."
                    },
                    "remote_path": {
                        "type": "string",
                        "description": "Destination path on the remote host."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why you need it — shown to the user for approval."
                    }
                },
                "required": ["local_path", "remote_path", "purpose"]
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
    TerminalRead,
    /// `terminal_exec` — run out of band, output captured directly.
    ExecCapture,
    /// `terminal_run` — typed into the live terminal, output read from the
    /// screen once it settles.
    Run,
    /// `fetch` — one HTTP request from inside CrabPort, optional proxy.
    Fetch,
    /// `read_file` / `read_directory` — the local filesystem.
    LocalFs,
    /// `tunnel_create` / `tunnel_open` / `tunnel_close` / `tunnel_delete`.
    Tunnel,
    /// `sftp_list` / `sftp_download` / `sftp_upload`.
    Sftp,
    /// Anything else the model invented; refused with a readable error.
    Unknown,
}

impl ToolKind {
    fn of(name: &str) -> Self {
        match name {
            "terminal_read" => Self::TerminalRead,
            "terminal_exec" => Self::ExecCapture,
            "terminal_run" => Self::Run,
            "fetch" => Self::Fetch,
            "read_file" | "read_directory" => Self::LocalFs,
            name if name.starts_with("tunnel_") => Self::Tunnel,
            name if name.starts_with("sftp_") => Self::Sftp,
            _ => Self::Unknown,
        }
    }
}

/// The card's title for one tool.
fn tool_title(name: &str) -> String {
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

/// One tunnel the agent defined through this terminal's connection.
///
/// Agent-facing ids are stable and never reused (a delete keeps its slot),
/// so a `tunnel_open` the model issues later cannot hit the wrong tunnel.
/// The tunnels live in the panel's own [`TunnelManager`] — scoped to this
/// terminal's connection, invisible to every other tab.
#[derive(Clone)]
struct AgentTunnel {
    agent_id: u64,
    kind: TunnelKind,
    name: String,
    bind_addr: String,
    bind_port: u16,
    target_host: String,
    target_port: u16,
    state: AgentTunnelState,
}

/// Lifecycle of an agent-created tunnel.
#[derive(Clone)]
enum AgentTunnelState {
    /// Defined, not started.
    Created,
    /// Started; carries the manager's tunnel id.
    Running(TunnelId),
    /// Stopped — the manager's entry is gone. Can be opened again.
    Closed,
    /// The last open attempt failed, with the reason.
    Failed(String),
    /// Deleted. Kept in the list so earlier ids stay valid.
    Deleted,
}

impl AgentTunnel {
    /// The token the agent refers to this tunnel by — `a<agent_id>`, so it
    /// can never collide with a Tunnels-page config id (`t<db id>`).
    fn handle(&self) -> String {
        format!("a{}", self.agent_id)
    }

    /// The full address pair of this tunnel — `bind -> target` for local and
    /// remote tunnels (the model needs both ends), just the bind port for
    /// dynamic ones (whose target is picked per connection). `0` as a bind
    /// port means "unused / pick a free one at open".
    fn addresses(&self) -> String {
        let mut out = format!("{}:{}", self.bind_addr, self.bind_port);
        if self.kind != TunnelKind::Dynamic && !self.target_host.is_empty() && self.target_port != 0
        {
            out.push_str(&format!(" -> {}:{}", self.target_host, self.target_port));
        }
        out
    }

    /// One-line description for cards and results.
    fn describe(&self, manager: Option<&Arc<TunnelManager>>) -> String {
        let base = format!("{} {} ({})", self.handle(), self.name, self.kind.as_str());
        match &self.state {
            AgentTunnelState::Created => {
                format!("{base} — created, not started ({})", self.addresses())
            }
            AgentTunnelState::Closed => format!("{base} — closed ({})", self.addresses()),
            AgentTunnelState::Deleted => format!("{base} — deleted ({})", self.addresses()),
            AgentTunnelState::Failed(reason) => {
                format!("{base} — open failed: {reason} ({})", self.addresses())
            }
            AgentTunnelState::Running(id) => match manager.and_then(|m| m.get(*id)) {
                Some(info) => {
                    let mut text = format!(
                        "{base} — {}",
                        t!(
                            "ai_panel.tunnel_running_at",
                            bind = format!("{}:{}", info.bind_addr, info.bind_port)
                        )
                    );
                    if !info.target_host.is_empty() && info.target_port != 0 {
                        text.push_str(&format!(" -> {}:{}", info.target_host, info.target_port));
                    }
                    text
                }
                None => format!("{base} — running ({})", self.addresses()),
            },
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
    /// Token that stops the running execution — the card's stop button
    /// flips it. A fresh token per call, harmless once the call is done.
    cancel: ExecCancel,
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
                _ => "[stopped waiting; the command may still be running in the terminal]",
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

/// How long a captured (`terminal_exec`) command may run before its backend
/// gives up and hands over the output so far. Generous enough for a normal
/// non-interactive command, bounded so a `tail -f`-style slip-up cannot
/// wedge the conversation forever.
const EXEC_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Most bytes a `read_file` loads before the result is capped for the model.
/// Big enough for any source file or config, small enough that pointing the
/// tool at a video cannot stall the panel.
const READ_FILE_LIMIT: u64 = 256 * 1024;

/// Most entries a `read_directory` lists before it says there were more.
const READ_DIR_LIMIT: usize = 500;

/// `fetch`'s overall HTTP timeout — a hung request must not wedge the
/// conversation any more than a hung command may.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long `sftp_list` waits for the backend's navigation to land before it
/// reports the directory as unreadable (the backend logs failures silently).
/// The first listing on a session also pays the SFTP subsystem handshake, so
/// this is generous.
const SFTP_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long an SFTP transfer may run before the tool call reports what has
/// happened so far. Long — transfers can genuinely take minutes — but bounded.
const SFTP_TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a tunnel's start may stay in "starting" before `tunnel_open`
/// reports it failed.
const TUNNEL_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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

/// Longest single tool result fed into a compaction transcript, in bytes.
/// The transcript is the *input* of the summarizer, so a handful of huge
/// command outputs must not blow up the very request meant to shrink the
/// conversation; `compaction_transcript` keeps the tail of a longer result,
/// where a command's errors and summary usually are.
const COMPACT_RESULT_CAP: usize = 4 * 1024;

/// Instruction for the compaction round: what the summary must preserve so
/// the work can continue without the original messages.
const COMPACTION_PROMPT: &str = "\
You maintain the working memory of a terminal-operating agent. Summarize the \
conversation transcript below into notes for yourself, so the work can \
continue without the original messages. Preserve: the user's goals and \
constraints (and anything they asked you to remember), decisions made, the \
commands that ran with their key output (paths, errors, numbers), the state \
of anything still running, and open questions. Be specific and terse — no \
pleasantries, no restating the obvious. Reply with the summary only, as \
Markdown.";

/// Preamble for a compaction summary in the wire history. Spelled out so
/// the model reads it as context rather than as a fresh instruction.
const COMPACTED_HEADER: &str =
    "Summary of the earlier conversation, compacted to save context:\n\n";

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
    /// The app's tunnel registry — lets `tunnel_list` also report the
    /// tunnels the Tunnels page started *borrowing this terminal's*
    /// connection. Read-only here: the agent manages only the tunnels it
    /// created itself.
    pub tunnels: Arc<crate::views::tunnels::TunnelRegistry>,
    /// The owning app entity — registry tunnels (`t…` ids) are started,
    /// stopped and deleted through the app's own machinery so the Tunnels
    /// page and the store stay consistent.
    pub app: WeakEntity<crate::app::CrabportApp>,
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
    /// This terminal's tunnel manager — created from the terminal's SSH
    /// source on first use, so every tunnel tool acts only on tunnels this
    /// panel started. `None` on terminals without an SSH source.
    tunnel_manager: Option<Arc<TunnelManager>>,
    /// Tunnels the agent defined, in creation order. Agent-facing ids index
    /// into this list (stable, never reused).
    agent_tunnels: Vec<AgentTunnel>,
    /// Next agent-facing tunnel id (1-based).
    next_tunnel_id: u64,
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
            tunnel_manager: None,
            agent_tunnels: Vec::new(),
            next_tunnel_id: 1,
            model_windows: HashMap::new(),
            context_used: 0,
            compaction: None,
            status: None,
            scroll_to_end_pending: false,
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

        self.pending_clear = true;
        // Compacts first when the request would be too large, then sends.
        self.continue_after_tools(cx);
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
        // The turn changed the history; keep the context chip honest.
        self.refresh_context_used();
        cx.notify();
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
        let terminal = self.session.terminal.upgrade();
        let (terminal_closed, exec_ok, tunnel_ok, sftp_ok) = match &terminal {
            None => (true, false, false, false),
            Some(view) => {
                let view = view.read(cx);
                (
                    false,
                    view.allow_exec_capture(),
                    view.tunnel_source().is_some(),
                    view.allow_sftp(),
                )
            }
        };
        let blocked = |name: &str| -> Option<String> {
            if terminal_closed {
                return Some(t!("ai_panel.tool_terminal_closed").to_string());
            }
            match ToolKind::of(name) {
                ToolKind::ExecCapture if !exec_ok => {
                    Some(t!("ai_panel.tool_exec_unsupported").to_string())
                }
                ToolKind::Tunnel if !tunnel_ok => Some(t!("ai_panel.tool_no_tunnel").to_string()),
                ToolKind::Sftp if !sftp_ok => Some(t!("ai_panel.tool_no_sftp").to_string()),
                _ => None,
            }
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
                    cancel: ExecCancel::default(),
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
            ToolKind::TerminalRead => {
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
            ToolKind::Fetch => self.start_fetch(msg_index, call_index, cx),
            ToolKind::LocalFs => self.start_local_fs(&call, msg_index, call_index, cx),
            ToolKind::Tunnel => self.start_tunnel_op(&call, msg_index, call_index, cx),
            ToolKind::Sftp => self.start_sftp_op(&call, msg_index, call_index, cx),
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
        self.refresh_context_used();
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
        // The token the card's stop button flips; the backend then reports
        // the output captured up to that moment.
        let cancel = ExecCancel::default();
        let rx = terminal
            .read(cx)
            .exec_capture(&effective, EXEC_CAPTURE_TIMEOUT, cancel.clone());

        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.decision = ToolDecision::Allowed;
            state.running = true;
            state.cancel = cancel;
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
        // The card's stop button ends the *wait* — the command itself keeps
        // running in the user's terminal, which is where they can interrupt
        // it for real.
        let cancel = ExecCancel::default();
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.decision = ToolDecision::Allowed;
            state.running = true;
            state.cancel = cancel.clone();
        }
        // Same path the snippets panel uses: the line, then a carriage
        // return, so the shell runs it as if it had been typed.
        let mut bytes = command.into_bytes();
        bytes.push(b'\r');
        terminal.read(cx).write_raw(&bytes);
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        self.await_command_output(msg_index, call_index, before, cancel, cx);
        cx.notify();
    }

    /// Mark an approved call as running: the card turns from buttons into a
    /// running state.
    fn mark_running(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.decision = ToolDecision::Allowed;
            state.running = true;
        }
        self.remeasure_row(self.list_row_of_tool_call(msg_index, call_index));
        cx.notify();
    }

    /// Run an approved `fetch` off the UI thread: CrabPort's own HTTP client,
    /// optionally through a proxy URL.
    fn start_fetch(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        let args = self
            .tool_state_mut(msg_index, call_index)
            .and_then(|state| state.args.clone());
        let arg_str = |key: &str| -> Option<String> {
            args.as_ref()
                .and_then(|args| args.get(key)?.as_str())
                .map(str::to_string)
        };
        let Some(url) = arg_str("url") else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_bad_args").to_string(),
                ToolDecision::Denied,
                cx,
            );
            return;
        };
        let proxy = arg_str("proxy");
        let method = arg_str("method")
            .map(|method| method.to_ascii_uppercase())
            .unwrap_or_else(|| "GET".to_string());
        let body = arg_str("body");

        self.mark_running(msg_index, call_index, cx);
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let result =
                smol::unblock(move || fetch_url(&url, proxy.as_deref(), &method, body.as_deref()))
                    .await;
            let _ = entity.update(cx, |panel, cx| {
                panel.settle_tool_result(msg_index, call_index, result, ToolDecision::Allowed, cx);
            });
        })
        .detach();
    }

    /// Read an approved local file / directory off the UI thread.
    fn start_local_fs(
        &mut self,
        call: &ToolCall,
        msg_index: usize,
        call_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .tool_state_mut(msg_index, call_index)
            .and_then(|state| state.arg_str("path").map(str::to_string))
        else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_bad_args").to_string(),
                ToolDecision::Denied,
                cx,
            );
            return;
        };
        self.mark_running(msg_index, call_index, cx);
        let is_file = call.name == "read_file";
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let result = smol::unblock(move || {
                if is_file {
                    read_local_file(&path)
                } else {
                    read_local_directory(&path)
                }
            })
            .await;
            let _ = entity.update(cx, |panel, cx| {
                panel.settle_tool_result(msg_index, call_index, result, ToolDecision::Allowed, cx);
            });
        })
        .detach();
    }

    /// This terminal's tunnel manager — created from the terminal's SSH
    /// source on first use, so every tunnel tool acts only on tunnels this
    /// panel started. `None` when the terminal has no SSH connection.
    fn tunnel_manager(&mut self, cx: &mut Context<Self>) -> Option<Arc<TunnelManager>> {
        if self.tunnel_manager.is_none() {
            let source = self
                .session
                .terminal
                .upgrade()?
                .read(cx)
                .tunnel_source()
                .cloned()?;
            self.tunnel_manager = Some(Arc::new(TunnelManager::new(source, Arc::new(|| {}))));
        }
        self.tunnel_manager.clone()
    }

    /// Handle one approved tunnel tool call: create (define), open (start),
    /// close (stop) and delete (stop + retire). Everything runs against the
    /// panel's own manager — never another terminal's connection.
    fn start_tunnel_op(
        &mut self,
        call: &ToolCall,
        msg_index: usize,
        call_index: usize,
        cx: &mut Context<Self>,
    ) {
        let args = self
            .tool_state_mut(msg_index, call_index)
            .and_then(|state| state.args.clone());
        let arg_u64 = |key: &str| args.as_ref().and_then(|args| args.get(key)?.as_u64());
        let arg_str = |key: &str| -> Option<String> {
            args.as_ref()
                .and_then(|args| args.get(key)?.as_str())
                .map(str::to_string)
        };

        match call.name.as_str() {
            "tunnel_list" => {
                // The agent's own tunnels first (their ids are what
                // open/close/delete take), then everything configured for
                // this terminal's host — however it runs: owned from the
                // Tunnels page, borrowed from this tab, or stopped. Those are
                // managed from the Tunnels page; the agent only reports them.
                let manager = self.tunnel_manager.as_ref();
                let mine: Vec<String> = self
                    .agent_tunnels
                    .iter()
                    .map(|tunnel| tunnel.describe(manager))
                    .collect();
                let mut lines: Vec<String> = Vec::new();
                if !mine.is_empty() {
                    lines.push(t!("ai_panel.tool_tunnel_list_mine").to_string());
                    lines.extend(mine);
                }
                let host_id = self
                    .session
                    .terminal
                    .upgrade()
                    .and_then(|view| view.read(cx).host_id());
                if let Some(host_id) = host_id {
                    let configured: Vec<String> = self
                        .session
                        .tunnels
                        .list()
                        .into_iter()
                        .filter(|view| view.host_id == host_id)
                        .map(|view| {
                            let source = if view.borrowed_tab_id.is_some() {
                                t!("ai_panel.tunnel_source_borrowed")
                            } else {
                                t!("ai_panel.tunnel_source_owned")
                            };
                            // The id token leads the line — it is what
                            // tunnel_open / tunnel_close / tunnel_delete take.
                            // Both ends of a local/remote tunnel follow: the
                            // model needs host *and* port to use or talk
                            // about the tunnel.
                            let mut line = format!(
                                "t{} {} ({}, {})",
                                view.id,
                                view.name,
                                view.kind.as_str(),
                                source
                            );
                            let mut addresses = format!("{}:{}", view.bind_addr, view.bind_port);
                            if !view.target_host.is_empty() && view.target_port != 0 {
                                addresses.push_str(&format!(
                                    " -> {}:{}",
                                    view.target_host, view.target_port
                                ));
                            }
                            if view.running {
                                line.push_str(&format!(
                                    " — {}",
                                    t!("ai_panel.tunnel_running_at", bind = addresses)
                                ));
                            } else {
                                line.push_str(&format!(
                                    " — {}",
                                    t!("ai_panel.tunnel_stopped_at", bind = addresses)
                                ));
                            }
                            line
                        })
                        .collect();
                    if !configured.is_empty() {
                        lines.push(t!("ai_panel.tool_tunnel_list_host").to_string());
                        lines.extend(configured);
                    }
                }
                let text = if lines.is_empty() {
                    t!("ai_panel.tool_tunnel_list_empty").to_string()
                } else {
                    lines.join("\n")
                };
                self.settle_tool_result(msg_index, call_index, text, ToolDecision::Allowed, cx);
            }
            "tunnel_create" => {
                let kind = TunnelKind::from_str(arg_str("kind").as_deref().unwrap_or("dynamic"));
                let name = arg_str("name")
                    .unwrap_or_else(|| format!("agent-tunnel-{}", self.next_tunnel_id));
                let bind_port = arg_u64("bind_port").unwrap_or(0) as u16;
                let bind_addr = arg_str("bind_addr").unwrap_or_else(|| "127.0.0.1".to_string());
                let target_host = arg_str("target_host").unwrap_or_default();
                let target_port = arg_u64("target_port").unwrap_or(0) as u16;
                // local/remote need a target: refuse the malformed definition
                // instead of keeping something that can never start.
                if matches!(kind, TunnelKind::Local | TunnelKind::Remote)
                    && (target_host.is_empty() || target_port == 0)
                {
                    self.settle_tool_result(
                        msg_index,
                        call_index,
                        t!("ai_panel.tool_bad_args").to_string(),
                        ToolDecision::Denied,
                        cx,
                    );
                    return;
                }
                let agent_id = self.next_tunnel_id;
                self.next_tunnel_id += 1;
                self.agent_tunnels.push(AgentTunnel {
                    agent_id,
                    kind,
                    name,
                    bind_addr,
                    bind_port,
                    target_host,
                    target_port,
                    state: AgentTunnelState::Created,
                });
                let result =
                    t!("ai_panel.tool_tunnel_created", id = agent_id.to_string()).to_string();
                self.settle_tool_result(msg_index, call_index, result, ToolDecision::Allowed, cx);
                let handle = format!("a{}", agent_id);
                let result = t!("ai_panel.tool_tunnel_created", id = handle).to_string();
                self.settle_tool_result(msg_index, call_index, result, ToolDecision::Allowed, cx);
            }
            "tunnel_open" | "tunnel_close" | "tunnel_delete" => {
                // The id token decides where the tunnel lives: `a<N>` is one
                // the agent created here, `t<N>` a Tunnels-page config bound
                // to this terminal's host.
                let Some(token) = arg_str("tunnel_id") else {
                    self.settle_tool_result(
                        msg_index,
                        call_index,
                        t!("ai_panel.tool_bad_args").to_string(),
                        ToolDecision::Denied,
                        cx,
                    );
                    return;
                };
                let (kind, number) = (
                    token.chars().next().unwrap_or(' '),
                    token[1..].parse::<u64>().ok(),
                );
                match (kind, number) {
                    ('a', Some(agent_id)) => {
                        let Some(spec_ix) = self
                            .agent_tunnels
                            .iter()
                            .position(|tunnel| tunnel.agent_id == agent_id)
                        else {
                            self.settle_tool_result(
                                msg_index,
                                call_index,
                                t!("ai_panel.tool_tunnel_unknown", id = token.as_str()).to_string(),
                                ToolDecision::Denied,
                                cx,
                            );
                            return;
                        };
                        match call.name.as_str() {
                            "tunnel_open" => self.tunnel_open(spec_ix, msg_index, call_index, cx),
                            "tunnel_close" => self.tunnel_close(spec_ix, msg_index, call_index, cx),
                            "tunnel_delete" => {
                                self.tunnel_delete(spec_ix, msg_index, call_index, cx)
                            }
                            _ => {}
                        }
                    }
                    ('t', Some(number)) => {
                        self.tunnel_registry_op(
                            call.name.as_str(),
                            number as i64,
                            msg_index,
                            call_index,
                            cx,
                        );
                    }
                    _ => {
                        self.settle_tool_result(
                            msg_index,
                            call_index,
                            t!("ai_panel.tool_tunnel_unknown", id = token.as_str()).to_string(),
                            ToolDecision::Denied,
                            cx,
                        );
                    }
                }
            }
            other => {
                self.settle_tool_result(
                    msg_index,
                    call_index,
                    t!("ai_panel.tool_unknown", name = other).to_string(),
                    ToolDecision::Denied,
                    cx,
                );
            }
        }
    }

    /// Start a defined tunnel and report it live: the model cannot use the
    /// port until it is listening, so the call waits for the manager to
    /// report Active (or the failure) before settling.
    fn tunnel_open(
        &mut self,
        spec_ix: usize,
        msg_index: usize,
        call_index: usize,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.agent_tunnels[spec_ix].state,
            AgentTunnelState::Running(_)
        ) {
            let text = t!("ai_panel.tool_tunnel_already").to_string();
            self.settle_tool_result(msg_index, call_index, text, ToolDecision::Allowed, cx);
            return;
        }
        let Some(manager) = self.tunnel_manager(cx) else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_no_tunnel").to_string(),
                ToolDecision::Unavailable,
                cx,
            );
            return;
        };
        let (kind, name, bind_addr, bind_port, target_host, target_port) = {
            let spec = &self.agent_tunnels[spec_ix];
            (
                spec.kind,
                spec.name.clone(),
                spec.bind_addr.clone(),
                spec.bind_port,
                spec.target_host.clone(),
                spec.target_port,
            )
        };
        let agent_id = self.agent_tunnels[spec_ix].agent_id;
        self.mark_running(msg_index, call_index, cx);
        let entity = cx.entity().downgrade();
        let manager_for_start = manager.clone();
        cx.spawn(async move |_this, cx| {
            let outcome = smol::unblock(move || {
                let start = async {
                    match kind {
                        TunnelKind::Dynamic => {
                            manager_for_start
                                .start_dynamic(name, bind_addr, bind_port)
                                .await
                        }
                        TunnelKind::Local => {
                            manager_for_start
                                .start_local(name, bind_addr, bind_port, target_host, target_port)
                                .await
                        }
                        TunnelKind::Remote => {
                            manager_for_start
                                .start_remote(name, bind_addr, bind_port, target_host, target_port)
                                .await
                        }
                    }
                };
                crabport_ssh::TOKIO.block_on(start)
            })
            .await;
            let result = match outcome {
                Ok(tunnel_id) => {
                    let started_at = std::time::Instant::now();
                    loop {
                        match manager.get(tunnel_id).map(|info| info.status) {
                            Some(TunnelStatus::Active) => break Ok(tunnel_id),
                            Some(TunnelStatus::Failed(reason)) => break Err(reason),
                            Some(TunnelStatus::Closed) => {
                                break Err("tunnel closed while starting".to_string());
                            }
                            _ if started_at.elapsed() >= TUNNEL_START_TIMEOUT => {
                                break Err(format!(
                                    "still starting after {}s",
                                    TUNNEL_START_TIMEOUT.as_secs()
                                ));
                            }
                            _ => {
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(100))
                                    .await;
                            }
                        }
                    }
                }
                Err(reason) => Err(reason),
            };
            let _ = entity.update(cx, |panel, cx| {
                let Some(spec) = panel
                    .agent_tunnels
                    .iter_mut()
                    .find(|tunnel| tunnel.agent_id == agent_id)
                else {
                    return;
                };
                match &result {
                    Ok(tunnel_id) => spec.state = AgentTunnelState::Running(*tunnel_id),
                    Err(reason) => spec.state = AgentTunnelState::Failed(reason.clone()),
                }
                let text = match &result {
                    Ok(_) => spec.describe(panel.tunnel_manager.as_ref()),
                    Err(reason) => format!("{}: {reason}", t!("ai_panel.tool_tunnel_open_failed")),
                };
                panel.settle_tool_result(msg_index, call_index, text, ToolDecision::Allowed, cx);
            });
        })
        .detach();
    }

    /// Stop one running tunnel. The manager's entry is removed; the spec
    /// stays and can be opened again.
    fn tunnel_close(
        &mut self,
        spec_ix: usize,
        msg_index: usize,
        call_index: usize,
        cx: &mut Context<Self>,
    ) {
        let agent_id = self.agent_tunnels[spec_ix].agent_id;
        let running = match &self.agent_tunnels[spec_ix].state {
            AgentTunnelState::Running(tunnel_id) => Some(*tunnel_id),
            _ => None,
        };
        let Some((tunnel_id, manager)) = running.zip(self.tunnel_manager(cx)) else {
            let text = t!(
                "ai_panel.tool_tunnel_not_running",
                id = agent_id.to_string()
            )
            .to_string();
            self.settle_tool_result(msg_index, call_index, text, ToolDecision::Allowed, cx);
            return;
        };
        self.mark_running(msg_index, call_index, cx);
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            smol::unblock(move || crabport_ssh::TOKIO.block_on(manager.stop(tunnel_id))).await;
            let _ = entity.update(cx, |panel, cx| {
                if let Some(spec) = panel
                    .agent_tunnels
                    .iter_mut()
                    .find(|tunnel| tunnel.agent_id == agent_id)
                {
                    spec.state = AgentTunnelState::Closed;
                }
                panel.settle_tool_result(
                    msg_index,
                    call_index,
                    t!("ai_panel.tool_tunnel_closed", id = agent_id.to_string()).to_string(),
                    ToolDecision::Allowed,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Delete one tunnel: stop it first when it is running, then retire the
    /// spec (the slot stays so earlier ids remain valid).
    fn tunnel_delete(
        &mut self,
        spec_ix: usize,
        msg_index: usize,
        call_index: usize,
        cx: &mut Context<Self>,
    ) {
        let agent_id = self.agent_tunnels[spec_ix].agent_id;
        let running = match &self.agent_tunnels[spec_ix].state {
            AgentTunnelState::Running(tunnel_id) => Some(*tunnel_id),
            _ => None,
        };
        if let (Some(tunnel_id), Some(manager)) = (running, self.tunnel_manager(cx)) {
            self.mark_running(msg_index, call_index, cx);
            let entity = cx.entity().downgrade();
            cx.spawn(async move |_this, cx| {
                smol::unblock(move || crabport_ssh::TOKIO.block_on(manager.stop(tunnel_id))).await;
                let _ = entity.update(cx, |panel, cx| {
                    if let Some(spec) = panel
                        .agent_tunnels
                        .iter_mut()
                        .find(|tunnel| tunnel.agent_id == agent_id)
                    {
                        spec.state = AgentTunnelState::Deleted;
                    }
                    panel.settle_tool_result(
                        msg_index,
                        call_index,
                        t!("ai_panel.tool_tunnel_deleted", id = agent_id.to_string()).to_string(),
                        ToolDecision::Allowed,
                        cx,
                    );
                });
            })
            .detach();
            return;
        }
        if let Some(spec) = self.agent_tunnels.get_mut(spec_ix) {
            spec.state = AgentTunnelState::Deleted;
        }
        self.settle_tool_result(
            msg_index,
            call_index,
            t!("ai_panel.tool_tunnel_deleted", id = agent_id.to_string()).to_string(),
            ToolDecision::Allowed,
            cx,
        );
    }

    /// Run a tunnel tool against a *Tunnels-page* config bound to this
    /// terminal's host. `open` borrows this terminal's SSH connection for the
    /// run; close/delete go through the app's own machinery so the Tunnels
    /// page and the store stay consistent — the agent never edits another
    /// host's (let alone another terminal's) tunnels.
    fn tunnel_registry_op(
        &mut self,
        op: &str,
        config_id: i64,
        msg_index: usize,
        call_index: usize,
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
        let host_id = terminal.read(cx).host_id();
        // Only tunnels configured for this terminal's host exist here —
        // anything else reports as an unknown id.
        let not_reachable = !self
            .session
            .tunnels
            .list()
            .iter()
            .any(|view| view.id == config_id && Some(view.host_id) == host_id);
        if not_reachable {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_tunnel_unknown", id = format!("t{config_id}")).to_string(),
                ToolDecision::Denied,
                cx,
            );
            return;
        }
        let Some(app) = self.session.app.upgrade() else {
            self.settle_tool_result(
                msg_index,
                call_index,
                t!("ai_panel.tool_terminal_closed").to_string(),
                ToolDecision::Allowed,
                cx,
            );
            return;
        };
        let registry = self.session.tunnels.clone();
        let id_token = format!("t{config_id}");

        match op {
            "tunnel_open" => {
                if registry.is_running(config_id) {
                    let text = t!("ai_panel.tool_tunnel_already").to_string();
                    self.settle_tool_result(msg_index, call_index, text, ToolDecision::Allowed, cx);
                    return;
                }
                let tab_id = terminal.read(cx).pane_id();
                app.update(cx, |app, cx| {
                    app.start_tunnel_borrowed(config_id, tab_id, cx)
                });
                self.mark_running(msg_index, call_index, cx);
                let entity = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    let started_at = std::time::Instant::now();
                    let result = loop {
                        if registry.is_running(config_id) {
                            break Ok(());
                        }
                        if started_at.elapsed() >= TUNNEL_START_TIMEOUT {
                            break Err(t!("ai_panel.tunnel_status_stopped").to_string());
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    };
                    let text = match (&result, registry.manager_for(config_id)) {
                        (Ok(_), Some(manager)) => match manager.list().first() {
                            Some(info) => {
                                let mut text = format!(
                                    "{} — {}",
                                    id_token,
                                    t!(
                                        "ai_panel.tunnel_running_at",
                                        bind = format!("{}:{}", info.bind_addr, info.bind_port)
                                    )
                                );
                                if !info.target_host.is_empty() && info.target_port != 0 {
                                    text.push_str(&format!(
                                        " -> {}:{}",
                                        info.target_host, info.target_port
                                    ));
                                }
                                text
                            }
                            None => id_token.clone(),
                        },
                        _ => format!(
                            "{}: {}",
                            t!("ai_panel.tool_tunnel_open_failed"),
                            match &result {
                                Err(reason) => reason.clone(),
                                Ok(_) => String::new(),
                            }
                        ),
                    };
                    let _ = entity.update(cx, |panel, cx| {
                        panel.settle_tool_result(
                            msg_index,
                            call_index,
                            text,
                            ToolDecision::Allowed,
                            cx,
                        );
                    });
                })
                .detach();
            }
            "tunnel_close" => {
                if !registry.is_running(config_id) {
                    self.settle_tool_result(
                        msg_index,
                        call_index,
                        t!("ai_panel.tool_tunnel_not_running", id = id_token.as_str()).to_string(),
                        ToolDecision::Allowed,
                        cx,
                    );
                    return;
                }
                app.update(cx, |app, cx| app.stop_tunnel(config_id, cx));
                self.mark_running(msg_index, call_index, cx);
                let entity = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    let started_at = std::time::Instant::now();
                    while registry.is_running(config_id)
                        && started_at.elapsed() < TUNNEL_START_TIMEOUT
                    {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                    let _ = entity.update(cx, |panel, cx| {
                        panel.settle_tool_result(
                            msg_index,
                            call_index,
                            t!("ai_panel.tool_tunnel_closed", id = id_token.as_str()).to_string(),
                            ToolDecision::Allowed,
                            cx,
                        );
                    });
                })
                .detach();
            }
            "tunnel_delete" => {
                app.update(cx, |app, cx| app.remove_tunnel(config_id, cx));
                self.mark_running(msg_index, call_index, cx);
                let entity = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    let started_at = std::time::Instant::now();
                    while registry.list().iter().any(|view| view.id == config_id)
                        && started_at.elapsed() < TUNNEL_START_TIMEOUT
                    {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                    let _ = entity.update(cx, |panel, cx| {
                        panel.settle_tool_result(
                            msg_index,
                            call_index,
                            t!("ai_panel.tool_tunnel_deleted", id = id_token.as_str()).to_string(),
                            ToolDecision::Allowed,
                            cx,
                        );
                    });
                })
                .detach();
            }
            _ => {}
        }
    }

    /// Handle one approved SFTP tool call: list a directory, or move a file
    /// between the remote host and the local machine — always on this
    /// session's connection.
    fn start_sftp_op(
        &mut self,
        call: &ToolCall,
        msg_index: usize,
        call_index: usize,
        cx: &mut Context<Self>,
    ) {
        let args = self
            .tool_state_mut(msg_index, call_index)
            .and_then(|state| state.args.clone());
        let arg_str = |key: &str| -> Option<String> {
            args.as_ref()
                .and_then(|args| args.get(key)?.as_str())
                .map(str::to_string)
        };
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
        match call.name.as_str() {
            "sftp_list" => {
                let Some(path) = arg_str("path") else {
                    self.settle_tool_result(
                        msg_index,
                        call_index,
                        t!("ai_panel.tool_bad_args").to_string(),
                        ToolDecision::Denied,
                        cx,
                    );
                    return;
                };
                // Snapshot the listing *identity* before navigating: the
                // backend installs a fresh entries Arc on every successful
                // read_dir, so a pointer change — not a cwd change — is what
                // says "the listing I asked for arrived". Comparing cwd
                // alone hangs forever when the SFTP panel already sits in
                // the requested directory, which is why the first listing
                // (usually the home directory) always timed out.
                let before_entries = terminal.read(cx).sftp_entries();
                self.mark_running(msg_index, call_index, cx);
                terminal.read(cx).sftp_navigate(&path);
                let entity = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    let started_at = std::time::Instant::now();
                    let mut result: Option<String> = None;
                    while started_at.elapsed() < SFTP_LIST_TIMEOUT {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                        // The backend logs a failed navigation silently; the
                        // timeout below is what reports it.
                        let Ok(arrived) = terminal.read_with(cx, |view, _| {
                            let entries = view.sftp_entries();
                            let refreshed = match (&before_entries, &entries) {
                                (None, Some(_)) => true,
                                (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
                                _ => false,
                            };
                            refreshed.then(|| view.sftp_cwd().zip(entries))
                        }) else {
                            break; // terminal view went away
                        };
                        let Some((cwd, entries)) = arrived.flatten() else {
                            continue;
                        };
                        result = Some(format_remote_listing(cwd.as_str(), &entries));
                        break;
                    }
                    let result =
                        result.unwrap_or_else(|| t!("ai_panel.tool_sftp_list_timeout").to_string());
                    let _ = entity.update(cx, |panel, cx| {
                        panel.settle_tool_result(
                            msg_index,
                            call_index,
                            result,
                            ToolDecision::Allowed,
                            cx,
                        );
                    });
                })
                .detach();
            }
            "sftp_download" | "sftp_upload" => {
                let (Some(remote_path), Some(local_path)) =
                    (arg_str("remote_path"), arg_str("local_path"))
                else {
                    self.settle_tool_result(
                        msg_index,
                        call_index,
                        t!("ai_panel.tool_bad_args").to_string(),
                        ToolDecision::Denied,
                        cx,
                    );
                    return;
                };
                self.mark_running(msg_index, call_index, cx);
                // Subscribe *before* starting: the completion event is the
                // only signal a transfer finished.
                let mut events = terminal.read(cx).subscribe_backend();
                if call.name == "sftp_download" {
                    terminal.read(cx).sftp_download(&remote_path, &local_path);
                } else {
                    terminal.read(cx).sftp_upload(&local_path, &remote_path);
                }
                let want_download = call.name == "sftp_download";
                let entity = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    let started_at = std::time::Instant::now();
                    let mut outcome: Option<(bool, String)> = None;
                    while outcome.is_none() && started_at.elapsed() < SFTP_TRANSFER_TIMEOUT {
                        cx.background_executor().timer(OUTPUT_POLL).await;
                        loop {
                            match events.try_recv() {
                                Ok(BackendEvent::SftpTransferFinished {
                                    kind,
                                    success,
                                    message,
                                }) => {
                                    let wanted = want_download
                                        && kind == SftpTransferKind::Download
                                        || !want_download && kind == SftpTransferKind::Upload;
                                    if wanted {
                                        outcome = Some((success, message));
                                        break;
                                    }
                                }
                                Ok(_) => continue,
                                Err(_) => break,
                            }
                        }
                    }
                    let result = match outcome {
                        Some((true, message)) => {
                            t!("ai_panel.tool_sftp_done", message = message).to_string()
                        }
                        Some((false, message)) => {
                            format!("{}: {message}", t!("ai_panel.tool_sftp_failed"))
                        }
                        None => t!(
                            "ai_panel.tool_sftp_transfer_timeout",
                            mins = (SFTP_TRANSFER_TIMEOUT.as_secs() / 60).to_string()
                        )
                        .to_string(),
                    };
                    let _ = entity.update(cx, |panel, cx| {
                        panel.settle_tool_result(
                            msg_index,
                            call_index,
                            result,
                            ToolDecision::Allowed,
                            cx,
                        );
                    });
                })
                .detach();
            }
            other => {
                self.settle_tool_result(
                    msg_index,
                    call_index,
                    t!("ai_panel.tool_unknown", name = other).to_string(),
                    ToolDecision::Denied,
                    cx,
                );
            }
        }
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
        let cut = compaction_cut(&self.messages, keep);
        if cut == 0 {
            // Everything is recent enough to keep — nothing to summarize.
            return false;
        }
        let transcript = compaction_transcript(&self.messages[..cut]);
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
            ToolKind::TerminalRead => {
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
        cancel: ExecCancel,
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
            let mut cancelled = false;
            loop {
                cx.background_executor().timer(OUTPUT_POLL).await;
                if cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
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
                if cancelled {
                    // The caption under the fields already says it was
                    // stopped; an empty body is honest.
                    String::new()
                } else if timed_out {
                    t!("ai_panel.tool_exec_no_output_timeout").to_string()
                } else {
                    t!("ai_panel.tool_exec_no_output").to_string()
                }
            } else {
                result
            };
            let _ = entity.update(cx, |panel, cx| {
                panel.finish_tool_run(msg_index, call_index, result, cancelled, cx);
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
            state.cancelled = out.cancelled;
            state.running = false;
        }
        self.settle_tool(msg_index, call_index, cx);
    }

    /// Store a visible run's result: the output the screen gained, plus
    /// whether the user ended the wait before it settled.
    fn finish_tool_run(
        &mut self,
        msg_index: usize,
        call_index: usize,
        result: String,
        cancelled: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.tool_state_mut(msg_index, call_index) {
            state.result = Some(result);
            state.cancelled = cancelled;
            state.running = false;
        }
        self.settle_tool(msg_index, call_index, cx);
    }

    /// Stop a running tool call — the card's stop button. A captured command
    /// is closed down (its output so far is kept); a visible run stops being
    /// waited on, and the command itself keeps running in the terminal,
    /// which is where the user can interrupt it for real. Either way the
    /// call settles through the normal path.
    fn cancel_tool_call(&mut self, msg_index: usize, call_index: usize, cx: &mut Context<Self>) {
        let Some(state) = self.tool_state_mut(msg_index, call_index) else {
            return;
        };
        if !state.running {
            return;
        }
        state.cancel.cancel();
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
                        // `a…` ids resolve against this panel's own tunnels;
                        // `t…` ids against the Tunnels-page registry (host-
                        // scoped; anything else shows the token untouched).
                        match token
                            .strip_prefix('a')
                            .and_then(|number| number.parse::<u64>().ok())
                        {
                            Some(agent_id) => self
                                .agent_tunnels
                                .iter()
                                .find(|tunnel| tunnel.agent_id == agent_id)
                                .map(|tunnel| tunnel.describe(self.tunnel_manager.as_ref()))
                                .unwrap_or_else(|| token.to_string()),
                            None => match token
                                .strip_prefix('t')
                                .and_then(|number| number.parse::<i64>().ok())
                                .and_then(|config_id| {
                                    self.session
                                        .tunnels
                                        .list()
                                        .into_iter()
                                        .find(|view| view.id == config_id)
                                }) {
                                Some(view) => {
                                    format!("{} {} ({})", token, view.name, view.kind.as_str())
                                }
                                None => token.to_string(),
                            },
                        }
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
        let input_area = div()
            // With no conversation the input area *is* the panel: it takes
            // every pixel the controls row doesn't need, and the editor
            // element (`height: 100%` + `flex_grow` in the library) fills
            // it. Once there are messages it goes back to its natural
            // height — 6 rows, auto-growing to 12.
            .when(!has_conversation, |el| el.flex_1().min_h_0())
            .child(input_el);
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

/// Most bytes of an HTTP body a `fetch` reads before capping. Bounds the
/// memory the read holds; the result is capped again for the model.
const FETCH_BODY_LIMIT: u64 = 256 * 1024;

/// Cap a tool result at [`MAX_TOOL_RESULT_BYTES`] keeping the *head* — used
/// where the beginning of the text is the interesting part (HTTP bodies, file
/// contents) — and note how much was dropped. The cut is walked back to a
/// char boundary so the slice can't panic on multi-byte output.
fn cap_tool_result_head(text: &str) -> String {
    if text.len() <= MAX_TOOL_RESULT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_TOOL_RESULT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n(later output omitted — {} bytes)",
        &text[..end],
        text.len() - end
    )
}

/// Send one HTTP request with CrabPort's own HTTP client, optionally through
/// a proxy URL, and return the text to hand back to the model. Never fails:
/// every problem becomes readable text.
fn fetch_url(url: &str, proxy: Option<&str>, method: &str, body: Option<&str>) -> String {
    let mut config = ureq::Agent::config_builder().timeout_global(Some(FETCH_TIMEOUT));
    if let Some(proxy_url) = proxy {
        match ureq::Proxy::new(proxy_url) {
            Ok(proxy) => config = config.proxy(Some(proxy)),
            Err(err) => return format!("invalid proxy url: {err}"),
        }
    }
    let agent = ureq::Agent::new_with_config(config.build());

    let outcome = match method {
        "POST" => agent.post(url).send(body.unwrap_or_default()),
        "PUT" => agent.put(url).send(body.unwrap_or_default()),
        "DELETE" => agent.delete(url).call(),
        "HEAD" => agent.head(url).call(),
        _ => agent.get(url).call(),
    };
    match outcome {
        Ok(mut resp) => {
            let mut text = format!("HTTP {}\n", resp.status());
            for (name, value) in resp.headers() {
                text.push_str(&format!("{name}: {}\n", value.to_str().unwrap_or("…")));
            }
            match resp
                .body_mut()
                .with_config()
                .limit(FETCH_BODY_LIMIT)
                .read_to_string()
            {
                Ok(body_text) => {
                    text.push('\n');
                    text.push_str(&cap_tool_result_head(body_text.trim_end()));
                }
                Err(err) => text.push_str(&format!("\n(body unreadable: {err})")),
            }
            text
        }
        Err(err) => format!("request failed: {err}"),
    }
}

/// Read up to [`READ_FILE_LIMIT`] bytes of a local file; binary content is
/// reported rather than dumped. Never fails — every problem becomes text.
fn read_local_file(path: &str) -> String {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return format!("cannot open {path}: {err}"),
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut reader = file.take(READ_FILE_LIMIT);
    let mut buf = Vec::new();
    use std::io::Read as _;
    if let Err(err) = reader.read_to_end(&mut buf) {
        return format!("reading {path} failed: {err}");
    }
    if buf.contains(&0) {
        return t!("ai_panel.tool_read_file_binary", bytes = size.to_string()).to_string();
    }
    cap_tool_result_head(&String::from_utf8_lossy(&buf))
}

/// List a local directory: one line per entry, directories marked with a
/// trailing slash, capped at [`READ_DIR_LIMIT`]. Never fails — every problem
/// becomes text.
fn read_local_directory(path: &str) -> String {
    let dir = match std::fs::read_dir(path) {
        Ok(dir) => dir,
        Err(err) => return format!("cannot list {path}: {err}"),
    };
    let mut rows: Vec<String> = Vec::new();
    let mut overflow = 0usize;
    for entry in dir {
        let Ok(entry) = entry else { continue };
        if rows.len() >= READ_DIR_LIMIT {
            overflow += 1;
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_dir {
            rows.push(format!("{name}/"));
        } else {
            let size = entry
                .metadata()
                .map(|meta| meta.len().to_string())
                .unwrap_or_else(|_| "?".to_string());
            rows.push(format!("{name}  {size}"));
        }
    }
    if rows.is_empty() {
        return t!("ai_panel.tool_dir_empty").to_string();
    }
    let mut out = rows.join("\n");
    if overflow > 0 {
        out.push('\n');
        out.push_str(&t!("ai_panel.tool_dir_truncated", more = overflow));
    }
    out
}

/// Format a remote SFTP directory listing for the model: the resolved
/// directory, then one line per entry (directories marked with a trailing
/// slash), capped at [`READ_DIR_LIMIT`].
fn format_remote_listing(cwd: &str, entries: &[crabport_sftp::FileEntry]) -> String {
    let mut out = format!("{cwd}:\n");
    for entry in entries.iter().take(READ_DIR_LIMIT) {
        if entry.is_dir {
            out.push_str(&format!("{}/\n", entry.name));
        } else {
            let size = entry
                .size
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "?".to_string());
            out.push_str(&format!("{}  {size}\n", entry.name));
        }
    }
    if entries.len() > READ_DIR_LIMIT {
        out.push_str(&t!(
            "ai_panel.tool_dir_truncated",
            more = entries.len() - READ_DIR_LIMIT
        ));
    }
    if entries.is_empty() {
        return t!("ai_panel.tool_dir_empty").to_string();
    }
    out
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

/// Rough token size of one display turn, matching what its wire form costs:
/// the content plus every tool call's arguments and result. Reasoning is not
/// counted — it is display-only and never replayed.
fn message_tokens(msg: &DisplayMessage) -> usize {
    let mut total = MESSAGE_OVERHEAD_TOKENS + estimate_tokens(&msg.content);
    for call in &msg.tool_calls {
        total += TOOL_CALL_OVERHEAD_TOKENS
            + estimate_tokens(&call.call.name)
            + estimate_tokens(&call.call.arguments)
            + call.result.as_deref().map(estimate_tokens).unwrap_or(0);
    }
    total
}

/// Index of the first message to keep verbatim when compacting a
/// conversation.
///
/// Walks backwards, keeping messages until roughly `keep_tokens` is reached,
/// then snaps the cut back to the user turn that opened that block: an
/// assistant's tool calls and their results must travel together, and the
/// kept tail reads best starting with the user's ask. `0` means "nothing old
/// enough to summarize" — the caller then leaves the history alone.
fn compaction_cut(messages: &[DisplayMessage], keep_tokens: usize) -> usize {
    let mut used = 0usize;
    for (ix, msg) in messages.iter().enumerate().rev() {
        used += message_tokens(msg);
        if used >= keep_tokens {
            return messages[..=ix]
                .iter()
                .rposition(|msg| msg.role == DisplayRole::User)
                .unwrap_or(0);
        }
    }
    0
}

/// Plain-text transcript of the turns being compacted, for the summarizer.
///
/// Tool results are capped ([`COMPACT_RESULT_CAP`], tail kept) so a few huge
/// command outputs can't make the compaction request larger than the
/// conversation it is meant to shrink.
fn compaction_transcript(messages: &[DisplayMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg.role {
            DisplayRole::User => {
                out.push_str("USER: ");
                out.push_str(msg.content.trim());
                out.push('\n');
            }
            DisplayRole::Summary => {
                out.push_str("SUMMARY SO FAR: ");
                out.push_str(msg.content.trim());
                out.push('\n');
            }
            DisplayRole::Assistant => {
                if !msg.content.trim().is_empty() {
                    out.push_str("ASSISTANT: ");
                    out.push_str(msg.content.trim());
                    out.push('\n');
                }
                for call in &msg.tool_calls {
                    out.push_str("  [");
                    out.push_str(&call.call.name);
                    out.push_str("] ");
                    out.push_str(call.call.arguments.trim());
                    if let Some(result) = &call.result {
                        out.push_str(" -> ");
                        out.push_str(cap_for_summary(result.trim()));
                    }
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// `text` shortened to [`COMPACT_RESULT_CAP`] from the front (keeping the
/// tail, where a command's errors and summary usually are) when it is longer.
fn cap_for_summary(text: &str) -> &str {
    if text.len() <= COMPACT_RESULT_CAP {
        return text;
    }
    let mut cut = text.len() - COMPACT_RESULT_CAP;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    &text[cut..]
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

    // Body: the fields the user asked for, per tool — the arguments spell out
    // exactly what a click would do.
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
    } else {
        let purpose = state.arg_str("purpose").unwrap_or("");
        match kind {
            ToolKind::TerminalRead => {
                let lines = state.arg_u64("lines").unwrap_or(200);
                card = card.child(tool_card_field(
                    t!("ai_panel.tool_field_read").as_ref(),
                    t!("ai_panel.tool_field_read_lines", lines = lines).to_string(),
                    false,
                ));
            }
            ToolKind::Fetch => {
                let url = state.arg_str("url").unwrap_or("");
                card = card.child(tool_card_field(
                    t!("ai_panel.tool_field_url").as_ref(),
                    url.to_string(),
                    true,
                ));
                if let Some(proxy) = state.arg_str("proxy") {
                    card = card.child(tool_card_field(
                        t!("ai_panel.tool_field_proxy").as_ref(),
                        proxy.to_string(),
                        true,
                    ));
                }
                if let Some(method) = state.arg_str("method")
                    && !method.eq_ignore_ascii_case("GET")
                {
                    card = card.child(tool_card_field(
                        t!("ai_panel.tool_field_method").as_ref(),
                        method.to_string(),
                        false,
                    ));
                }
                if let Some(body) = state.arg_str("body") {
                    card = card.child(tool_card_field(
                        t!("ai_panel.tool_field_body").as_ref(),
                        body.to_string(),
                        true,
                    ));
                }
                if let Some(expected) = state.arg_str("expected")
                    && !expected.is_empty()
                {
                    card = card.child(tool_card_field(
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
                card = card.child(tool_card_field(
                    label.as_ref(),
                    state.arg_str("path").unwrap_or("").to_string(),
                    true,
                ));
            }
            ToolKind::Tunnel => {
                if let Some(tunnel) = tunnel {
                    card = card.child(tool_card_field(
                        t!("ai_panel.tool_field_tunnel").as_ref(),
                        tunnel.to_string(),
                        false,
                    ));
                }
                if name == "tunnel_create" {
                    let kind_arg = state.arg_str("kind").unwrap_or("dynamic");
                    card = card.child(tool_card_field(
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
                    card = card.child(tool_card_field(
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
                        card = card.child(tool_card_field(
                            t!("ai_panel.tool_field_target").as_ref(),
                            target,
                            true,
                        ));
                    }
                    if let Some(expected) = state.arg_str("expected")
                        && !expected.is_empty()
                    {
                        card = card.child(tool_card_field(
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
                card = card.child(tool_card_field(label.as_ref(), value, true));
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
                    card = card.child(tool_card_field(label.as_ref(), value, true));
                }
            }
            ToolKind::ExecCapture | ToolKind::Run | ToolKind::Unknown => {}
        }
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

    // How the call ended, in the same muted register as the field labels:
    // the green check in the corner already says "it ran", so only a stop, a
    // timeout or a non-zero status is worth spelling out.
    if state.cancelled {
        let caption = match kind {
            ToolKind::ExecCapture => t!("ai_panel.tool_capture_cancelled").to_string(),
            _ => t!("ai_panel.tool_run_cancelled").to_string(),
        };
        card = card.child(div().text_xs().text_color(rgb(text_muted())).child(caption));
    } else if state.timed_out {
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
            if state.running && is_execute {
                // Stop: the only control while a *cancellable* call runs —
                // the two command executors. Other async tools (fetch, local
                // reads, SFTP transfers) show just the loader; they cannot be
                // stopped yet.
                let red = term_red();
                let h = entity.clone();
                card = card.child(
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
                                panel.cancel_tool_call(message_ix, call_ix, cx)
                            });
                        }),
                    ),
                );
            }
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
    use super::{
        COMPACT_RESULT_CAP, DisplayMessage, DisplayRole, MAX_TOOL_RESULT_BYTES, ToolCallState,
        ToolDecision, ToolKind, cap_tool_result, cap_tool_result_head, compaction_cut,
        compaction_transcript, format_remote_listing, message_tokens, quote_shell, with_cwd,
    };

    fn msg(role: DisplayRole, content: &str) -> DisplayMessage {
        DisplayMessage {
            role,
            content: content.to_string(),
            reasoning: String::new(),
            reasoning_expanded: false,
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn tool_kind_maps_wire_names() {
        assert_eq!(ToolKind::of("terminal_read"), ToolKind::TerminalRead);
        assert_eq!(ToolKind::of("terminal_exec"), ToolKind::ExecCapture);
        assert_eq!(ToolKind::of("terminal_run"), ToolKind::Run);
        assert_eq!(ToolKind::of("terminal_execute"), ToolKind::Unknown);
        assert_eq!(ToolKind::of("fetch"), ToolKind::Fetch);
        assert_eq!(ToolKind::of("read_file"), ToolKind::LocalFs);
        assert_eq!(ToolKind::of("read_directory"), ToolKind::LocalFs);
        assert_eq!(ToolKind::of("tunnel_create"), ToolKind::Tunnel);
        assert_eq!(ToolKind::of("tunnel_open"), ToolKind::Tunnel);
        assert_eq!(ToolKind::of("tunnel_close"), ToolKind::Tunnel);
        assert_eq!(ToolKind::of("tunnel_delete"), ToolKind::Tunnel);
        assert_eq!(ToolKind::of("sftp_list"), ToolKind::Sftp);
        assert_eq!(ToolKind::of("sftp_download"), ToolKind::Sftp);
        assert_eq!(ToolKind::of("sftp_upload"), ToolKind::Sftp);
        assert_eq!(ToolKind::of("sftp_delete"), ToolKind::Sftp);
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

    /// Compaction keeps the recent tail and snaps its cut back to a user
    /// turn, so an assistant's tool calls never get separated from the
    /// results that answer them.
    #[test]
    fn compaction_cut_keeps_recent_turns_and_snaps_to_user() {
        let messages = vec![
            msg(DisplayRole::User, "u1"),
            msg(DisplayRole::Assistant, "a1"),
            msg(DisplayRole::User, "u2"),
            msg(DisplayRole::Assistant, "a2"),
            msg(DisplayRole::User, "u3"),
        ];
        // A tiny budget keeps only the newest user turn.
        assert_eq!(compaction_cut(&messages, 1), 4);
        // A budget covering `u3` and `a2` snaps back to the turn that opened
        // the block: `a2` must not travel without `u2`.
        let keep = message_tokens(&messages[4]) + message_tokens(&messages[3]);
        assert_eq!(compaction_cut(&messages, keep), 2);
        // Everything fits: nothing old enough to summarize.
        assert_eq!(compaction_cut(&messages, usize::MAX), 0);
        assert_eq!(compaction_cut(&[], 10), 0);
    }

    /// The transcript fed to the summarizer labels roles, carries tool calls
    /// with their results, and caps huge results so the compaction request
    /// cannot be bigger than the history it is shrinking.
    #[test]
    fn compaction_transcript_labels_roles_and_caps_results() {
        let long_result = "x".repeat(COMPACT_RESULT_CAP * 2);
        let mut assistant = msg(DisplayRole::Assistant, "done");
        assistant.tool_calls.push(ToolCallState {
            call: crabport_ai::ToolCall {
                id: "c1".into(),
                name: "terminal_exec".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
            args: None,
            decision: ToolDecision::Allowed,
            result: Some(long_result.clone()),
            running: false,
            exit_code: Some(0),
            timed_out: false,
            cancel: crabport_terminal::terminal::ExecCancel::default(),
            cancelled: false,
        });
        let text = compaction_transcript(&[msg(DisplayRole::User, "hi"), assistant]);
        assert!(text.contains("USER: hi"), "{text}");
        assert!(
            text.contains("[terminal_exec] {\"command\":\"ls\"} -> "),
            "{text}"
        );
        // The capped result must not carry more than one cap of text.
        assert!(!text.contains(&"x".repeat(COMPACT_RESULT_CAP + 1)));
    }

    /// The head-keeping cap is for HTTP bodies and file contents: the
    /// beginning survives, the dropped amount is noted, and multi-byte
    /// output is never split.
    #[test]
    fn cap_tool_result_head_keeps_the_front_on_a_char_boundary() {
        let short = "hello";
        assert_eq!(cap_tool_result_head(short), short);

        let long = "⇒".repeat(MAX_TOOL_RESULT_BYTES * 2);
        let capped = cap_tool_result_head(&long);
        assert!(capped.len() < long.len());
        assert!(capped.contains("(later output omitted"));
        assert!(capped.starts_with("⇒"));
    }

    /// A remote listing marks directories with a trailing slash, carries
    /// sizes where known, and resolves the directory it lists.
    #[test]
    fn format_remote_listing_marks_dirs_and_sizes() {
        let entry = |name: &str, is_dir: bool, size: Option<u64>| crabport_sftp::FileEntry {
            name: name.to_string(),
            is_dir,
            size,
            permissions: None,
            modified: None,
        };
        let text = format_remote_listing(
            "/var/log",
            &[entry("src", true, None), entry("a.txt", false, Some(12))],
        );
        assert!(text.starts_with("/var/log:\n"), "{text}");
        assert!(text.contains("src/\n"), "{text}");
        assert!(text.contains("a.txt  12\n"), "{text}");
    }
}
