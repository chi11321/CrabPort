//! Bottom toolbar framework.
//!
//! This module is intentionally *generic*: it knows how to lay out a
//! horizontal strip of slots with a gear button on the right that opens a
//! context menu for toggling each slot's visibility. It does NOT know what
//! the slots actually render — that's the caller's responsibility (see
//! `crabport-ui/src/views/terminal/toolbar.rs` for the terminal-tab
//! concrete toolbar, and the SFTP tab uses the same framework to render
//! transfer progress + a "history" toggle).
//!
//! ## Framework / caller contract
//!
//! - The caller passes a `Vec<ToolbarSlot>` describing every *possible*
//!   slot: its stable id, the label shown in the ctxmenu, its current
//!   visibility, and a closure that builds its element tree.
//! - The caller also passes an `on_toggle` closure that's invoked when
//!   the user picks a slot in the ctxmenu. The caller is responsible for
//!   persisting the new visibility (e.g. to `config.toml`); the framework
//!   just dispatches the toggle and re-renders on the next frame.
//! - The toolbar's overall open/closed state is derived: it's shown iff
//!   at least one slot is both *visible* and *non-empty* (i.e. the slot's
//!   render closure returned `Some`). This matches the old behavior where
//!   the terminal toolbar stayed hidden until metrics loaded.
//!
//! ## Layout
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ ● 12ms │ ▮ 2.1G/16G │ CPU 5% │ 💾 80% │ ↑1.2K ↓4K │ [⚙]    │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! The gear icon opens a context menu whose items are the slot labels,
//! each prefixed with a checkmark when the slot is currently visible.

use std::rc::Rc;

use gpui::{prelude::FluentBuilder, *};
use gpui_animation::animation::TransitionExt;

use rust_i18n::t;

use crate::color::*;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::motion::{EASE_STANDARD, duration_slower};

pub const TOOLBAR_HEIGHT: f32 = 36.0;
pub const BAR_WIDTH: f32 = 80.0;
pub const BAR_HEIGHT: f32 = 6.0;

// ---------------------------------------------------------------------------
// Status colors (used by the terminal concrete toolbar below)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) fn status_color(status: crabport_terminal::terminal::RemoteStatus) -> u32 {
    use crabport_terminal::terminal::RemoteStatus;
    match status {
        RemoteStatus::Local => term_bright_black(),
        RemoteStatus::Connected => term_green(),
        RemoteStatus::Connecting => term_yellow(),
        RemoteStatus::Disconnected => term_red(),
    }
}

/// Accent color for progress bar fills — read live so theme changes are
/// picked up without a recompile.
#[allow(dead_code)]
pub(crate) fn color_accent() -> u32 {
    term_blue()
}

// ---------------------------------------------------------------------------
// ToolbarSlot — one unit of toolbar content
// ---------------------------------------------------------------------------

/// A single slot in the toolbar.
///
/// `id` must be stable across renders — it's used as the ctxmenu item
/// discriminator and is passed to `on_toggle`. Use a `&'static str` literal
/// (e.g. `"latency"`, `"cpu"`, `"sftp_history"`).
///
/// `visible` is the *caller-controlled* visibility (driven by config).
/// The framework additionally hides the slot when `render` returns
/// `None`, so a slot that has no data (e.g. CPU stats before the first
/// monitor tick) doesn't take up space even when its config flag is on.
///
/// `render` is called every frame the toolbar is painted. Returning
/// `None` means "no content this frame" — the framework will skip this
/// slot without leaving a gap.
#[derive(Clone)]
pub struct ToolbarSlot {
    pub id: &'static str,
    pub label: SharedString,
    pub visible: bool,
    pub render: Rc<dyn Fn() -> Option<AnyElement>>,
}

impl ToolbarSlot {
    pub fn new(
        id: &'static str,
        label: impl Into<SharedString>,
        visible: bool,
        render: impl Fn() -> Option<AnyElement> + 'static,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            visible,
            render: Rc::new(render),
        }
    }
}

// ---------------------------------------------------------------------------
// ToolbarProps — everything the framework needs to render
// ---------------------------------------------------------------------------

/// Props passed to [`render_toolbar`].
///
/// The caller builds a fresh `ToolbarProps` every render. The framework
/// doesn't hold state between frames — visibility lives in the caller's
/// config, and the ctxmenu's open/dismiss state is handled by the global
/// [`ContextMenuController`].
pub struct ToolbarProps {
    /// Slots in left-to-right display order. Hidden slots (`visible ==
    /// false` or `render == None`) are dropped from the layout.
    pub slots: Vec<ToolbarSlot>,
    /// Invoked when the user clicks a slot's entry in the ctxmenu. The
    /// closure receives the slot's `id` so the caller can flip the
    /// matching config flag. The caller is responsible for persisting
    /// the change.
    pub on_toggle: Rc<dyn Fn(&str, &mut App) + 'static>,
    /// Entity of the global context-menu controller. Used to show the
    /// slot-toggle menu when the user clicks the gear icon. If `None`,
    /// the gear button is hidden (no toggling).
    pub context_menu: Option<Entity<ContextMenuController>>,
    /// Extra right-aligned children rendered *before* the gear icon.
    /// Used by callers that want toolbar-internal actions that aren't
    /// toggleable (e.g. an "SFTP history" toggle button that flips
    /// a panel rather than a slot).
    pub trailing: Vec<AnyElement>,
}

impl ToolbarProps {
    pub fn new(on_toggle: impl Fn(&str, &mut App) + 'static) -> Self {
        Self {
            slots: Vec::new(),
            on_toggle: Rc::new(on_toggle),
            context_menu: None,
            trailing: Vec::new(),
        }
    }

    pub fn slot(mut self, slot: ToolbarSlot) -> Self {
        self.slots.push(slot);
        self
    }

    pub fn context_menu(mut self, cm: Entity<ContextMenuController>) -> Self {
        self.context_menu = Some(cm);
        self
    }

    pub fn trailing(mut self, el: impl IntoElement) -> Self {
        self.trailing.push(el.into_any_element());
        self
    }
}

// ---------------------------------------------------------------------------
// render_toolbar — the framework entry point
// ---------------------------------------------------------------------------

/// Render the toolbar framework.
///
/// The toolbar's overall height animates between `TOOLBAR_HEIGHT` and `0`
/// based on whether any slot produced visible content. The transition
/// mirrors the original terminal toolbar's feel (320ms EaseInOutCubic).
pub fn render_toolbar(props: ToolbarProps) -> impl IntoElement {
    // Compute the visible-element list up front so we can decide whether
    // the toolbar should be open at all. We materialize the elements
    // (rather than deferring to a lazy iterator) because gpui's `.children()`
    // takes a concrete `IntoIterator` and we need a stable count for the
    // show/hide decision anyway.
    let mut visible: Vec<AnyElement> = Vec::new();
    for slot in &props.slots {
        if !slot.visible {
            continue;
        }
        if let Some(el) = (slot.render)() {
            visible.push(el);
        }
    }
    let has_trailing = !props.trailing.is_empty();
    let show_toolbar = !visible.is_empty() || has_trailing;

    let on_toggle = props.on_toggle.clone();
    let context_menu = props.context_menu.clone();
    // Snapshot the slot list for the ctxmenu builder (which runs on click,
    // not at render time).
    let slots_snapshot = props.slots.clone();
    let trailing = props.trailing;

    div()
        .id("terminal-toolbar")
        .w_full()
        .overflow_hidden()
        .border_t_1()
        .with_transition("terminal-toolbar-height")
        .transition_when_else(
            show_toolbar,
            duration_slower(),
            EASE_STANDARD,
            |el| el.h(px(TOOLBAR_HEIGHT)),
            |el| el.h_0(),
        )
        .bg(rgb(bg_tab_bar()))
        .border_b_1()
        .border_color(rgb(border()))
        .when(show_toolbar, |el| {
            el.child(
                div()
                    .w_full()
                    .h(px(TOOLBAR_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .gap_4()
                    .text_color(rgb(text_muted()))
                    .children(visible)
                    // Flexible spacer pushes the trailing cluster to the
                    // far right edge of the toolbar.
                    .child(div().flex_1())
                    .children(trailing)
                    // Right-click anywhere on the toolbar opens the
                    // slot-visibility context menu. This replaces the
                    // earlier gear button — same menu, but discoverable
                    // via the conventional right-click gesture and without
                    // taking up toolbar real estate.
                    .when_some(context_menu, |el, cm| {
                        el.on_mouse_down(MouseButton::Right, move |event, _w, cx| {
                            show_slot_menu(&cm, &slots_snapshot, event.position, &on_toggle, cx);
                        })
                    }),
            )
        })
}

// ---------------------------------------------------------------------------
// Right-click ctxmenu — show the slot-visibility menu
// ---------------------------------------------------------------------------

/// Show the slot-visibility context menu at `pos` (window-relative pixels).
///
/// Each slot becomes one menu item, prefixed with `✓` when the slot is
/// currently visible. Clicking an item flips that slot via `on_toggle`,
/// which the caller turns into a config write, AND re-shows the menu with
/// the updated checkmarks — this is a **sticky** menu (see
/// `ContextMenuState::sticky`), so the user can toggle multiple slots in
/// a row without re-right-clicking. The menu closes on backdrop click.
///
/// This is invoked from the toolbar's right-click handler. It mirrors the
/// SFTP panel's `cm.update(cx, |c, cx| c.show(...))` pattern, plus the
/// sticky re-show loop.
pub(crate) fn show_slot_menu(
    cm: &Entity<ContextMenuController>,
    slots: &[ToolbarSlot],
    pos: Point<Pixels>,
    on_toggle: &Rc<dyn Fn(&str, &mut App) + 'static>,
    cx: &mut App,
) {
    let items = build_slot_items(slots, on_toggle, cm, pos);
    cm.update(cx, |c, cx| {
        c.show(
            ContextMenuState {
                position: pos,
                header: Some(t!("toolbar.gear_header").to_string().into()),
                items,
                sticky: true,
                ..ContextMenuState::default()
            },
            cx,
        );
    });
}

/// Build the menu items for the slot-visibility menu.
///
/// Each item's click handler:
///   1. Invokes `on_toggle(id, cx)` to flip the config flag.
///   2. Flips the matching slot's `visible` field in the captured snapshot.
///   3. Replaces the current menu's items in-place (without re-playing the
///      open animation) so the checkmarks update instantly. We do NOT call
///      `ContextMenuController::show` here — that would reset the
///      gpui-animation transition state and replay the pop-in animation,
///      causing the flicker the user reported. Instead we poke the items
///      directly via `update_state_items`, which keeps the menu open with
///      the new items and no animation replay.
fn build_slot_items(
    slots: &[ToolbarSlot],
    on_toggle: &Rc<dyn Fn(&str, &mut App) + 'static>,
    cm: &Entity<ContextMenuController>,
    pos: Point<Pixels>,
) -> Vec<ContextMenuItem> {
    let mut items: Vec<ContextMenuItem> = Vec::with_capacity(slots.len());
    for slot in slots {
        let id = slot.id;
        let label = slot.label.clone();
        let checked = slot.visible;
        let display_label = if checked {
            format!("✓  {label}")
        } else {
            format!("     {label}")
        };
        let on_toggle = on_toggle.clone();
        let cm = cm.clone();
        let slots_snapshot: Vec<ToolbarSlot> = slots.to_vec();
        items.push(ContextMenuItem::new(display_label, move |_w, cx| {
            // 1. Flip the config flag.
            on_toggle(id, cx);
            // 2. Flip the matching slot's `visible` in the snapshot, so the
            //    re-shown menu's checkmark matches the new reality.
            let mut updated = slots_snapshot.clone();
            if let Some(s) = updated.iter_mut().find(|s| s.id == id) {
                s.visible = !s.visible;
            }
            // 3. Build the new items and poke them into the controller's
            //    current state without replaying the open animation.
            let new_items = build_slot_items(&updated, &on_toggle, &cm, pos);
            cm.update(cx, |c, cx| {
                c.replace_items(new_items, cx);
            });
        }));
    }
    items
}

// ---------------------------------------------------------------------------
// (removed) render_gear_button
// ---------------------------------------------------------------------------
//
// The earlier implementation used a gear-shaped button in the toolbar
// that opened the slot-visibility ctxmenu on left-click. It never fired
// reliably — `with_transition` wraps the element in an `AnimatedWrapper`
// that doesn't expose `Div`'s mouse-listener API, and even after reordering
// the calls so `on_mouse_down` was registered on the raw `Stateful<Div>`,
// clicks still didn't reach the handler (likely because an ancestor
// `Div` in the terminal view's mouse overlay was intercepting them).
// The right-click-on-toolbar gesture is more conventional and avoids
// the whole hit-target problem.

/*
fn render_gear_button(
    cm: Entity<ContextMenuController>,
    slots: Vec<ToolbarSlot>,
    on_toggle: Rc<dyn Fn(&str, &mut App) + 'static>,
) -> impl IntoElement {
    ...
}
*/

// ---------------------------------------------------------------------------
// Shared formatting helpers — re-exported for concrete toolbar impls
// ---------------------------------------------------------------------------

/// Format used/total as e.g. "2.1G / 16.0G" or "512.0M / 8.0G"
pub fn format_memory(used: u64, total: u64) -> String {
    let (used_val, used_unit) = human_bytes(used);
    let (total_val, total_unit) = human_bytes(total);
    format!(
        "{:.1}{} / {:.1}{}",
        used_val, used_unit, total_val, total_unit
    )
}

pub fn human_bytes(bytes: u64) -> (f64, &'static str) {
    let b = bytes as f64;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    if b >= GB {
        (b / GB, "G")
    } else if b >= MB {
        (b / MB, "M")
    } else if b >= KB {
        (b / KB, "K")
    } else {
        (b, "B")
    }
}

pub fn format_rate(bytes_per_sec: u64) -> String {
    let (val, unit) = human_bytes(bytes_per_sec);
    format!("{:.1}{}/s", val, unit)
}

/// Format a `done / total` byte ratio for display, e.g. "2.1M / 8.0M".
pub fn format_byte_ratio(done: u64, total: u64) -> String {
    if total == 0 {
        let (d, du) = human_bytes(done);
        format!("{:.1}{}", d, du)
    } else {
        let (d, du) = human_bytes(done);
        let (t, tu) = human_bytes(total);
        format!("{:.1}{} / {:.1}{}", d, du, t, tu)
    }
}

/// Truncate a filesystem path in the *middle*, keeping the head (top-level
/// directory) and tail (filename) visible with `…` between them.
///
/// Lifted verbatim from the previous terminal-toolbar module so the SFTP
/// progress chip keeps its existing behavior.
pub fn truncate_path_middle(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let is_absolute = path.starts_with('/');
    let prefix = if is_absolute { "/" } else { "" };

    if parts.len() >= 3 {
        let head = parts[0];
        let tail = parts.last().unwrap();
        let candidate = format!("{prefix}{head}/…/{tail}");
        if candidate.len() <= max {
            return candidate;
        }
        let budget = max.saturating_sub(prefix.len() + head.len() + 4); // "/…/…"
        if budget > 4 {
            let half = budget / 2;
            let t_len = tail.len();
            if t_len > budget {
                let keep_head = &tail[..half];
                let keep_tail = &tail[t_len - half..];
                return format!("{prefix}{head}/…/{keep_head}…{keep_tail}");
            }
        }
        let tail_budget = max.saturating_sub(prefix.len() + head.len() + 3); // "/…"
        let cut = tail_budget.saturating_sub(1).max(1);
        return format!("{prefix}{head}/…/{}…", &tail[..cut.min(tail.len())]);
    }

    let cut = max.saturating_sub(1);
    let half = cut / 2;
    let chars: Vec<char> = path.chars().collect();
    if chars.len() > cut {
        let head: String = chars[..half].iter().collect();
        let tail: String = chars[chars.len() - half..].iter().collect();
        return format!("{head}…{tail}");
    }
    path.chars().take(cut).collect::<String>() + "…"
}
