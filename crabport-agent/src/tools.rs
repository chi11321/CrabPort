//! The advertised tool specs and their wire-name metadata. Stable names —
//! conversations replay them, and everything else dispatches on [`ToolKind`].

use crabport_ai::ToolSpec;

/// Every tool name the agent advertises, in definition order.
///
/// For surfaces that only need to *enumerate* the tools — the settings
/// pane's per-tool permission list — without building the JSON schemas.
/// Kept in sync with [`agent_tools`] by a test.
pub const TOOL_NAMES: &[&str] = &[
    "terminal_read",
    "terminal_exec",
    "terminal_run",
    "read_file",
    "read_directory",
    "fetch",
    "tunnel_create",
    "tunnel_list",
    "tunnel_open",
    "tunnel_close",
    "tunnel_delete",
    "sftp_list",
    "sftp_download",
    "sftp_upload",
];

/// Tools advertised to the model. Names are stable — the wire history replays
/// them, and the panel dispatches on them in [`ToolKind::of`].
pub fn agent_tools() -> Vec<ToolSpec> {
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

/// Which agent tool a call names. The names are the wire contract with the
/// model, so they are mapped in exactly one place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
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
    pub fn of(name: &str) -> Self {
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

#[cfg(test)]
mod tests {
    use super::{TOOL_NAMES, ToolKind, agent_tools};

    /// The name list the settings pane enumerates matches the advertised
    /// specs exactly — a tool added to one and not the other would silently
    /// lose its permission row (or list a tool that doesn't exist).
    #[test]
    fn tool_names_match_agent_tools() {
        let advertised: Vec<String> = agent_tools().into_iter().map(|spec| spec.name).collect();
        assert_eq!(TOOL_NAMES, advertised.as_slice());
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
}
