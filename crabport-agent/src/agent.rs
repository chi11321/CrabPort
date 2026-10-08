//! The agent's tool execution: one [`Agent`] per conversation runs the
//! advertised tools against an [`AgentSession`].
//!
//! Everything here is UI-free — the polling loops included. They run on
//! background threads via `smol`, and hand their result to the UI through an
//! [`Executed::Pending`] receiver; the UI only awaits and renders.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_channel::{Receiver, bounded};
use rust_i18n::t;
use serde_json::Value;

use crabport_ssh::{TOKIO, TunnelKind, TunnelManager, TunnelStatus};
use crabport_terminal::terminal::{BackendEvent, ExecCancel, ExecOutput, SftpTransferKind};

use crate::limits::{
    AGENT_READ_LINES, EXEC_CAPTURE_TIMEOUT, OUTPUT_POLL, OUTPUT_QUIET, OUTPUT_TIMEOUT,
    SFTP_LIST_TIMEOUT, SFTP_TRANSFER_TIMEOUT, TUNNEL_START_TIMEOUT,
};
use crate::session::AgentSession;
use crate::sftp::format_remote_listing;
use crate::shell::with_cwd;
use crate::text::{cap_tool_result, new_since};
use crate::tools::ToolKind;
use crate::tunnel::{AgentTunnel, AgentTunnelState};

/// What one tool call produced.
#[derive(Clone, Default)]
pub struct ToolOutcome {
    /// The text handed back to the model (the approval card renders it too).
    pub text: String,
    /// Exit status of a captured command, when the backend reported one.
    pub exit_code: Option<u32>,
    /// A captured command outlived its deadline; the output is what was
    /// captured before that.
    pub timed_out: bool,
    /// The user stopped the call while it ran.
    pub cancelled: bool,
}

impl ToolOutcome {
    /// An outcome that is plain text and nothing else.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// How a tool call is running.
pub enum Executed {
    /// The result is ready.
    Done(ToolOutcome),
    /// Work started. The outcome arrives exactly once on the receiver, and
    /// `cancel` stops the call (the card's stop button) — for a visible run
    /// that ends the *wait*, not the command in the user's terminal.
    Pending {
        outcome: Receiver<ToolOutcome>,
        cancel: ExecCancel,
    },
    /// The call could not start: missing or malformed arguments, or a
    /// capability this session turned out not to have. The text says why, and
    /// the card shows it as a refusal.
    Refused(String),
}

/// One conversation's tool runner.
pub struct Agent {
    /// Tunnels the agent defined here, in creation order. Shared with the
    /// tasks that start them, so a completion can record its state.
    tunnels: Arc<Mutex<Vec<AgentTunnel>>>,
    next_tunnel_id: u64,
}

impl Agent {
    pub fn new() -> Self {
        Self {
            tunnels: Arc::new(Mutex::new(Vec::new())),
            next_tunnel_id: 1,
        }
    }

    /// Whether this tool can run on the session at all; `Some(reason)` blocks
    /// it, and the card says so without offering a decision that would do
    /// nothing.
    pub fn blocked_reason(&self, session: &dyn AgentSession, name: &str) -> Option<String> {
        match ToolKind::of(name) {
            ToolKind::ExecCapture if !session.allow_exec_capture() => {
                Some(t!("ai_panel.tool_exec_unsupported").to_string())
            }
            ToolKind::Tunnel if !session.allow_tunnels() => {
                Some(t!("ai_panel.tool_no_tunnel").to_string())
            }
            ToolKind::Sftp if !session.allow_sftp() => {
                Some(t!("ai_panel.tool_no_sftp").to_string())
            }
            _ => None,
        }
    }

    /// One-line description of the tunnel a `tunnel_*` call refers to, for its
    /// approval card. `None` when the token isn't one of this agent's own.
    pub fn describe_tunnel_token(
        &self,
        manager: Option<&Arc<TunnelManager>>,
        token: &str,
    ) -> Option<String> {
        let agent_id = token
            .strip_prefix('a')
            .and_then(|number| number.parse::<u64>().ok())?;
        let tunnels = self.tunnels.lock().unwrap_or_else(|e| e.into_inner());
        tunnels
            .iter()
            .find(|tunnel| tunnel.agent_id == agent_id)
            .map(|tunnel| tunnel.describe(manager))
    }

    /// Run one approved call. `args` is the model's parsed argument object.
    pub fn run(&mut self, session: &Arc<dyn AgentSession>, name: &str, args: &Value) -> Executed {
        match ToolKind::of(name) {
            ToolKind::TerminalRead => self.read_terminal(session, args),
            ToolKind::ExecCapture => match command_arg(args) {
                Some(command) => self.start_capture(session, &command, ExecCancel::default()),
                None => Executed::Refused(t!("ai_panel.tool_bad_args").to_string()),
            },
            ToolKind::Run => match command_arg(args) {
                Some(command) => self.start_visible_run(session, &command, ExecCancel::default()),
                None => Executed::Refused(t!("ai_panel.tool_bad_args").to_string()),
            },
            ToolKind::Fetch => self.start_fetch(args),
            ToolKind::LocalFs => self.start_local_fs(name, args),
            ToolKind::Sftp => match name {
                "sftp_list" => self.start_sftp_list(session, args),
                _ => self.start_sftp_transfer(session, name, args),
            },
            ToolKind::Tunnel => self.run_tunnel(session, name, args),
            ToolKind::Unknown => {
                Executed::Refused(t!("ai_panel.tool_unknown", name = name).to_string())
            }
        }
    }

    // -- terminal tools -----------------------------------------------------

    /// `terminal_read`: hand back the most recent screenful.
    fn read_terminal(&self, session: &Arc<dyn AgentSession>, args: &Value) -> Executed {
        let lines = args
            .get("lines")
            .and_then(|lines| lines.as_u64())
            .unwrap_or(200) as usize;
        let text = session.dump_text(lines);
        let text = if text.trim().is_empty() {
            t!("ai_panel.tool_read_empty").to_string()
        } else {
            text
        };
        Executed::Done(ToolOutcome::text(text))
    }

    /// `terminal_exec`: out of band, output captured directly. The command
    /// runs where the user's shell is (when it has reported a directory).
    fn start_capture(
        &self,
        session: &Arc<dyn AgentSession>,
        command: &str,
        cancel: ExecCancel,
    ) -> Executed {
        if !session.allow_exec_capture() {
            return Executed::Refused(t!("ai_panel.tool_exec_unsupported").to_string());
        }
        let effective = with_cwd(command, session.cwd().as_deref());
        let (tx, rx) = bounded(1);
        session.exec_capture(
            &effective,
            EXEC_CAPTURE_TIMEOUT,
            cancel.clone(),
            Arc::new(move |out: ExecOutput| {
                let text = cap_tool_result(out.output.trim_matches('\n'));
                let text = if text.trim().is_empty() {
                    t!("ai_panel.tool_exec_no_output").to_string()
                } else {
                    text
                };
                let _ = tx.try_send(ToolOutcome {
                    text,
                    exit_code: out.exit_code,
                    timed_out: out.timed_out,
                    cancelled: out.cancelled,
                });
            }),
        );
        Executed::Pending {
            outcome: rx,
            cancel,
        }
    }

    /// `terminal_run`: type the line into the live shell and wait for its
    /// output to settle. "Settled" is a heuristic — a shell gives no
    /// completion signal without OSC 133 — so the screen is sampled every
    /// [`OUTPUT_POLL`] and counts as done after [`OUTPUT_QUIET`] without
    /// change, or after [`OUTPUT_TIMEOUT`] when it keeps printing.
    fn start_visible_run(
        &self,
        session: &Arc<dyn AgentSession>,
        command: &str,
        cancel: ExecCancel,
    ) -> Executed {
        let before = session.dump_text(AGENT_READ_LINES);
        let mut bytes = command.as_bytes().to_vec();
        bytes.push(b'\r');
        session.write_raw(&bytes);

        let session = session.clone();
        let task_cancel = cancel.clone();
        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            let started = Instant::now();
            let mut last = before.clone();
            let mut quiet_since = Instant::now();
            let mut timed_out = false;
            let mut cancelled = false;
            loop {
                smol::Timer::after(OUTPUT_POLL).await;
                if task_cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let Some(now) = session.try_dump_text(AGENT_READ_LINES) else {
                    continue; // terminal lock held by the reader; try next tick
                };
                if now != last {
                    last = now;
                    quiet_since = Instant::now();
                } else if quiet_since.elapsed() >= OUTPUT_QUIET {
                    break;
                }
                if started.elapsed() >= OUTPUT_TIMEOUT {
                    timed_out = true;
                    break;
                }
            }
            let mut text = new_since(&before, &last);
            if text.trim().is_empty() {
                text = if cancelled {
                    // The card's caption already says it was stopped; an
                    // empty body is honest.
                    String::new()
                } else if timed_out {
                    t!("ai_panel.tool_exec_no_output_timeout").to_string()
                } else {
                    t!("ai_panel.tool_exec_no_output").to_string()
                };
            }
            let _ = tx
                .send(ToolOutcome {
                    text,
                    exit_code: None,
                    timed_out,
                    cancelled,
                })
                .await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel,
        }
    }

    // -- local & network tools ---------------------------------------------

    /// `fetch`: one HTTP request through CrabPort's own client.
    fn start_fetch(&self, args: &Value) -> Executed {
        let Some(url) = args.get("url").and_then(|url| url.as_str()) else {
            return Executed::Refused(t!("ai_panel.tool_bad_args").to_string());
        };
        let url = url.to_string();
        let proxy = args
            .get("proxy")
            .and_then(|proxy| proxy.as_str())
            .map(str::to_string);
        let method = args
            .get("method")
            .and_then(|method| method.as_str())
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let body = args
            .get("body")
            .and_then(|body| body.as_str())
            .map(str::to_string);

        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            let text = smol::unblock(move || {
                crate::fetch::fetch_url(&url, proxy.as_deref(), &method, body.as_deref())
            })
            .await;
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    /// `read_file` / `read_directory`: the machine CrabPort runs on.
    fn start_local_fs(&self, name: &str, args: &Value) -> Executed {
        let Some(path) = args.get("path").and_then(|path| path.as_str()) else {
            return Executed::Refused(t!("ai_panel.tool_bad_args").to_string());
        };
        let path = path.to_string();
        let is_file = name == "read_file";
        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            let text = smol::unblock(move || {
                if is_file {
                    crate::local_fs::read_local_file(&path)
                } else {
                    crate::local_fs::read_local_directory(&path)
                }
            })
            .await;
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    // -- sftp tools ---------------------------------------------------------

    /// `sftp_list`: navigate the backend and wait for the *fresh* listing.
    /// Freshness is the entries `Arc` being replaced — the backend installs a
    /// new one on every successful read_dir — because the cwd alone stays put
    /// when the requested directory is the one already shown.
    fn start_sftp_list(&self, session: &Arc<dyn AgentSession>, args: &Value) -> Executed {
        if !session.allow_sftp() {
            return Executed::Refused(t!("ai_panel.tool_no_sftp").to_string());
        }
        let Some(path) = args.get("path").and_then(|path| path.as_str()) else {
            return Executed::Refused(t!("ai_panel.tool_bad_args").to_string());
        };
        let before = session.sftp_listing().map(|(_, entries)| entries);
        session.sftp_navigate(path);

        let session = session.clone();
        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            let started = Instant::now();
            let mut text = None;
            while started.elapsed() < SFTP_LIST_TIMEOUT {
                smol::Timer::after(OUTPUT_POLL).await;
                // The backend logs a failed navigation silently; the timeout
                // below is what reports it.
                let Some((cwd, entries)) = session.sftp_listing() else {
                    continue;
                };
                let fresh = match &before {
                    None => true,
                    Some(old) => !Arc::ptr_eq(old, &entries),
                };
                if fresh {
                    text = Some(format_remote_listing(&cwd, &entries));
                    break;
                }
            }
            let text = text.unwrap_or_else(|| t!("ai_panel.tool_sftp_list_timeout").to_string());
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    /// `sftp_download` / `sftp_upload`: start the transfer and wait for its
    /// completion event — the `SftpTransferFinished` the UI also shows as a
    /// toast. The subscription is taken *before* the transfer starts: that
    /// event is the only signal a transfer finished.
    fn start_sftp_transfer(
        &self,
        session: &Arc<dyn AgentSession>,
        name: &str,
        args: &Value,
    ) -> Executed {
        if !session.allow_sftp() {
            return Executed::Refused(t!("ai_panel.tool_no_sftp").to_string());
        }
        let remote = args
            .get("remote_path")
            .and_then(|path| path.as_str())
            .map(str::to_string);
        let local = args
            .get("local_path")
            .and_then(|path| path.as_str())
            .map(str::to_string);
        let (Some(remote), Some(local)) = (remote, local) else {
            return Executed::Refused(t!("ai_panel.tool_bad_args").to_string());
        };
        let kind = if name == "sftp_download" {
            SftpTransferKind::Download
        } else {
            SftpTransferKind::Upload
        };
        let Some(mut events) = session.subscribe_events() else {
            return Executed::Refused(t!("ai_panel.tool_no_sftp").to_string());
        };
        if name == "sftp_download" {
            session.sftp_download(&remote, &local);
        } else {
            session.sftp_upload(&local, &remote);
        }

        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            let started = Instant::now();
            let mut outcome: Option<(bool, String)> = None;
            while outcome.is_none() && started.elapsed() < SFTP_TRANSFER_TIMEOUT {
                smol::Timer::after(OUTPUT_POLL).await;
                loop {
                    match events.try_recv() {
                        Ok(BackendEvent::SftpTransferFinished {
                            kind: finished,
                            success,
                            message,
                        }) => {
                            if finished == kind {
                                outcome = Some((success, message));
                                break;
                            }
                        }
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            }
            let text = match outcome {
                Some((true, message)) => {
                    t!("ai_panel.tool_sftp_done", message = message).to_string()
                }
                Some((false, message)) => {
                    t!("ai_panel.tool_sftp_failed", message = message).to_string()
                }
                None => t!(
                    "ai_panel.tool_sftp_transfer_timeout",
                    mins = (SFTP_TRANSFER_TIMEOUT.as_secs() / 60).to_string()
                )
                .to_string(),
            };
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    // -- tunnel tools -------------------------------------------------------

    /// `tunnel_list` / `tunnel_create` / `tunnel_open` / `tunnel_close` /
    /// `tunnel_delete`. Id tokens decide where a tunnel lives: `a<N>` is one
    /// this agent created, `t<N>` a Tunnels-page config bound to this
    /// session's host.
    fn run_tunnel(
        &mut self,
        session: &Arc<dyn AgentSession>,
        name: &str,
        args: &Value,
    ) -> Executed {
        match name {
            "tunnel_list" => Executed::Done(ToolOutcome::text(self.tunnel_list_text(session))),
            "tunnel_create" => self.tunnel_create(args),
            "tunnel_open" | "tunnel_close" | "tunnel_delete" => {
                let Some(token) = args.get("tunnel_id").and_then(|id| id.as_str()) else {
                    return Executed::Refused(t!("ai_panel.tool_bad_args").to_string());
                };
                let (prefix, number) = split_token(token);
                match (prefix, number) {
                    ('a', Some(agent_id)) => match name {
                        "tunnel_open" => self.tunnel_open(session, agent_id),
                        "tunnel_close" => self.tunnel_close(session, agent_id),
                        _ => self.tunnel_delete(session, agent_id),
                    },
                    ('t', Some(config_id)) => self.registry_op(session, name, config_id as i64),
                    _ => Executed::Refused(
                        t!("ai_panel.tool_tunnel_unknown", id = token).to_string(),
                    ),
                }
            }
            other => Executed::Refused(t!("ai_panel.tool_unknown", name = other).to_string()),
        }
    }

    /// The tunnel list: the agent's own first (their ids are the actionable
    /// ones), then the Tunnels-page configs for this host.
    fn tunnel_list_text(&self, session: &Arc<dyn AgentSession>) -> String {
        let manager = session.tunnel_manager();
        let mut lines: Vec<String> = Vec::new();
        {
            let tunnels = self.tunnels.lock().unwrap_or_else(|e| e.into_inner());
            if !tunnels.is_empty() {
                lines.push(t!("ai_panel.tool_tunnel_list_mine").to_string());
                lines.extend(
                    tunnels
                        .iter()
                        .map(|tunnel| tunnel.describe(manager.as_ref())),
                );
            }
        }
        let configured: Vec<String> = session
            .registry_tunnels()
            .into_iter()
            .map(|view| {
                let source = if view.borrowed {
                    t!("ai_panel.tunnel_source_borrowed")
                } else {
                    t!("ai_panel.tunnel_source_owned")
                };
                // Both ends of a local/remote tunnel go into the result — the
                // model needs host *and* port to use or talk about it.
                let mut addresses = format!("{}:{}", view.bind_addr, view.bind_port);
                if !view.target_host.is_empty() && view.target_port != 0 {
                    addresses.push_str(&format!(" -> {}:{}", view.target_host, view.target_port));
                }
                let state = if view.running {
                    t!("ai_panel.tunnel_running_at", bind = addresses)
                } else {
                    t!("ai_panel.tunnel_stopped_at", bind = addresses)
                };
                // The id token leads the line: it is what the open / close /
                // delete calls take.
                format!(
                    "t{} {} ({}, {}) — {}",
                    view.id,
                    view.name,
                    view.kind.as_str(),
                    source,
                    state
                )
            })
            .collect();
        if !configured.is_empty() {
            lines.push(t!("ai_panel.tool_tunnel_list_host").to_string());
            lines.extend(configured);
        }
        if lines.is_empty() {
            t!("ai_panel.tool_tunnel_list_empty").to_string()
        } else {
            lines.join("\n")
        }
    }

    /// Define a session-local tunnel. Not started, never persisted.
    fn tunnel_create(&mut self, args: &Value) -> Executed {
        let kind = TunnelKind::from_str(
            args.get("kind")
                .and_then(|kind| kind.as_str())
                .unwrap_or("dynamic"),
        );
        let name = args
            .get("name")
            .and_then(|name| name.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("agent-tunnel-{}", self.next_tunnel_id));
        let bind_port = args
            .get("bind_port")
            .and_then(|port| port.as_u64())
            .unwrap_or(0) as u16;
        let bind_addr = args
            .get("bind_addr")
            .and_then(|addr| addr.as_str())
            .unwrap_or("127.0.0.1")
            .to_string();
        let target_host = args
            .get("target_host")
            .and_then(|host| host.as_str())
            .unwrap_or("")
            .to_string();
        let target_port = args
            .get("target_port")
            .and_then(|port| port.as_u64())
            .unwrap_or(0) as u16;
        // local/remote need a target: refuse the malformed definition instead
        // of keeping something that can never start.
        if matches!(kind, TunnelKind::Local | TunnelKind::Remote)
            && (target_host.is_empty() || target_port == 0)
        {
            return Executed::Refused(t!("ai_panel.tool_bad_args").to_string());
        }
        let agent_id = self.next_tunnel_id;
        self.next_tunnel_id += 1;
        let tunnel = AgentTunnel {
            agent_id,
            kind,
            name,
            bind_addr,
            bind_port,
            target_host,
            target_port,
            state: AgentTunnelState::Created,
        };
        let handle = tunnel.handle();
        self.tunnels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tunnel);
        Executed::Done(ToolOutcome::text(
            t!("ai_panel.tool_tunnel_created", id = handle).to_string(),
        ))
    }

    /// Start one of the agent's own tunnels and wait until it is actually
    /// listening (or failed), so the model can use the port right away.
    fn tunnel_open(&self, session: &Arc<dyn AgentSession>, agent_id: u64) -> Executed {
        let spec = {
            let tunnels = self.tunnels.lock().unwrap_or_else(|e| e.into_inner());
            tunnels
                .iter()
                .find(|tunnel| tunnel.agent_id == agent_id)
                .cloned()
        };
        let Some(spec) = spec else {
            return Executed::Refused(
                t!("ai_panel.tool_tunnel_unknown", id = format!("a{agent_id}")).to_string(),
            );
        };
        if matches!(spec.state, AgentTunnelState::Running(_)) {
            return Executed::Done(ToolOutcome::text(
                t!("ai_panel.tool_tunnel_already").to_string(),
            ));
        }
        let Some(manager) = session.tunnel_manager() else {
            return Executed::Refused(t!("ai_panel.tool_no_tunnel").to_string());
        };

        let tunnels = self.tunnels.clone();
        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            let manager_for_start = manager.clone();
            let kind = spec.kind;
            let name = spec.name.clone();
            let bind_addr = spec.bind_addr.clone();
            let (bind_port, target_host, target_port) =
                (spec.bind_port, spec.target_host.clone(), spec.target_port);
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
                TOKIO.block_on(start)
            })
            .await;
            let result = match outcome {
                Ok(tunnel_id) => {
                    let started_at = Instant::now();
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
                                smol::Timer::after(std::time::Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
                Err(reason) => Err(reason),
            };
            let text = {
                let mut guard = tunnels.lock().unwrap_or_else(|e| e.into_inner());
                match guard.iter_mut().find(|tunnel| tunnel.agent_id == agent_id) {
                    Some(spec) => {
                        match &result {
                            Ok(tunnel_id) => spec.state = AgentTunnelState::Running(*tunnel_id),
                            Err(reason) => spec.state = AgentTunnelState::Failed(reason.clone()),
                        }
                        match &result {
                            Ok(_) => spec.describe(Some(&manager)),
                            Err(reason) => {
                                format!("{}: {reason}", t!("ai_panel.tool_tunnel_open_failed"))
                            }
                        }
                    }
                    None => String::new(),
                }
            };
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    /// Stop one of the agent's own tunnels. The manager's entry goes away;
    /// the definition stays and can be opened again.
    fn tunnel_close(&self, session: &Arc<dyn AgentSession>, agent_id: u64) -> Executed {
        let running = {
            let tunnels = self.tunnels.lock().unwrap_or_else(|e| e.into_inner());
            match tunnels
                .iter()
                .find(|tunnel| tunnel.agent_id == agent_id)
                .map(|tunnel| &tunnel.state)
            {
                Some(AgentTunnelState::Running(tunnel_id)) => Some(*tunnel_id),
                Some(_) => None,
                None => {
                    return Executed::Refused(
                        t!("ai_panel.tool_tunnel_unknown", id = format!("a{agent_id}")).to_string(),
                    );
                }
            }
        };
        let Some(tunnel_id) = running else {
            return Executed::Done(ToolOutcome::text(
                t!(
                    "ai_panel.tool_tunnel_not_running",
                    id = format!("a{agent_id}")
                )
                .to_string(),
            ));
        };
        let Some(manager) = session.tunnel_manager() else {
            return Executed::Refused(t!("ai_panel.tool_no_tunnel").to_string());
        };

        let tunnels = self.tunnels.clone();
        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            smol::unblock(move || TOKIO.block_on(manager.stop(tunnel_id))).await;
            {
                let mut guard = tunnels.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(spec) = guard.iter_mut().find(|tunnel| tunnel.agent_id == agent_id) {
                    spec.state = AgentTunnelState::Closed;
                }
            }
            let text = t!("ai_panel.tool_tunnel_closed", id = format!("a{agent_id}")).to_string();
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    /// Delete one of the agent's own tunnels, stopping it first when it runs.
    /// The slot stays (marked deleted) so earlier tokens remain unambiguous.
    fn tunnel_delete(&self, session: &Arc<dyn AgentSession>, agent_id: u64) -> Executed {
        let running = {
            let tunnels = self.tunnels.lock().unwrap_or_else(|e| e.into_inner());
            match tunnels
                .iter()
                .find(|tunnel| tunnel.agent_id == agent_id)
                .map(|tunnel| &tunnel.state)
            {
                Some(AgentTunnelState::Running(tunnel_id)) => Some(*tunnel_id),
                Some(_) => None,
                None => {
                    return Executed::Refused(
                        t!("ai_panel.tool_tunnel_unknown", id = format!("a{agent_id}")).to_string(),
                    );
                }
            }
        };
        let manager = running.and_then(|_| session.tunnel_manager());
        let tunnels = self.tunnels.clone();
        let (tx, rx) = bounded(1);
        smol::spawn(async move {
            if let (Some(tunnel_id), Some(manager)) = (running, manager) {
                smol::unblock(move || TOKIO.block_on(manager.stop(tunnel_id))).await;
            }
            {
                let mut guard = tunnels.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(spec) = guard.iter_mut().find(|tunnel| tunnel.agent_id == agent_id) {
                    spec.state = AgentTunnelState::Deleted;
                }
            }
            let text = t!("ai_panel.tool_tunnel_deleted", id = format!("a{agent_id}")).to_string();
            let _ = tx.send(ToolOutcome::text(text)).await;
        })
        .detach();
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }

    /// Open / close / delete a *Tunnels-page* config bound to this session's
    /// host. The UI routes the mutations through the app's own tunnel
    /// machinery; this only waits for the registry to reflect them.
    fn registry_op(&self, session: &Arc<dyn AgentSession>, name: &str, config_id: i64) -> Executed {
        let token = format!("t{config_id}");
        let known = session
            .registry_tunnels()
            .iter()
            .any(|view| view.id == config_id);
        if !known {
            return Executed::Refused(t!("ai_panel.tool_tunnel_unknown", id = token).to_string());
        }
        let session = session.clone();
        let (tx, rx) = bounded(1);
        match name {
            "tunnel_open" => {
                if session.registry_is_running(config_id) {
                    return Executed::Done(ToolOutcome::text(
                        t!("ai_panel.tool_tunnel_already").to_string(),
                    ));
                }
                session.registry_open(config_id);
                smol::spawn(async move {
                    let started_at = Instant::now();
                    let mut running = false;
                    while started_at.elapsed() < TUNNEL_START_TIMEOUT {
                        if session.registry_is_running(config_id) {
                            running = true;
                            break;
                        }
                        smol::Timer::after(std::time::Duration::from_millis(120)).await;
                    }
                    let text = match (running, session.registry_live(config_id)) {
                        (true, Some((bind_addr, bind_port, target_host, target_port))) => {
                            let mut text = format!(
                                "{token} — {}",
                                t!(
                                    "ai_panel.tunnel_running_at",
                                    bind = format!("{bind_addr}:{bind_port}")
                                )
                            );
                            if !target_host.is_empty() && target_port != 0 {
                                text.push_str(&format!(" -> {target_host}:{target_port}"));
                            }
                            text
                        }
                        (true, None) => token.clone(),
                        (false, _) => format!(
                            "{}: {}",
                            t!("ai_panel.tool_tunnel_open_failed"),
                            t!("ai_panel.tunnel_start_timeout")
                        ),
                    };
                    let _ = tx.send(ToolOutcome::text(text)).await;
                })
                .detach();
            }
            "tunnel_close" => {
                if !session.registry_is_running(config_id) {
                    return Executed::Done(ToolOutcome::text(
                        t!("ai_panel.tool_tunnel_not_running", id = token).to_string(),
                    ));
                }
                session.registry_close(config_id);
                smol::spawn(async move {
                    let started_at = Instant::now();
                    while session.registry_is_running(config_id)
                        && started_at.elapsed() < TUNNEL_START_TIMEOUT
                    {
                        smol::Timer::after(std::time::Duration::from_millis(120)).await;
                    }
                    let text = t!("ai_panel.tool_tunnel_closed", id = token).to_string();
                    let _ = tx.send(ToolOutcome::text(text)).await;
                })
                .detach();
            }
            _ => {
                session.registry_delete(config_id);
                smol::spawn(async move {
                    let started_at = Instant::now();
                    while session
                        .registry_tunnels()
                        .iter()
                        .any(|view| view.id == config_id)
                        && started_at.elapsed() < TUNNEL_START_TIMEOUT
                    {
                        smol::Timer::after(std::time::Duration::from_millis(120)).await;
                    }
                    let text = t!("ai_panel.tool_tunnel_deleted", id = token).to_string();
                    let _ = tx.send(ToolOutcome::text(text)).await;
                })
                .detach();
            }
        }
        Executed::Pending {
            outcome: rx,
            cancel: ExecCancel::default(),
        }
    }
}

/// The `command` argument both execution tools take.
fn command_arg(args: &Value) -> Option<String> {
    args.get("command")
        .and_then(|command| command.as_str())
        .map(str::to_string)
}

/// `"a12"` → `('a', Some(12))`; malformed tokens yield no number.
fn split_token(token: &str) -> (char, Option<u64>) {
    (
        token.chars().next().unwrap_or(' '),
        token[1..].parse::<u64>().ok(),
    )
}
