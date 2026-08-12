//! OpenSSH configuration import preview and validation UI.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::InputState;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crabport_core::credential::{HostEntry, HostKind};
use crabport_core::ssh_import::{
    SshImportBlocker, SshImportCandidate, SshImportNotice, SshImportRecord, SshImportScan,
    normalize_alias,
};
use crabport_ssh::{PrivateKeyFileStatus, inspect_private_key_file, validate_private_key_file};

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::components::form::{form_dialog, form_title};
use crate::components::input::StyledPasswordInput;
use crate::components::overlay::render_overlay;
use crate::components::switch::Switch;
use crate::motion::RADIUS_SM;

/// Result passed from the validated preview to the transactional importer.
pub struct SshImportOutput {
    /// Fully validated rows to persist in one transaction.
    pub records: Vec<SshImportRecord>,
    /// Candidate aliases and patterns intentionally skipped by the user or policy.
    pub skipped: usize,
    /// Non-blocking compatibility notices attached to selected aliases.
    pub warnings: usize,
}

/// Private-key readiness discovered before the preview is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImportKeyState {
    /// The private key can be decoded without a password phrase.
    Ready,
    /// The private key is encrypted and needs one shared phrase input.
    PassphraseRequired,
    /// The file exists but cannot be decoded by CrabPort's SSH backend.
    Invalid,
}

/// A preview-only condition produced while resolving conflicts and jumps.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ImportIssue {
    /// More than one saved connection has the normalized alias.
    AmbiguousConflict,
    /// The private key could not be decoded by the real SSH backend.
    InvalidPrivateKey,
    /// A jump alias is neither in the scan nor among the saved sessions.
    JumpMissing(String),
    /// The jump alias exists as a candidate but is itself blocked (e.g. its
    /// private key is invalid) so importing it cannot satisfy the chain.
    JumpNotImportable(String),
    /// More than one saved session matches a jump alias.
    JumpAmbiguous(String),
    /// A saved jump alias is not an SSH session.
    JumpNotSsh(String),
    /// A skipped conflicting row must be explicitly updated to preserve a chain.
    DependencyUpdateRequired { alias: String, jump: String },
    /// Two selected chains require different predecessors for one alias.
    ConflictingJump {
        alias: String,
        first: String,
        second: String,
    },
    /// A saved connection has the same alias but a non-SSH kind, so importing
    /// would either silently rewrite it to SSH or create a duplicate name.
    KindMismatch,
    /// The resolved selected and existing links form a cycle.
    JumpCycle(String),
}

/// One row in the import preview.
#[derive(Clone)]
pub struct SshImportPreviewRow {
    /// Parsed and resolved OpenSSH candidate.
    pub candidate: SshImportCandidate,
    /// Whether this row will be included in the transaction.
    pub selected: bool,
    /// Unique saved host selected for explicit update, if any.
    pub existing_host_id: Option<i64>,
    /// Whether this row was pulled in to satisfy another selected jump chain.
    pub dependency: bool,
    /// Immediate jump alias written to CrabPort after chain expansion.
    pub jump_host_name: Option<String>,
    /// Row-level dependency, conflict, key, and cycle problems.
    issues: Vec<ImportIssue>,
    /// Private-key readiness for this candidate's sole key file.
    key_state: ImportKeyState,
    /// Whether multiple saved rows made the normalized conflict ambiguous.
    ambiguous_conflict: bool,
    /// Whether a saved non-SSH row already uses the normalized alias. Importing
    /// must not silently rewrite it, and creating a second same-named SSH row
    /// would also be confusing, so the row stays blocked until the user renames
    /// the existing connection.
    non_ssh_conflict: bool,
}

impl SshImportPreviewRow {
    /// Return whether the user may select this row in the preview.
    fn can_select(&self) -> bool {
        self.candidate.blockers.is_empty()
            && self.key_state != ImportKeyState::Invalid
            && !self.ambiguous_conflict
            && !self.non_ssh_conflict
    }

    /// Return whether this selected row still has a blocking preview issue.
    fn has_blocker(&self) -> bool {
        !self.candidate.blockers.is_empty()
            || self.key_state == ImportKeyState::Invalid
            || self.ambiguous_conflict
            || self.non_ssh_conflict
            || !self.issues.is_empty()
    }
}

/// One password-phrase input shared by every candidate using the same key path.
#[derive(Clone)]
pub struct SshImportPassphrase {
    /// Canonical preview key for phrase reuse; key contents are never copied.
    pub path: PathBuf,
    /// Masked GPUI input bound to the active window.
    pub input: Entity<InputState>,
    /// Validation error shown after a failed submit.
    pub error: Option<SharedString>,
}

/// Mutable state for the OpenSSH import preview overlay.
#[derive(Clone)]
pub struct SshImportState {
    /// Open/close animation state.
    pub open: bool,
    /// Root OpenSSH config file shown in the preview header.
    pub config_path: PathBuf,
    /// Import candidates in source order.
    pub rows: Vec<SshImportPreviewRow>,
    /// Password-phrase inputs deduplicated by private-key path.
    pub passphrases: Vec<SshImportPassphrase>,
    /// Wildcard and negated host patterns excluded by policy.
    pub skipped_patterns: Vec<String>,
    /// Dialog-level validation error.
    pub error: Option<SharedString>,
    /// Saved hosts used for conflict and existing jump-chain resolution.
    existing_hosts: Vec<HostEntry>,
}

impl SshImportState {
    /// Build a preview from a completed OpenSSH scan and the current store state.
    pub fn new(
        scan: SshImportScan,
        existing_hosts: Vec<HostEntry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        // Update candidates are limited to SSH hosts: the OpenSSH import flow
        // rebinds credential/host/port/username, which would silently rewrite
        // a Telnet/Serial row. A same-named non-SSH row is tracked separately
        // so the candidate can surface a kind mismatch and stay unselectable.
        let mut existing_by_alias: HashMap<String, Vec<i64>> = HashMap::new();
        let mut non_ssh_aliases: HashSet<String> = HashSet::new();
        for host in &existing_hosts {
            let normalized = normalize_alias(&host.name);
            if host.kind == HostKind::Ssh {
                existing_by_alias
                    .entry(normalized)
                    .or_default()
                    .push(host.id);
            } else {
                non_ssh_aliases.insert(normalized);
            }
        }

        // Inspect each unique path once; encrypted keys later share one input.
        let mut key_states: HashMap<PathBuf, ImportKeyState> = HashMap::new();
        for candidate in &scan.candidates {
            let Some(path) = candidate.identity_file() else {
                continue;
            };
            key_states
                .entry(path.to_path_buf())
                .or_insert_with(|| match inspect_private_key_file(path) {
                    Ok(PrivateKeyFileStatus::Ready) => ImportKeyState::Ready,
                    Ok(PrivateKeyFileStatus::PassphraseRequired) => {
                        ImportKeyState::PassphraseRequired
                    }
                    Err(error) => {
                        tracing::warn!(
                            "ssh import: private key inspection failed for {}: {}",
                            path.display(),
                            error
                        );
                        ImportKeyState::Invalid
                    }
                });
        }

        let rows = scan
            .candidates
            .into_iter()
            .map(|candidate| {
                let normalized = normalize_alias(&candidate.name);
                let matches = existing_by_alias
                    .get(&normalized)
                    .cloned()
                    .unwrap_or_default();
                let ambiguous_conflict = matches.len() > 1;
                let non_ssh_conflict = non_ssh_aliases.contains(&normalized);
                let existing_host_id = matches
                    .as_slice()
                    .first()
                    .copied()
                    .filter(|_| !ambiguous_conflict && !non_ssh_conflict);
                let key_state = candidate
                    .identity_file()
                    .and_then(|path| key_states.get(path).copied())
                    .unwrap_or(ImportKeyState::Invalid);
                let mut issues = Vec::new();
                if non_ssh_conflict {
                    issues.push(ImportIssue::KindMismatch);
                }
                // Importable new rows start selected; name conflicts default to skip.
                let selected = candidate.blockers.is_empty()
                    && key_state != ImportKeyState::Invalid
                    && !ambiguous_conflict
                    && !non_ssh_conflict
                    && existing_host_id.is_none();
                SshImportPreviewRow {
                    candidate,
                    selected,
                    existing_host_id,
                    dependency: false,
                    jump_host_name: None,
                    issues,
                    key_state,
                    ambiguous_conflict,
                    non_ssh_conflict,
                }
            })
            .collect();

        let mut passphrases = Vec::new();
        for (path, state) in key_states {
            if state != ImportKeyState::PassphraseRequired {
                continue;
            }
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_masked(true, window, cx);
                input
            });
            passphrases.push(SshImportPassphrase {
                path,
                input,
                error: None,
            });
        }
        passphrases.sort_by(|left, right| left.path.cmp(&right.path));

        let mut state = Self {
            open: true,
            config_path: scan.config_path,
            rows,
            passphrases,
            skipped_patterns: scan.skipped_patterns,
            error: None,
            existing_hosts,
        };
        state.recompute_dependencies();
        state
    }

    /// Toggle one candidate, treating a selected name conflict as an update.
    pub fn set_selected(&mut self, index: usize, selected: bool) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if !row.can_select() {
            return;
        }
        row.selected = selected;
        row.dependency = false;
        self.error = None;
        self.recompute_dependencies();
    }

    /// Select every importable new row while leaving conflicts skipped.
    pub fn select_all_new(&mut self) {
        for row in &mut self.rows {
            if row.can_select() && row.existing_host_id.is_none() {
                row.selected = true;
            }
        }
        self.error = None;
        self.recompute_dependencies();
    }

    /// Clear the current selection and all derived dependency links.
    pub fn clear_selection(&mut self) {
        for row in &mut self.rows {
            row.selected = false;
            row.dependency = false;
        }
        self.error = None;
        self.recompute_dependencies();
    }

    /// Return the number of rows currently selected for create or update.
    pub fn selected_count(&self) -> usize {
        self.rows.iter().filter(|row| row.selected).count()
    }

    /// Return whether the current preview can enter phrase validation.
    pub fn can_submit(&self) -> bool {
        self.selected_count() > 0
            && self
                .rows
                .iter()
                .filter(|row| row.selected)
                .all(|row| !row.has_blocker())
    }

    /// Validate phrases and build the exact transaction records.
    pub fn output(&mut self, cx: &App) -> Option<SshImportOutput> {
        self.error = None;
        for phrase in &mut self.passphrases {
            phrase.error = None;
        }
        if !self.can_submit() {
            self.error = Some(t!("ssh_import.error_blocked").into());
            return None;
        }

        let selected_paths: HashSet<PathBuf> = self
            .rows
            .iter()
            .filter(|row| row.selected && row.key_state == ImportKeyState::PassphraseRequired)
            .filter_map(|row| row.candidate.identity_file().map(Path::to_path_buf))
            .collect();
        let mut phrases = HashMap::new();
        let mut valid = true;
        for phrase in &mut self.passphrases {
            if !selected_paths.contains(&phrase.path) {
                continue;
            }
            let value = phrase.input.read(cx).value().to_string();
            if value.is_empty() {
                phrase.error = Some(t!("ssh_import.error_passphrase_required").into());
                valid = false;
                continue;
            }
            if let Err(error) = validate_private_key_file(&phrase.path, &value) {
                tracing::warn!(
                    "ssh import: password phrase validation failed for {}: {}",
                    phrase.path.display(),
                    error
                );
                phrase.error = Some(t!("ssh_import.error_passphrase_invalid").into());
                valid = false;
                continue;
            }
            phrases.insert(phrase.path.clone(), value);
        }
        if !valid {
            self.error = Some(t!("ssh_import.error_passphrases").into());
            return None;
        }

        let records = self
            .rows
            .iter()
            .filter(|row| row.selected)
            .filter_map(|row| {
                let identity_file = row.candidate.identity_file()?.to_path_buf();
                Some(SshImportRecord {
                    name: row.candidate.name.clone(),
                    host: row.candidate.host.clone(),
                    port: row.candidate.port,
                    username: row.candidate.username.clone(),
                    passphrase: phrases.get(&identity_file).cloned().unwrap_or_default(),
                    identity_file,
                    existing_host_id: row.existing_host_id,
                    jump_host_name: row.jump_host_name.clone(),
                })
            })
            .collect();
        let skipped = self.rows.len() - self.selected_count() + self.skipped_patterns.len();
        let warnings = self
            .rows
            .iter()
            .filter(|row| row.selected)
            .map(|row| row.candidate.notices.len())
            .sum();
        Some(SshImportOutput {
            records,
            skipped,
            warnings,
        })
    }

    /// Expand selected `ProxyJump` chains and validate dependencies and cycles.
    fn recompute_dependencies(&mut self) {
        for row in &mut self.rows {
            row.dependency = false;
            row.jump_host_name = None;
            row.issues.clear();
            if row.ambiguous_conflict {
                row.issues.push(ImportIssue::AmbiguousConflict);
            }
            if row.key_state == ImportKeyState::Invalid {
                row.issues.push(ImportIssue::InvalidPrivateKey);
            }
        }

        let candidate_by_alias: HashMap<String, usize> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| (normalize_alias(&row.candidate.name), index))
            .collect();

        // Newly selected dependencies may have their own ProxyJump chain, so
        // repeat until selection reaches a fixed point.
        for _ in 0..=self.rows.len() {
            let before = self.selected_count();
            let selected: Vec<usize> = self
                .rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| row.selected.then_some(index))
                .collect();
            for root in selected {
                let root_name = self.rows[root].candidate.name.clone();
                let chain = self.rows[root].candidate.proxy_jump.clone();
                let mut child = root_name;
                for jump in chain.iter().rev() {
                    if !self.ensure_jump_destination(root, jump, &candidate_by_alias) {
                        break;
                    }
                    if !self.assign_jump(root, &child, jump, &candidate_by_alias) {
                        break;
                    }
                    child = jump.clone();
                }
            }
            if self.selected_count() == before {
                break;
            }
        }
        self.detect_jump_cycles();
    }

    /// Ensure a jump alias will exist as a unique SSH host after the batch.
    fn ensure_jump_destination(
        &mut self,
        root: usize,
        alias: &str,
        candidates: &HashMap<String, usize>,
    ) -> bool {
        let normalized = normalize_alias(alias);
        if let Some(index) = candidates.get(&normalized).copied() {
            if self.rows[index].selected {
                return true;
            }
            let matches = self.existing_matches(&normalized);
            match matches.as_slice() {
                [host] if host.kind == HostKind::Ssh => return true,
                [_, _, ..] => {
                    self.push_issue(root, ImportIssue::JumpAmbiguous(alias.to_string()));
                    return false;
                }
                [host] => {
                    let _ = host;
                    self.push_issue(root, ImportIssue::JumpNotSsh(alias.to_string()));
                    return false;
                }
                [] if self.rows[index].can_select() => {
                    self.rows[index].selected = true;
                    self.rows[index].dependency = true;
                    return true;
                }
                [] => {
                    self.push_issue(root, ImportIssue::JumpNotImportable(alias.to_string()));
                    return false;
                }
            }
        }

        let matches = self.existing_matches(&normalized);
        match matches.as_slice() {
            [host] if host.kind == HostKind::Ssh => true,
            [_, _, ..] => {
                self.push_issue(root, ImportIssue::JumpAmbiguous(alias.to_string()));
                false
            }
            [host] => {
                let _ = host;
                self.push_issue(root, ImportIssue::JumpNotSsh(alias.to_string()));
                false
            }
            [] => {
                self.push_issue(root, ImportIssue::JumpMissing(alias.to_string()));
                false
            }
        }
    }

    /// Assign one immediate jump edge to an imported or compatible saved alias.
    fn assign_jump(
        &mut self,
        root: usize,
        child_alias: &str,
        jump_alias: &str,
        candidates: &HashMap<String, usize>,
    ) -> bool {
        let child_normalized = normalize_alias(child_alias);
        if let Some(index) = candidates.get(&child_normalized).copied() {
            if self.rows[index].selected {
                if let Some(existing) = self.rows[index].jump_host_name.clone()
                    && normalize_alias(&existing) != normalize_alias(jump_alias)
                {
                    self.push_issue(
                        root,
                        ImportIssue::ConflictingJump {
                            alias: child_alias.to_string(),
                            first: existing,
                            second: jump_alias.to_string(),
                        },
                    );
                    return false;
                }
                self.rows[index].jump_host_name = Some(jump_alias.to_string());
                if index != root {
                    self.rows[index].dependency = true;
                }
                return true;
            }
        }

        let matches = self.existing_matches(&child_normalized);
        match matches.as_slice() {
            [host] if self.existing_jump_matches(host, jump_alias) => true,
            [host] => {
                self.push_issue(
                    root,
                    ImportIssue::DependencyUpdateRequired {
                        alias: host.name.clone(),
                        jump: jump_alias.to_string(),
                    },
                );
                false
            }
            [_, _, ..] => {
                self.push_issue(root, ImportIssue::JumpAmbiguous(child_alias.to_string()));
                false
            }
            [] => {
                self.push_issue(root, ImportIssue::JumpMissing(child_alias.to_string()));
                false
            }
        }
    }

    /// Return saved hosts matching a normalized alias.
    fn existing_matches(&self, normalized: &str) -> Vec<HostEntry> {
        self.existing_hosts
            .iter()
            .filter(|host| normalize_alias(&host.name) == normalized)
            .cloned()
            .collect()
    }

    /// Return whether an existing host already points at `jump_alias`.
    fn existing_jump_matches(&self, host: &HostEntry, jump_alias: &str) -> bool {
        host.jump_host_id
            .and_then(|id| {
                self.existing_hosts
                    .iter()
                    .find(|candidate| candidate.id == id)
            })
            .is_some_and(|jump| normalize_alias(&jump.name) == normalize_alias(jump_alias))
    }

    /// Add a row issue once so fixed-point resolution does not duplicate text.
    fn push_issue(&mut self, row: usize, issue: ImportIssue) {
        if let Some(issues) = self.rows.get_mut(row).map(|row| &mut row.issues)
            && !issues.contains(&issue)
        {
            issues.push(issue);
        }
    }

    /// Detect cycles reachable from selected rows in the resolved graph.
    fn detect_jump_cycles(&mut self) {
        let selected_aliases: HashSet<String> = self
            .rows
            .iter()
            .filter(|row| row.selected)
            .map(|row| normalize_alias(&row.candidate.name))
            .collect();
        let mut links = HashMap::new();

        // Unique saved aliases contribute their current links unless a selected
        // import row overrides that alias in this transaction.
        let mut saved_by_alias: HashMap<String, Vec<&HostEntry>> = HashMap::new();
        for host in &self.existing_hosts {
            saved_by_alias
                .entry(normalize_alias(&host.name))
                .or_default()
                .push(host);
        }
        for (alias, hosts) in saved_by_alias {
            if selected_aliases.contains(&alias) || hosts.len() != 1 {
                continue;
            }
            if let Some(jump_id) = hosts[0].jump_host_id
                && let Some(jump) = self.existing_hosts.iter().find(|host| host.id == jump_id)
            {
                links.insert(alias, normalize_alias(&jump.name));
            }
        }
        for row in &self.rows {
            if row.selected
                && let Some(jump) = row.jump_host_name.as_deref()
            {
                links.insert(normalize_alias(&row.candidate.name), normalize_alias(jump));
            }
        }

        let starts: Vec<(usize, String)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.selected)
            .map(|(index, row)| (index, normalize_alias(&row.candidate.name)))
            .collect();
        for (row_index, start) in starts {
            let mut positions = HashMap::new();
            let mut path = Vec::new();
            let mut current = Some(start);
            while let Some(alias) = current {
                if let Some(cycle_start) = positions.get(&alias).copied() {
                    let cycle = path[cycle_start..]
                        .iter()
                        .chain(std::iter::once(&alias))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    self.push_issue(row_index, ImportIssue::JumpCycle(cycle));
                    break;
                }
                positions.insert(alias.clone(), path.len());
                path.push(alias.clone());
                current = links.get(&alias).cloned();
            }
        }
    }
}

/// Pure renderer for an [`SshImportState`] snapshot.
#[derive(IntoElement)]
pub struct SshImportView {
    state: SshImportState,
    app: Entity<CrabportApp>,
}

impl SshImportView {
    /// Snapshot the current state for one GPUI render pass.
    pub fn new(state: &SshImportState, app: Entity<CrabportApp>) -> Self {
        Self {
            state: state.clone(),
            app,
        }
    }
}

impl RenderOnce for SshImportView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let app = self.app.clone();
        render_overlay(
            ElementId::Name("ssh-import-overlay".into()),
            self.state.open,
            Some(std::rc::Rc::new(move |_window, cx| {
                app.update(cx, |app, cx| app.close_ssh_import(cx));
            })),
            render_dialog(self.state, self.app),
        )
    }
}

/// Render the fixed header, scrollable preview, and transaction footer.
fn render_dialog(state: SshImportState, app: Entity<CrabportApp>) -> impl IntoElement {
    let selected_count = state.selected_count();
    let total_count = state.rows.len();
    let can_submit = state.can_submit();
    let config_path = state.config_path.to_string_lossy().into_owned();

    form_dialog("ssh-import-dialog", state.open, 760.0, |dialog| {
        dialog.max_h(px(680.0)).overflow_hidden()
    })
    .child(
        div()
            .px_6()
            .pt_6()
            .flex()
            .flex_col()
            .gap_2()
            .child(form_title(t!("ssh_import.title").to_string()))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(config_path),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div().text_sm().text_color(rgb(text_muted())).child(
                            t!(
                                "ssh_import.selection_summary",
                                selected = selected_count,
                                total = total_count
                            )
                            .to_string(),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("ssh-import-select-all")
                                    .icon("icons/circle-check.svg")
                                    .w_auto()
                                    .px_2()
                                    .child(t!("ssh_import.select_all").to_string())
                                    .on_click({
                                        let app = app.clone();
                                        move |_event, _window, cx| {
                                            app.update(cx, |app, cx| {
                                                if let Some(state) = app.ssh_import.as_mut() {
                                                    state.select_all_new();
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("ssh-import-clear")
                                    .w_auto()
                                    .px_2()
                                    .child(t!("ssh_import.clear").to_string())
                                    .on_click({
                                        let app = app.clone();
                                        move |_event, _window, cx| {
                                            app.update(cx, |app, cx| {
                                                if let Some(state) = app.ssh_import.as_mut() {
                                                    state.clear_selection();
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    }),
                            ),
                    ),
            ),
    )
    .child(div().h_px().bg(rgb(border())).mx_6())
    .child(
        div()
            .id("ssh-import-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                div()
                    .px_6()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(
                        state
                            .rows
                            .iter()
                            .enumerate()
                            .map(|(index, row)| render_candidate_row(index, row, app.clone())),
                    )
                    .children(render_passphrase_fields(&state))
                    .when(!state.skipped_patterns.is_empty(), |element| {
                        element.child(
                            div().text_xs().text_color(rgb(text_muted())).child(
                                t!(
                                    "ssh_import.skipped_patterns",
                                    count = state.skipped_patterns.len()
                                )
                                .to_string(),
                            ),
                        )
                    })
                    .when_some(state.error.clone(), |element, error| {
                        element.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .text_color(rgb(term_red()))
                                .child(svg().path("icons/circle-alert.svg").size_4().flex_none())
                                .child(error),
                        )
                    }),
            ),
    )
    .child(div().h_px().bg(rgb(border())).mx_6())
    .child(
        div()
            .px_6()
            .pb_6()
            .flex()
            .justify_end()
            .gap_3()
            .child(
                Button::new("ssh-import-cancel")
                    .centered(true)
                    .child(t!("ssh_import.cancel").to_string())
                    .on_click({
                        let app = app.clone();
                        move |_event, _window, cx| {
                            app.update(cx, |app, cx| app.close_ssh_import(cx));
                        }
                    }),
            )
            .child(
                Button::new("ssh-import-confirm")
                    .primary()
                    .centered(true)
                    .disabled(!can_submit)
                    .child(t!("ssh_import.import").to_string())
                    .on_click(move |_event, window, cx| {
                        app.update(cx, |app, cx| app.perform_ssh_import(window, cx));
                    }),
            ),
    )
}

/// Render one candidate with selection, connection details, and diagnostics.
fn render_candidate_row(
    index: usize,
    row: &SshImportPreviewRow,
    app: Entity<CrabportApp>,
) -> AnyElement {
    let status = if row.has_blocker() {
        t!("ssh_import.status_blocked").to_string()
    } else if !row.selected {
        t!("ssh_import.status_skip").to_string()
    } else if row.existing_host_id.is_some() {
        t!("ssh_import.status_update").to_string()
    } else {
        t!("ssh_import.status_create").to_string()
    };
    let status_color = if row.has_blocker() {
        term_red()
    } else if !row.selected {
        text_muted()
    } else if row.existing_host_id.is_some() {
        term_yellow()
    } else {
        term_green()
    };
    let address = format!(
        "{}@{}:{}",
        row.candidate.username, row.candidate.host, row.candidate.port
    );
    let key_path = row
        .candidate
        .identity_file()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| t!("ssh_import.no_key").to_string());
    let jump = row
        .candidate
        .proxy_jump
        .is_empty()
        .then(|| t!("ssh_import.direct").to_string())
        .unwrap_or_else(|| row.candidate.proxy_jump.join(" -> "));
    let diagnostics = row_diagnostics(row);

    div()
        .id(ElementId::Name(
            format!("ssh-import-candidate-{index}").into(),
        ))
        .flex()
        .items_start()
        .gap_3()
        .p_3()
        .rounded(RADIUS_SM)
        .border_1()
        .border_color(rgb(border()))
        .bg(rgb(surface_active()))
        .child(
            Switch::new(ElementId::Name(format!("ssh-import-switch-{index}").into()))
                .checked(row.selected)
                .disabled(!row.can_select())
                .on_change(move |selected, _window, cx| {
                    app.update(cx, |app, cx| {
                        if let Some(state) = app.ssh_import.as_mut() {
                            state.set_selected(index, *selected);
                            cx.notify();
                        }
                    });
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(text_primary()))
                                .child(row.candidate.name.clone()),
                        )
                        .child(div().text_xs().text_color(rgb(status_color)).child(status))
                        .when(row.dependency, |element| {
                            element.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(term_blue()))
                                    .child(t!("ssh_import.dependency").to_string()),
                            )
                        }),
                )
                .child(div().text_xs().text_color(rgb(text_muted())).child(address))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(format!("{} · {}", key_path, jump)),
                )
                .children(diagnostics.into_iter().map(|diagnostic| {
                    div()
                        .flex()
                        .items_start()
                        .gap_1()
                        .text_xs()
                        .text_color(rgb(term_red()))
                        .child(
                            svg()
                                .path("icons/circle-alert.svg")
                                .size_3()
                                .mt(px(2.0))
                                .flex_none(),
                        )
                        .child(diagnostic)
                })),
        )
        .into_any_element()
}

/// Render unique password-phrase fields used by selected encrypted keys.
fn render_passphrase_fields(state: &SshImportState) -> Vec<AnyElement> {
    let selected_paths: HashSet<PathBuf> = state
        .rows
        .iter()
        .filter(|row| row.selected && row.key_state == ImportKeyState::PassphraseRequired)
        .filter_map(|row| row.candidate.identity_file().map(Path::to_path_buf))
        .collect();
    state
        .passphrases
        .iter()
        .enumerate()
        .filter(|(_, phrase)| selected_paths.contains(&phrase.path))
        .map(|(index, phrase)| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(phrase.path.to_string_lossy().into_owned()),
                )
                .child(
                    StyledPasswordInput::new(
                        format!("ssh-import-passphrase-{index}"),
                        phrase.input.clone(),
                    )
                    .label(t!("ssh_import.passphrase").to_string())
                    .when_some(phrase.error.clone(), |input, error| input.error(error)),
                )
                .into_any_element()
        })
        .collect()
}

/// Convert structured core and preview issues into localized display text.
fn row_diagnostics(row: &SshImportPreviewRow) -> Vec<String> {
    let mut diagnostics: Vec<String> = row.candidate.blockers.iter().map(blocker_text).collect();
    diagnostics.extend(row.issues.iter().map(issue_text));
    diagnostics.extend(row.candidate.notices.iter().map(notice_text));
    diagnostics
}

/// Localize one scanner blocker.
fn blocker_text(blocker: &SshImportBlocker) -> String {
    match blocker {
        SshImportBlocker::MissingIdentityFile => t!("ssh_import.blocker_missing_key").to_string(),
        SshImportBlocker::MultipleIdentityFiles(paths) => {
            t!("ssh_import.blocker_multiple_keys", count = paths.len()).to_string()
        }
        SshImportBlocker::IdentityFileMissing(path) => t!(
            "ssh_import.blocker_key_missing",
            path = path.to_string_lossy().as_ref()
        )
        .to_string(),
        SshImportBlocker::UnsupportedIdentityToken(token) => {
            t!("ssh_import.blocker_key_token", token = token.as_str()).to_string()
        }
        SshImportBlocker::LocalUsernameMissing => {
            t!("ssh_import.blocker_local_username_missing").to_string()
        }
        SshImportBlocker::UnsupportedProxyJump(jump) => {
            t!("ssh_import.blocker_proxy_jump", jump = jump.as_str()).to_string()
        }
        SshImportBlocker::UnsupportedDirectives(fields) => t!(
            "ssh_import.blocker_directives",
            fields = fields.join(", ").as_str()
        )
        .to_string(),
    }
}

/// Localize one dependency or conflict issue.
fn issue_text(issue: &ImportIssue) -> String {
    match issue {
        ImportIssue::AmbiguousConflict => t!("ssh_import.issue_ambiguous_conflict").to_string(),
        ImportIssue::KindMismatch => t!("ssh_import.issue_kind_mismatch").to_string(),
        ImportIssue::InvalidPrivateKey => t!("ssh_import.issue_invalid_key").to_string(),
        ImportIssue::JumpMissing(alias) => {
            t!("ssh_import.issue_jump_missing", alias = alias.as_str()).to_string()
        }
        ImportIssue::JumpNotImportable(alias) => t!(
            "ssh_import.issue_jump_not_importable",
            alias = alias.as_str()
        )
        .to_string(),
        ImportIssue::JumpAmbiguous(alias) => {
            t!("ssh_import.issue_jump_ambiguous", alias = alias.as_str()).to_string()
        }
        ImportIssue::JumpNotSsh(alias) => {
            t!("ssh_import.issue_jump_not_ssh", alias = alias.as_str()).to_string()
        }
        ImportIssue::DependencyUpdateRequired { alias, jump } => t!(
            "ssh_import.issue_dependency_update",
            alias = alias.as_str(),
            jump = jump.as_str()
        )
        .to_string(),
        ImportIssue::ConflictingJump {
            alias,
            first,
            second,
        } => t!(
            "ssh_import.issue_jump_conflict",
            alias = alias.as_str(),
            first = first.as_str(),
            second = second.as_str()
        )
        .to_string(),
        ImportIssue::JumpCycle(cycle) => {
            t!("ssh_import.issue_jump_cycle", cycle = cycle.as_str()).to_string()
        }
    }
}

/// Localize one non-blocking scanner notice.
fn notice_text(notice: &SshImportNotice) -> String {
    match notice {
        SshImportNotice::InferredHostName => t!("ssh_import.notice_host_inferred").to_string(),
        SshImportNotice::InferredUsername => t!("ssh_import.notice_user_inferred").to_string(),
        SshImportNotice::RelativeIdentityPath => {
            t!("ssh_import.notice_relative_identity_path").to_string()
        }
        SshImportNotice::IgnoredDirectives(fields) => t!(
            "ssh_import.notice_ignored",
            fields = fields.join(", ").as_str()
        )
        .to_string(),
    }
}
