//! `SftpTabView` struct definition, non-render `impl` methods, top-level
//! `Render`, and `CrabPortTab` impl. Panel rendering (`render_panel`)
//! lives in [`super::panel`] as a separate impl block. Free helpers
//! (`render_panel_ellipsis_button`, `trigger_*`) live in [`super::helpers`].

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::InputState;
use rust_i18n::t;
use rustc_hash::FxHashSet;

use crate::app::{CrabPortTab, CrabportApp};
use crate::components::context_menu::ContextMenuController;
use crate::components::dialog::AlertController;
use crate::components::host_selector::{HostSelectorOverlay, PanelSide};
use crate::views::sessions::ConnectionHost;
use crate::views::terminal::{SftpProgress, TerminalView};

use crabport_core::credential::{CredentialKind as CoreCredentialKind, HostKind as CoreHostKind};
use crabport_ssh::backend::SshBackend;
use crabport_ssh::session::SshConnectionInfo;
use crabport_terminal::terminal::SftpTransferKind;

/// What a panel is connected to.
///
/// A saved host becomes a session when connected: `Disconnected` and
/// `Local` have no SSH session, while `Remote` wraps a live `TerminalView`
/// that represents an established session.
pub enum PanelHost {
    /// Not connected to any host. Shows a placeholder with a "Select Host" button.
    Disconnected,
    /// Local filesystem.
    Local,
    /// Remote SSH host, driven by a hidden `TerminalView`.
    Remote {
        terminal: Entity<TerminalView>,
        host_name: String,
    },
}

impl PanelHost {
    pub fn is_remote(&self) -> bool {
        matches!(self, PanelHost::Remote { .. })
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self, PanelHost::Disconnected)
    }

    pub fn terminal(&self) -> Option<&Entity<TerminalView>> {
        match self {
            PanelHost::Remote { terminal, .. } => Some(terminal),
            _ => None,
        }
    }
}

/// Per-panel state (left or right). Encapsulates everything a panel
/// needs: its host connection, entries, selection, path input, and
/// SFTP callbacks.
pub(super) struct PanelState {
    pub host: PanelHost,
    pub local_cwd: PathBuf,
    pub local_entries: Vec<crabport_sftp::FileEntry>,
    pub remote_cwd: Option<Arc<String>>,
    pub remote_entries: Arc<Vec<crabport_sftp::FileEntry>>,
    pub path_input: Option<Entity<InputState>>,
    /// Last path string written into the input by `sync_path_input`. Used to
    /// tell a programmatic overwrite (cwd changed underneath the user) apart
    /// from the user actively typing in the input — without this, every
    /// keystroke would trigger a render, and `sync_path_input` would clobber
    /// the half-typed text.
    pub last_synced_path: Option<String>,
    /// Whether hidden files (Unix dotfiles) are shown in the list. Toggled
    /// by the ellipsis menu's "show hidden files" action.
    pub show_hidden: bool,
    pub hovered: Option<String>,
    pub selected: FxHashSet<String>,
    pub context_menu_entry: Option<String>,
    pub drag_over: bool,
    pub scroll: VirtualListScrollHandle,
    pub renaming: Option<String>,
    pub rename_input: Option<Entity<InputState>>,
    /// When `Some`, an inline "new folder" input is shown at the top of the
    /// file list. Holds the placeholder name to seed the input with.
    /// `None` means no mkdir is in progress.
    pub mkdir_pending: Option<()>,
    pub mkdir_input: Option<Entity<InputState>>,
    /// Monotonic connection counter. Incremented every time a new SSH
    /// connection is established on this panel. Used in the connection
    /// overlay's transition ID so the fade-in/fade-out animation replays
    /// on every connection attempt instead of being cached from the
    /// previous one.
    pub connect_count: u64,
    pub on_navigate: Option<Rc<dyn Fn(String, &mut App)>>,
    pub on_download: Option<Rc<dyn Fn(String, String, &mut App)>>,
    pub on_upload: Option<Rc<dyn Fn(String, String, &mut App)>>,
    pub on_upload_batch: Option<Rc<dyn Fn(Vec<(String, String)>, &mut App)>>,
    pub on_delete: Option<Rc<dyn Fn(String, &mut App)>>,
    pub on_rename: Option<Rc<dyn Fn(String, String, &mut App)>>,
    pub on_edit: Option<Rc<dyn Fn(String, &mut App)>>,
}

impl PanelState {
    fn new_local() -> Self {
        Self {
            host: PanelHost::Local,
            local_cwd: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            local_entries: Vec::new(),
            remote_cwd: None,
            remote_entries: Arc::new(Vec::new()),
            path_input: None,
            last_synced_path: None,
            show_hidden: false,
            hovered: None,
            selected: FxHashSet::default(),
            context_menu_entry: None,
            drag_over: false,
            scroll: VirtualListScrollHandle::new(),
            renaming: None,
            rename_input: None,
            mkdir_pending: None,
            mkdir_input: None,
            connect_count: 0,
            on_navigate: None,
            on_download: None,
            on_upload: None,
            on_upload_batch: None,
            on_delete: None,
            on_rename: None,
            on_edit: None,
        }
    }

    fn new_disconnected() -> Self {
        Self {
            host: PanelHost::Disconnected,
            local_cwd: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            local_entries: Vec::new(),
            remote_cwd: None,
            remote_entries: Arc::new(Vec::new()),
            path_input: None,
            last_synced_path: None,
            show_hidden: false,
            hovered: None,
            selected: FxHashSet::default(),
            context_menu_entry: None,
            drag_over: false,
            scroll: VirtualListScrollHandle::new(),
            renaming: None,
            rename_input: None,
            mkdir_pending: None,
            mkdir_input: None,
            connect_count: 0,
            on_navigate: None,
            on_download: None,
            on_upload: None,
            on_upload_batch: None,
            on_delete: None,
            on_rename: None,
            on_edit: None,
        }
    }

    /// Clear selection + rename state.
    fn clear_interaction(&mut self) {
        self.selected.clear();
        self.renaming = None;
        self.context_menu_entry = None;
        self.hovered = None;
        self.mkdir_pending = None;
    }
}

/// Full-screen dual-panel SFTP file browser.
///
/// Each panel (left + right) can independently be connected to the local
/// filesystem or a remote SSH host. Host switching happens in-place via
/// [`SftpTabView::switch_panel_host`].
pub struct SftpTabView {
    pub(super) left: PanelState,
    pub(super) right: PanelState,

    // --- Shared controllers (injected via set_state) ---
    pub(super) context_menu: Option<Entity<ContextMenuController>>,
    pub(super) alert_controller: Option<Entity<AlertController>>,
    pub(super) tooltip: Option<Entity<crate::components::tooltip::TooltipController>>,
    pub(super) transfer_history: Option<Entity<crate::views::sftp::TransferHistoryController>>,

    // --- Host selector overlay ---
    pub(super) host_selector: Option<Entity<HostSelectorOverlay>>,
    pub(super) host_selector_open_for: Option<PanelSide>,
    pub(super) hosts: Vec<ConnectionHost>,
    pub(super) app: Option<Entity<CrabportApp>>,
}

impl SftpTabView {
    pub fn new() -> Self {
        Self {
            left: PanelState::new_local(),
            right: PanelState::new_disconnected(),
            context_menu: None,
            alert_controller: None,
            tooltip: None,
            transfer_history: None,
            host_selector: None,
            host_selector_open_for: None,
            hosts: Vec::new(),
            app: None,
        }
    }

    /// Read the local filesystem entries for a given path.
    /// Returns `FileEntry` tuples, sorted directories-first then
    /// alphabetically. Does NOT prepend `..` — that's added at render
    /// time so the entries vec stays a pure mirror of the directory.
    pub(super) fn read_local_dir(path: &Path) -> Vec<crabport_sftp::FileEntry> {
        Self::read_local_dir_filtered(path, false)
    }

    /// Read a local directory, optionally including hidden (dot-prefixed)
    /// entries. Sorting is the same as [`Self::read_local_dir`].
    pub(super) fn read_local_dir_filtered(
        path: &Path,
        _show_hidden: bool,
    ) -> Vec<crabport_sftp::FileEntry> {
        use std::time::UNIX_EPOCH;
        let mut out: Vec<crabport_sftp::FileEntry> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                // Skip hidden files on Unix for a cleaner listing, unless the
                // user has explicitly enabled "show hidden files".
                #[cfg(unix)]
                if !_show_hidden && name.starts_with('.') {
                    continue;
                }
                let size = metadata.as_ref().filter(|m| !m.is_dir()).map(|m| m.len());
                #[cfg(unix)]
                let permissions = metadata.as_ref().map(|m| {
                    use std::os::unix::fs::MetadataExt;
                    format_mode(m.mode())
                });
                #[cfg(not(unix))]
                let permissions = None;
                let modified = metadata
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                out.push(crabport_sftp::FileEntry {
                    name,
                    is_dir,
                    size,
                    permissions,
                    modified,
                });
            }
        }
        out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        out
    }

    /// Navigate the local panel to `path` and refresh entries.
    pub(super) fn local_navigate(
        &mut self,
        side: PanelSide,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let panel = self.panel_mut(side);
        if path.is_dir() {
            panel.local_cwd = path;
            panel.local_entries =
                Self::read_local_dir_filtered(&panel.local_cwd, panel.show_hidden);
            panel.selected.clear();
            panel.renaming = None;
            // Remember the new cwd as the last synced path so the input
            // overwrite guard in `sync_path_input` doesn't fight the user.
            panel.last_synced_path = Some(panel.local_cwd.to_string_lossy().into_owned());
            cx.notify();
        }
    }

    /// Navigate a remote panel by invoking `on_navigate`.
    pub(super) fn remote_navigate(&self, side: PanelSide, path: String, cx: &mut App) {
        let panel = self.panel(side);
        if let Some(ref cb) = panel.on_navigate {
            cb(path, cx);
        }
    }

    /// Get a reference to the panel for the given side.
    pub(super) fn panel(&self, side: PanelSide) -> &PanelState {
        match side {
            PanelSide::Left => &self.left,
            PanelSide::Right => &self.right,
        }
    }

    /// Check if the given panel is already connected to the specified host.
    /// Used by the session right-click "Connect via SFTP" to avoid
    /// reconnecting when the panel is already on that host.
    pub fn is_panel_connected_to(&self, side: PanelSide, host_id: i64, cx: &App) -> bool {
        match &self.panel(side).host {
            PanelHost::Remote { terminal, .. } => {
                terminal.read_with(cx, |t, _cx| t.host_id()) == Some(host_id)
            }
            _ => false,
        }
    }

    /// Live SFTP transfer progress from whichever panel has an active
    /// transfer, or `None` if neither does. Read by `content.rs` to drive
    /// the shared bottom toolbar so the animation persists across
    /// terminal ↔ SFTP tab switches.
    pub fn sftp_progress(&self, cx: &App) -> Option<SftpProgress> {
        let left = self
            .left
            .host
            .terminal()
            .and_then(|t| t.read_with(cx, |v, _| v.sftp_progress().cloned()));
        let right = self
            .right
            .host
            .terminal()
            .and_then(|t| t.read_with(cx, |v, _| v.sftp_progress().cloned()));
        left.or(right)
    }

    /// Get a mutable reference to the panel for the given side.
    pub(super) fn panel_mut(&mut self, side: PanelSide) -> &mut PanelState {
        match side {
            PanelSide::Left => &mut self.left,
            PanelSide::Right => &mut self.right,
        }
    }

    /// Push per-render state from `content.rs`.
    ///
    /// The SftpTabView reads SFTP entries / cwd from each panel's own
    /// `TerminalView` internally. This method only stashes the shared
    /// controllers + host list + app handle.
    pub fn set_state(
        &mut self,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        tooltip: Entity<crate::components::tooltip::TooltipController>,
        transfer_history: Entity<crate::views::sftp::TransferHistoryController>,
        hosts: Vec<ConnectionHost>,
        app: Entity<CrabportApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // --- Lazily create the host selector overlay on first call ---
        if self.host_selector.is_none() {
            let entity = cx.new(|cx| HostSelectorOverlay::new(window, cx));

            // on_close: hide the overlay.
            let close_entity = entity.clone();
            entity.update(cx, |overlay, _cx| {
                overlay.set_on_close(move |_w, cx| {
                    close_entity.update(cx, |o, cx| o.close(cx));
                });
            });

            // Wire on_select. The captured `app` handle lets us activate
            // the SFTP tab and switch the target panel's host in-place.
            //
            // We call `sftp_view.update()` directly (not `app.update()`)
            // to avoid re-entering the CrabportApp entity lock, which
            // would panic with "cannot read while it is already being
            // updated" if the callback fires during a render cycle.
            // Tab activation is deferred via `app_for_cb` only when
            // called from the Sessions view (not from the in-panel
            // selector, where the SFTP tab is already active).
            let self_entity = cx.entity();
            let close_entity_for_select = entity.clone();
            entity.update(cx, |overlay, cx| {
                overlay.set_on_select(move |host_id, w, cx| {
                    // Determine which panel opened the selector.
                    let side = self_entity
                        .read_with(cx, |view, _cx| view.host_selector_open_for)
                        .unwrap_or(PanelSide::Right);
                    // Call switch_panel_host directly on SftpTabView —
                    // this is safe because SftpTabView is a different
                    // entity from CrabportApp.
                    self_entity.update(cx, |view, cx| {
                        view.switch_panel_host(side, host_id, w, cx);
                    });
                    // Close the overlay regardless of selection.
                    close_entity_for_select.update(cx, |o, cx| o.close(cx));
                });
                cx.notify();
            });

            self.host_selector = Some(entity);
        }
        self.hosts = hosts;
        self.app = Some(app);
        self.transfer_history = Some(transfer_history);
        // Keep the overlay's host list in sync.
        if let Some(ref overlay) = self.host_selector {
            overlay.update(cx, |o, cx| {
                o.set_hosts(self.hosts.clone());
                cx.notify();
            });
        }

        // --- Refresh each panel's local entries (if local) ---
        self.refresh_local_entries_if_local(PanelSide::Left);
        self.refresh_local_entries_if_local(PanelSide::Right);

        // --- Lazily init path input states for each panel ---
        self.ensure_path_input(PanelSide::Left, window, cx);
        self.ensure_path_input(PanelSide::Right, window, cx);

        // --- Read remote state from each panel's terminal ---
        self.sync_remote_state(PanelSide::Left, cx);
        self.sync_remote_state(PanelSide::Right, cx);

        // --- Sync path inputs with current cwd ---
        self.sync_path_input(PanelSide::Left, window, cx);
        self.sync_path_input(PanelSide::Right, window, cx);

        // Stash shared controllers.
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        self.tooltip = Some(tooltip);
    }

    /// Refresh local directory entries for a panel if it's in Local mode.
    fn refresh_local_entries_if_local(&mut self, side: PanelSide) {
        let panel = self.panel_mut(side);
        if matches!(panel.host, PanelHost::Local) {
            panel.local_entries =
                Self::read_local_dir_filtered(&panel.local_cwd, panel.show_hidden);
        }
    }

    /// Lazily create the path input for a panel if it doesn't exist yet.
    fn ensure_path_input(&mut self, side: PanelSide, window: &mut Window, cx: &mut Context<Self>) {
        let needs_init = self.panel(side).path_input.is_none();
        if !needs_init {
            return;
        }

        let initial_local = self.panel(side).local_cwd.to_string_lossy().into_owned();
        let entity = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial_local)
                .placeholder("/path/to/dir")
        });
        cx.subscribe(
            &entity,
            move |this, input, event: &gpui_component::input::InputEvent, cx| {
                if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                    let path = input.read(cx).value().to_string();
                    if !path.is_empty() {
                        // Check if the panel is remote or local and dispatch
                        // accordingly.
                        if this.panel(side).host.is_remote() {
                            this.remote_navigate(side, path, cx);
                        } else {
                            this.local_navigate(side, PathBuf::from(path), cx);
                        }
                    }
                }
            },
        )
        .detach();
        let blur_handle = entity.read(cx).focus_handle(cx);
        cx.on_blur(&blur_handle, window, move |this, window, cx| {
            let input = this.panel(side).path_input.clone();
            if let Some(ref input) = input {
                let panel = this.panel(side);
                let val = match panel.host {
                    PanelHost::Local => panel.local_cwd.to_string_lossy().into_owned(),
                    PanelHost::Remote { .. } => panel
                        .remote_cwd
                        .as_ref()
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "/".to_string()),
                    PanelHost::Disconnected => "".to_string(),
                };
                input.update(cx, |state, cx| {
                    state.set_value(val, window, cx);
                });
            }
        })
        .detach();
        self.panel_mut(side).path_input = Some(entity);
    }

    /// Read remote SFTP entries / cwd from the panel's `TerminalView`
    /// (if it's remote) and update the panel state + path input.
    fn sync_remote_state(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        let terminal = match self.panel(side).host.terminal().cloned() {
            Some(t) => t,
            None => return,
        };
        let (remote_entries, remote_cwd): (
            Arc<Vec<crabport_sftp::FileEntry>>,
            Option<Arc<String>>,
        ) = terminal.read_with(cx, |view, _cx| {
            (view.sftp_entries().unwrap_or_default(), view.sftp_cwd())
        });

        let panel = self.panel_mut(side);

        // Detect listing change for selection invalidation
        let remote_entries_changed = if Arc::ptr_eq(&panel.remote_entries, &remote_entries) {
            false
        } else {
            let prev: Vec<&str> = panel
                .remote_entries
                .iter()
                .map(|e| e.name.as_str())
                .collect();
            let next: Vec<&str> = remote_entries.iter().map(|e| e.name.as_str()).collect();
            prev != next
        };

        panel.remote_cwd = remote_cwd;
        panel.remote_entries = remote_entries;

        if remote_entries_changed {
            panel.selected.clear();
            panel.renaming = None;
        }
    }

    /// Sync a panel's path input text with its current cwd.
    /// Called each render after `sync_remote_state`.
    ///
    /// We only overwrite the input when the cwd moved away from what we last
    /// wrote into the input — i.e. a programmatic change (navigation, host
    /// switch). If the user is mid-edit the live text will differ from the
    /// last synced value, and we must NOT clobber it. Otherwise every
    /// keystroke would trigger a render and reset the input.
    fn sync_path_input(&mut self, side: PanelSide, window: &mut Window, _cx: &mut Context<Self>) {
        let panel = self.panel_mut(side);
        let Some(ref input) = panel.path_input else {
            return;
        };
        let actual = match &panel.host {
            PanelHost::Local => panel.local_cwd.to_string_lossy().into_owned(),
            PanelHost::Remote { .. } => panel
                .remote_cwd
                .as_ref()
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|| "/".to_string()),
            PanelHost::Disconnected => "".to_string(),
        };
        // If we have a previous synced value and the cwd hasn't moved past
        // it, the user is editing — leave the input alone.
        if let Some(ref last) = panel.last_synced_path {
            if *last == actual {
                // Cwd is still what we last wrote. Either the user is editing
                // (don't overwrite) or nothing changed (no-op either way).
                return;
            }
        }
        // Cwd moved. Overwrite the input and remember the new synced value.
        let cur = input.read(_cx).value().to_string();
        if cur != actual {
            input.update(_cx, |state, cx| {
                state.set_value(actual.clone(), window, cx);
            });
        }
        panel.last_synced_path = Some(actual);
    }

    // --- Rename helpers (remote) ---

    pub(super) fn start_remote_rename(
        &mut self,
        side: PanelSide,
        entry_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.panel_mut(side);
        panel.renaming = Some(entry_name.clone());
        if panel.rename_input.is_none() {
            let entity = cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder("new name");
                state.focus(window, cx);
                state
            });
            cx.subscribe(
                &entity,
                move |this, _input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        this.commit_remote_rename(side, cx);
                    }
                },
            )
            .detach();
            let blur_handle = entity.read(cx).focus_handle(cx);
            cx.on_blur(&blur_handle, window, move |this, _window, cx| {
                if this.panel(side).renaming.is_some() {
                    this.cancel_remote_rename(side, cx);
                }
            })
            .detach();
            panel.rename_input = Some(entity);
        }
        let panel = self.panel_mut(side);
        if let Some(ref input) = panel.rename_input {
            input.update(cx, |state, cx| {
                state.set_value(entry_name, window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn commit_remote_rename(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        let new_name = self.panel(side).rename_input.as_ref().and_then(|input| {
            let v = input.read(cx).value().to_string();
            if v.is_empty() { None } else { Some(v) }
        });
        let Some(new_name) = new_name else { return };
        let panel = self.panel_mut(side);
        let Some(entry_name) = panel.renaming.take() else {
            return;
        };
        let cwd_str = panel
            .remote_cwd
            .as_ref()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        let old_path = join_remote_path(&cwd_str, &entry_name);
        let new_path = join_remote_path(&cwd_str, &new_name);
        let cb = panel.on_rename.clone();
        if new_name == entry_name {
            panel.selected.clear();
            cx.notify();
            return;
        }
        if let Some(ref cb) = cb {
            let cb = cb.clone();
            let old = old_path.clone();
            let new = new_path.clone();
            cx.defer(move |cx| cb(old, new, cx));
        }
        let panel = self.panel_mut(side);
        panel.selected.clear();
        cx.notify();
    }

    pub(super) fn cancel_remote_rename(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        self.panel_mut(side).renaming = None;
        cx.notify();
    }

    // --- Rename helpers (local) ---

    pub(super) fn start_local_rename(
        &mut self,
        side: PanelSide,
        entry_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.panel_mut(side);
        panel.renaming = Some(entry_name.clone());
        if panel.rename_input.is_none() {
            let entity = cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder("new name");
                state.focus(window, cx);
                state
            });
            cx.subscribe(
                &entity,
                move |this, _input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        this.commit_local_rename(side, cx);
                    }
                },
            )
            .detach();
            let blur_handle = entity.read(cx).focus_handle(cx);
            cx.on_blur(&blur_handle, window, move |this, _window, cx| {
                if this.panel(side).renaming.is_some() {
                    this.cancel_local_rename(side, cx);
                }
            })
            .detach();
            panel.rename_input = Some(entity);
        }
        let panel = self.panel_mut(side);
        if let Some(ref input) = panel.rename_input {
            input.update(cx, |state, cx| {
                state.set_value(entry_name, window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn commit_local_rename(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        let new_name = self.panel(side).rename_input.as_ref().and_then(|input| {
            let v = input.read(cx).value().to_string();
            if v.is_empty() { None } else { Some(v) }
        });
        let Some(new_name) = new_name else { return };
        let panel = self.panel_mut(side);
        let Some(entry_name) = panel.renaming.take() else {
            return;
        };
        if new_name == entry_name {
            panel.selected.clear();
            cx.notify();
            return;
        }
        let old_path = panel.local_cwd.join(&entry_name);
        let new_path = panel.local_cwd.join(&new_name);
        if let Err(e) = std::fs::rename(&old_path, &new_path) {
            tracing::warn!("local rename failed: {e}");
        }
        let panel = self.panel_mut(side);
        panel.local_entries = Self::read_local_dir(&panel.local_cwd);
        panel.selected.clear();
        cx.notify();
    }

    pub(super) fn cancel_local_rename(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        self.panel_mut(side).renaming = None;
        cx.notify();
    }

    // --- Host switching ---

    /// Resolve credentials for a host by ID. Returns all the info needed
    /// to create an `SshBackend` + `TerminalView`.
    ///
    /// Reads directly from the Store (not the `CrabportApp` entity) so this
    /// method is safe to call from within `app.update` — e.g. when the
    /// session right-click "Connect via SFTP" invokes
    /// `switch_sftp_panel_host` inside an `app.update` callback.
    fn resolve_host_credentials(
        &self,
        host_id: i64,
        cx: &mut Context<Self>,
    ) -> Option<ResolvedHost> {
        let store = crate::app_state::AppState::store(cx);

        let host = store.lock().find_host(host_id).ok().flatten()?;

        // Only SSH hosts support SFTP.
        let host_kind = match host.kind {
            CoreHostKind::Ssh => crate::views::sessions::ConnectionKind::SSH,
            CoreHostKind::Telnet => crate::views::sessions::ConnectionKind::Telnet,
            CoreHostKind::Serial => crate::views::sessions::ConnectionKind::Serial,
        };
        if host_kind != crate::views::sessions::ConnectionKind::SSH {
            return None;
        }

        let _ = store.lock().touch_host_login(host_id);

        // Refresh hosts list in the app. Deferred so we don't re-enter the
        // `CrabportApp` entity lock when called from within `app.update`.
        if let Ok(all) = store.lock().hosts() {
            if let Some(ref app) = self.app {
                let app = app.clone();
                cx.defer(move |cx| {
                    app.update(cx, |a, _cx| {
                        a.hosts = all
                            .into_iter()
                            .map(|h| ConnectionHost {
                                id: h.id,
                                name: h.name,
                                host: h.host,
                                port: h.port,
                                username: h.username,
                                kind: match h.kind {
                                    CoreHostKind::Ssh => {
                                        crate::views::sessions::ConnectionKind::SSH
                                    }
                                    CoreHostKind::Telnet => {
                                        crate::views::sessions::ConnectionKind::Telnet
                                    }
                                    CoreHostKind::Serial => {
                                        crate::views::sessions::ConnectionKind::Serial
                                    }
                                },
                                credential_id: h.credential_id,
                                last_login: h.last_login,
                                favorite: h.favorite,
                                proxy_id: h.proxy_id,
                                group_id: h.group_id,
                            })
                            .collect();
                    });
                });
            }
        }

        let cred = host
            .credential_id
            .and_then(|cid| store.lock().find_credential(cid).ok().flatten());

        let (password, private_key, passphrase) = match cred.as_ref() {
            Some(c) if c.kind == CoreCredentialKind::Certificate => (
                String::new(),
                if c.private_key.is_empty() {
                    None
                } else {
                    Some(c.private_key.as_str())
                },
                if c.secret.is_empty() {
                    None
                } else {
                    Some(c.secret.as_str())
                },
            ),
            Some(c) => (c.secret.clone(), None, None),
            None => (String::new(), None, None),
        };

        let proxy_config = host
            .proxy_id
            .and_then(|pid| store.lock().find_proxy_config(pid).ok().flatten());

        Some(ResolvedHost {
            name: host.name,
            host: host.host,
            port: host.port,
            username: host.username,
            password,
            private_key: private_key.map(|s| s.to_string()),
            passphrase: passphrase.map(|s| s.to_string()),
            proxy: proxy_config,
            startup_command: host.startup_command,
        })
    }

    /// Switch a panel to a different host (or Local if `host_id` is None).
    ///
    /// This creates a new `TerminalView` with an `SshBackend` for remote
    /// hosts, or switches the panel back to local filesystem browsing.
    pub fn switch_panel_host(
        &mut self,
        side: PanelSide,
        host_id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match host_id {
            Some(hid) => {
                let Some(resolved) = self.resolve_host_credentials(hid, cx) else {
                    return;
                };
                self.connect_panel_remote(side, resolved, hid, window, cx);
            }
            None => {
                self.connect_panel_local(side, window, cx);
            }
        }
    }

    /// Connect a panel to a remote SSH host.
    fn connect_panel_remote(
        &mut self,
        side: PanelSide,
        resolved: ResolvedHost,
        host_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Close the old terminal if it was remote.
        let old = std::mem::replace(&mut self.panel_mut(side).host, PanelHost::Local);
        if let PanelHost::Remote { terminal, .. } = old {
            terminal.update(cx, |v, _cx| {
                v.close();
            });
        }

        // Increment the connection counter so the connection overlay's
        // transition ID changes — this forces gpui-animation to replay the
        // fade-in/fade-out instead of using the cached state from the
        // previous connection.
        self.panel_mut(side).connect_count += 1;

        // Build SSH backend + TerminalView (mirrors add_sftp_tab logic)
        let mut info =
            SshConnectionInfo::new(&resolved.host, &resolved.username, &resolved.password)
                .with_port(resolved.port);
        if let Some(ref pk) = resolved.private_key {
            info = info.with_private_key(pk, resolved.passphrase.clone());
        }
        if let Some(p) = resolved.proxy {
            info = info.with_proxy(p);
        }
        if !resolved.startup_command.is_empty() {
            info = info.with_startup_command(&resolved.startup_command);
        }
        let info_for_view = info.clone();
        let cols: usize = 80;
        let rows: usize = 24;

        let overlay: crate::views::terminal::connection_overlay::SharedOverlayState =
            Arc::new(parking_lot::Mutex::new(
                crate::views::terminal::connection_overlay::ConnectionOverlayState::new(),
            ));
        let overlay_cb = overlay.clone();
        let verifier =
            crate::views::terminal::connection_overlay::make_host_key_verifier(overlay.clone());

        let backend = Arc::new(SshBackend::new(
            info,
            cols as u16,
            rows as u16,
            Arc::new(move |msg: String| {
                overlay_cb.lock().log(
                    crate::views::terminal::connection_overlay::ConnectionLogLevel::Info,
                    msg,
                );
            }),
            Some(verifier),
        ));

        // Use a pseudo pane id for this SFTP panel. We derive it from the
        // side to avoid collisions: left=100000, right=100001.
        let pane_id = match side {
            PanelSide::Left => 100_000u64,
            PanelSide::Right => 100_001u64,
        };

        let terminal_view = cx.new(|cx| {
            TerminalView::with_backend_and_host_and_overlay(
                backend,
                cols,
                rows,
                format!("{}@{}", resolved.username, resolved.host),
                Some(host_id),
                overlay,
                Some(info_for_view),
                None,
                None, // no SerialConnectionInfo for SFTP
                pane_id,
                cx,
            )
        });

        // Wire callbacks for SFTP progress + transfer notifications.
        let app_handle = self.app.clone().map(|e| e.downgrade());
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_progress_changed(move |cx| {
                if let Some(ref ah) = app_handle {
                    let _ = ah.update(cx, |_, cx| cx.notify());
                }
            });
        });

        let app_handle = self.app.clone().map(|e| e.downgrade());
        let transfer_history_ctrl = self.transfer_history.clone();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_transfer_finished(move |kind, success, message, cx| {
                // Append to the global transfer-history controller so
                // the popover can display it. Done before the early-return
                // arms below (Rename/Edit on success skip the toast but
                // should still be logged in history).
                let record = crate::views::sftp::TransferRecord {
                    kind,
                    success,
                    message: message.clone(),
                    finished_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                };
                if let Some(ref ctrl) = transfer_history_ctrl {
                    let _ = ctrl.update(cx, |c, cx| {
                        c.push(record, cx);
                    });
                }
                let Some(ref ah) = app_handle else { return };
                let _ = ah.update(cx, |app, cx| {
                    let (title, message_notif, level, duration) = match (kind, success) {
                        (SftpTransferKind::Download, true) => (
                            t!("sftp.notif_download_done_title").to_string(),
                            t!("sftp.notif_download_done_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Download, false) => (
                            t!("sftp.notif_download_failed_title").to_string(),
                            t!("sftp.notif_download_failed_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Upload, true) => (
                            t!("sftp.notif_upload_done_title").to_string(),
                            t!("sftp.notif_upload_done_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Upload, false) => (
                            t!("sftp.notif_upload_failed_title").to_string(),
                            t!("sftp.notif_upload_failed_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Rename, true) => return,
                        (SftpTransferKind::Rename, false) => (
                            t!("sftp.notif_rename_failed_title").to_string(),
                            t!("sftp.notif_rename_failed_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Edit, true) => return,
                        (SftpTransferKind::Edit, false) => (
                            t!("sftp.notif_edit_save_failed_title").to_string(),
                            t!(
                                "sftp.notif_edit_save_failed_msg",
                                message = message.as_str()
                            )
                            .to_string(),
                            crate::components::notification::NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Delete, true) => (
                            t!("sftp.notif_delete_done_title").to_string(),
                            t!("sftp.notif_delete_done_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Delete, false) => (
                            t!("sftp.notif_delete_failed_title").to_string(),
                            t!("sftp.notif_delete_failed_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        // Mkdir: silent on success (the listing refresh surfaces
                        // the new folder); only failures toast.
                        (SftpTransferKind::Mkdir, true) => return,
                        (SftpTransferKind::Mkdir, false) => (
                            t!("sftp.notif_mkdir_failed_title").to_string(),
                            t!("sftp.notif_mkdir_failed_msg", message = message.as_str())
                                .to_string(),
                            crate::components::notification::NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                    };
                    app.app_ctx.notifications.update(cx, |c, cx| {
                        c.show(
                            crate::components::notification::Notification::new(title)
                                .level(level)
                                .message(message_notif)
                                .duration(duration),
                            cx,
                        );
                    });
                    cx.notify();
                });
            });
        });

        // Wire SFTP callbacks for this panel.
        let term = terminal_view.clone();
        let on_navigate: Rc<dyn Fn(String, &mut App)> = Rc::new({
            let term = term.clone();
            move |path: String, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_navigate(&path);
                });
            }
        });
        let on_download: Rc<dyn Fn(String, String, &mut App)> = Rc::new({
            let term = term.clone();
            move |remote_path: String, local_path: String, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_download(&remote_path, &local_path);
                });
            }
        });
        let on_upload: Rc<dyn Fn(String, String, &mut App)> = Rc::new({
            let term = term.clone();
            move |local_path: String, remote_path: String, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_upload(&local_path, &remote_path);
                });
            }
        });
        let on_upload_batch: Rc<dyn Fn(Vec<(String, String)>, &mut App)> = Rc::new({
            let term = term.clone();
            move |items: Vec<(String, String)>, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_upload_batch(&items);
                });
            }
        });
        let on_delete: Rc<dyn Fn(String, &mut App)> = Rc::new({
            let term = term.clone();
            move |remote_path: String, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_delete(&remote_path);
                });
            }
        });
        let on_rename: Rc<dyn Fn(String, String, &mut App)> = Rc::new({
            let term = term.clone();
            move |old_path: String, new_path: String, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_rename(&old_path, &new_path);
                });
            }
        });
        let on_edit: Rc<dyn Fn(String, &mut App)> = Rc::new({
            let term = term.clone();
            move |remote_path: String, cx: &mut App| {
                term.read_with(cx, |view, _cx| {
                    view.sftp_open_in_editor(&remote_path);
                });
            }
        });

        let panel = self.panel_mut(side);
        panel.host = PanelHost::Remote {
            terminal: terminal_view,
            host_name: resolved.name,
        };
        panel.remote_cwd = None;
        panel.remote_entries = Arc::new(Vec::new());
        panel.clear_interaction();
        panel.on_navigate = Some(on_navigate);
        panel.on_download = Some(on_download);
        panel.on_upload = Some(on_upload);
        panel.on_upload_batch = Some(on_upload_batch);
        panel.on_delete = Some(on_delete);
        panel.on_rename = Some(on_rename);
        panel.on_edit = Some(on_edit);

        // Sync path input to the (empty) remote cwd
        if let Some(ref input) = panel.path_input {
            input.update(cx, |state, cx| {
                state.set_value("/".to_string(), window, cx);
            });
        }
        panel.last_synced_path = Some("/".to_string());

        // We need `window` to persist for future renders; tell GPUI to
        // keep the window's frame pump alive by rendering the terminal.
        cx.notify();
    }

    /// Connect a panel to the local filesystem.
    fn connect_panel_local(
        &mut self,
        side: PanelSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = std::mem::replace(&mut self.panel_mut(side).host, PanelHost::Local);
        if let PanelHost::Remote { terminal, .. } = old {
            terminal.update(cx, |v, _cx| {
                v.close();
            });
        }

        let panel = self.panel_mut(side);
        panel.local_cwd = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        panel.local_entries = Self::read_local_dir_filtered(&panel.local_cwd, panel.show_hidden);
        panel.remote_cwd = None;
        panel.remote_entries = Arc::new(Vec::new());
        panel.clear_interaction();
        panel.on_navigate = None;
        panel.on_download = None;
        panel.on_upload = None;
        panel.on_delete = None;
        panel.on_rename = None;
        panel.on_edit = None;

        // Sync path input
        if let Some(ref input) = panel.path_input {
            let val = panel.local_cwd.to_string_lossy().into_owned();
            input.update(cx, |state, cx| {
                state.set_value(val.clone(), window, cx);
            });
            panel.last_synced_path = Some(val);
        }

        cx.notify();
    }

    // --- Panel actions driven by the ellipsis menu ---

    /// Toggle whether hidden files (Unix dotfiles) are shown on this panel.
    ///
    /// For local panels this is a synchronous re-read of the cwd with the
    /// new filter. For remote panels there's no client-side filter (the
    /// backend's `read_dir` returns everything), so we just trigger a fresh
    /// `sftp_navigate` to the current cwd to re-fetch and re-render.
    pub(super) fn toggle_hidden(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        let panel = self.panel_mut(side);
        panel.show_hidden = !panel.show_hidden;
        let show_hidden = panel.show_hidden;
        let is_remote = panel.host.is_remote();
        let local_cwd = panel.local_cwd.clone();
        let remote_cwd = panel.remote_cwd.clone();
        let on_navigate = panel.on_navigate.clone();
        if is_remote {
            if let (Some(cb), Some(cwd)) = (on_navigate, remote_cwd) {
                let cwd = cwd.as_str().to_string();
                cx.defer(move |cx| {
                    cb(cwd, cx);
                });
            }
        } else {
            panel.local_entries = Self::read_local_dir_filtered(&local_cwd, show_hidden);
        }
        panel.selected.clear();
        cx.notify();
    }

    /// Create a new folder ("New Folder" with a uniquified name).
    ///
    /// Begin the "new folder" flow: open an inline input at the top of
    /// the file list, pre-seeded with a unique "New Folder" name. The user
    /// edits the name + presses Enter to commit, or clicks away / presses
    /// Escape to cancel.
    pub(super) fn start_make_folder(
        &mut self,
        side: PanelSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.panel_mut(side);
        // Build a unique default name. For local panels we can probe the
        // filesystem; for remote we can't cheaply, so we just use "New Folder"
        // and let the server fail if it exists (the failure surfaces as a
        // toast, which is acceptable).
        let default_name = if panel.host.is_remote() {
            "New Folder".to_string()
        } else {
            let cwd = panel.local_cwd.clone();
            let base = "New Folder";
            let mut name = base.to_string();
            let mut i = 1;
            while cwd.join(&name).exists() {
                name = format!("{base} ({i})");
                i += 1;
            }
            name
        };
        panel.mkdir_pending = Some(());
        if panel.mkdir_input.is_none() {
            let entity = cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder("folder name");
                state.focus(window, cx);
                state
            });
            cx.subscribe(
                &entity,
                move |this, _input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        this.commit_make_folder(side, cx);
                    }
                },
            )
            .detach();
            let blur_handle = entity.read(cx).focus_handle(cx);
            cx.on_blur(&blur_handle, window, move |this, _window, cx| {
                if this.panel(side).mkdir_pending.is_some() {
                    this.cancel_make_folder(side, cx);
                }
            })
            .detach();
            panel.mkdir_input = Some(entity);
        }
        let panel = self.panel_mut(side);
        if let Some(ref input) = panel.mkdir_input {
            input.update(cx, |state, cx| {
                state.set_value(&default_name, window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Commit the mkdir: read the new name from `mkdir_input`, create the
    /// folder (local `std::fs::create_dir` or remote `sftp_mkdir`), and
    /// close the inline input.
    pub(super) fn commit_make_folder(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        let name = self.panel(side).mkdir_input.as_ref().and_then(|input| {
            let v = input.read(cx).value().to_string();
            if v.trim().is_empty() { None } else { Some(v) }
        });
        let Some(name) = name else {
            // Empty name → cancel.
            self.cancel_make_folder(side, cx);
            return;
        };
        let panel = self.panel_mut(side);
        let _ = panel.mkdir_pending.take();
        let is_remote = panel.host.is_remote();
        if is_remote {
            let remote_cwd = panel.remote_cwd.clone();
            let term = panel.host.terminal().cloned();
            let Some(cwd) = remote_cwd else { return };
            let Some(term) = term else { return };
            let remote_path = join_remote_path(cwd.as_str(), &name);
            term.read_with(cx, |view, _cx| {
                view.sftp_mkdir(&remote_path);
            });
        } else {
            let path = panel.local_cwd.join(&name);
            let _ = std::fs::create_dir(&path);
            panel.local_entries =
                Self::read_local_dir_filtered(&panel.local_cwd, panel.show_hidden);
        }
        cx.notify();
    }

    /// Abort the mkdir flow without creating anything.
    pub(super) fn cancel_make_folder(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        self.panel_mut(side).mkdir_pending = None;
        cx.notify();
    }

    /// Refresh the panel's listing — local re-reads its cwd, remote
    /// re-navigates to its current cwd.
    pub(super) fn refresh_listing(&mut self, side: PanelSide, cx: &mut Context<Self>) {
        let panel = self.panel_mut(side);
        let is_remote = panel.host.is_remote();
        let local_cwd = panel.local_cwd.clone();
        let remote_cwd = panel.remote_cwd.clone();
        let show_hidden = panel.show_hidden;
        let on_navigate = panel.on_navigate.clone();
        if is_remote {
            if let (Some(cb), Some(cwd)) = (on_navigate, remote_cwd) {
                let cwd = cwd.as_str().to_string();
                cx.defer(move |cx| {
                    cb(cwd, cx);
                });
            }
        } else {
            panel.local_entries = Self::read_local_dir_filtered(&local_cwd, show_hidden);
            panel.selected.clear();
            cx.notify();
        }
    }

    /// Disconnect the panel from its current host.
    ///
    /// For remote panels this closes the underlying SSH/SFTP connection
    /// (`TerminalView::close`) before dropping the handle. For local panels
    /// it just flips the host to `Disconnected`. The panel becomes the same
    /// placeholder that a brand-new SFTP tab starts with — "No host
    /// connected" + a "Select Host" button.
    pub(super) fn disconnect_panel(
        &mut self,
        side: PanelSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = std::mem::replace(&mut self.panel_mut(side).host, PanelHost::Disconnected);
        if let PanelHost::Remote { terminal, .. } = old {
            terminal.update(cx, |v, _cx| {
                v.close();
            });
        }
        let panel = self.panel_mut(side);
        panel.remote_cwd = None;
        panel.remote_entries = Arc::new(Vec::new());
        panel.clear_interaction();
        panel.on_navigate = None;
        panel.on_download = None;
        panel.on_upload = None;
        panel.on_upload_batch = None;
        panel.on_delete = None;
        panel.on_rename = None;
        panel.on_edit = None;
        // Reset the path input so the next render shows an empty field
        // rather than the stale remote path.
        if let Some(ref input) = panel.path_input {
            input.update(cx, |state, cx| {
                state.set_value("".to_string(), window, cx);
            });
        }
        panel.last_synced_path = Some(String::new());
        cx.notify();
    }
}

impl Default for SftpTabView {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolved host credentials needed to create an SSH backend.
struct ResolvedHost {
    name: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    private_key: Option<String>,
    passphrase: Option<String>,
    proxy: Option<crabport_core::credential::ProxyConfig>,
    startup_command: String,
}

impl CrabPortTab for SftpTabView {
    fn close(&mut self) {
        // Remote terminals are closed when switching hosts or dropping
        // the entity. Nothing extra to do here for the persistent tab.
    }
}

// ---------------------------------------------------------------------------
// Path join helpers (shared with panel.rs / helpers.rs)
// ---------------------------------------------------------------------------

/// Join a remote path component onto a remote cwd string, handling the
/// trailing-slash cases. POSIX-style (forward slash).
pub(super) fn join_remote_path(cwd: &str, name: &str) -> String {
    if cwd.ends_with('/') {
        format!("{}{}", cwd, name)
    } else {
        format!("{}/{}", cwd, name)
    }
}

/// Compute the parent path of a remote cwd string. Returns "/" for root.
pub(super) fn remote_parent(cwd: &str) -> String {
    let mut parts: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    parts.pop();
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Format a Unix mode integer as an `ls`-style permission string
/// (e.g. `rwxr-xr-x`). Used by `read_local_dir` on Unix.
#[cfg(unix)]
fn format_mode(mode: u32) -> String {
    let mode = mode & 0o777;
    let chars = [
        (mode & 0o400 != 0, 'r'),
        (mode & 0o200 != 0, 'w'),
        (mode & 0o100 != 0, 'x'),
        (mode & 0o040 != 0, 'r'),
        (mode & 0o020 != 0, 'w'),
        (mode & 0o010 != 0, 'x'),
        (mode & 0o004 != 0, 'r'),
        (mode & 0o002 != 0, 'w'),
        (mode & 0o001 != 0, 'x'),
    ];
    chars
        .iter()
        .map(|(on, c)| if *on { *c } else { '-' })
        .collect()
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

impl Render for SftpTabView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Clear context-menu-entry highlights if the menu closed.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(_cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            self.left.context_menu_entry = None;
            self.right.context_menu_entry = None;
        }

        let entity = _cx.entity().downgrade();
        let tooltip_ctrl = self.tooltip.clone();
        let host_selector = self.host_selector.clone();

        // The SFTP transfer toolbar lives in `content.rs` (shared with the
        // terminal tab) so its animation persists across tab switches. It
        // reads progress via `SftpTabView::sftp_progress`.

        div()
            .h_full()
            .w_full()
            .flex()
            .flex_col()
            .bg(rgb(crate::color::bg_base()))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    // Left panel
                    .child(self.render_panel(PanelSide::Left, &entity, &tooltip_ctrl, _window, _cx))
                    // Divider
                    .child(div().w(px(1.0)).h_full().bg(rgb(crate::color::border())))
                    // Right panel
                    .child(self.render_panel(
                        PanelSide::Right,
                        &entity,
                        &tooltip_ctrl,
                        _window,
                        _cx,
                    )),
            )
            // Host selector overlay (rendered on top)
            .when_some(host_selector, |el, overlay| el.child(overlay))
    }
}
