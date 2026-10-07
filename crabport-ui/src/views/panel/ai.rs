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

use crabport_ai::{AiError, ChatMessage, ChatRequest, ChatResponse, ChatStream, StreamEvent};
use crabport_core::config;
use gpui_component::ActiveTheme as _;
use gpui_component::text::{TextView, TextViewStyle};

use crate::ai;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::motion::RADIUS_MD;

/// Pinned ahead of every conversation. Kept short; the agent prompt (tools,
/// safety rules) will extend this in a later phase.
const SYSTEM_PROMPT: &str =
    "You are the built-in AI assistant of CrabPort, an SSH/SFTP client. Be concise and practical.";

/// Body text size for the conversation (user bubbles, assistant answers and
/// the live streaming tail), in px. Deliberately below the app default
/// (16px — gpui-component's `Theme::font_size`) so the panel reads as a
/// compact chat sidebar instead of a document. Markdown headings and code
/// blocks are scaled from this too.
const CONVERSATION_TEXT_SIZE: f32 = 13.0;

/// One entry in the combined provider·model picker.
#[derive(Clone)]
struct ComboItem {
    provider_id: String,
    model: String,
}

/// Committed display role.
#[derive(Clone, Copy)]
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
}

impl Default for AiPanel {
    fn default() -> Self {
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
        }
    }
}

impl AiPanel {
    pub fn new() -> Self {
        Self::default()
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
        });
        if stick {
            self.scroll_list_to_end();
        }

        let model = config::snapshot().ai.model;
        let mut wire: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 1);
        wire.push(ChatMessage::system(SYSTEM_PROMPT));
        for msg in &self.messages {
            wire.push(match msg.role {
                DisplayRole::User => ChatMessage::user(msg.content.clone()),
                DisplayRole::Assistant => ChatMessage::assistant(msg.content.clone()),
            });
        }
        let request = ChatRequest::new(model).with_messages(wire);

        let stream = crabport_ai::spawn_chat_stream(Arc::new(provider), request);
        self.generation += 1;
        let generation = self.generation;
        self.stream = Some(stream.clone());
        self.stream_buf.clear();
        self.stream_reasoning.clear();
        self.error = None;
        self.pending_clear = true;
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
                self.push_partial(text, reasoning);
                // Tools aren't advertised in the chat MVP, so
                // `finish_reason == ToolCalls` can't occur here.
            }
            Err(AiError::Cancelled) => {
                // Keep whatever streamed in before the stop.
                self.push_partial(partial, partial_reasoning);
            }
            Err(err) => {
                self.push_partial(partial, partial_reasoning);
                self.error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    /// Commit an assistant turn unless both streams (answer and reasoning)
    /// are blank.
    fn push_partial(&mut self, content: String, reasoning: String) {
        if content.trim().is_empty() && reasoning.trim().is_empty() {
            return;
        }
        self.messages.push(DisplayMessage {
            role: DisplayRole::Assistant,
            content,
            reasoning,
            // Disclosure starts collapsed; the live streaming tail shows
            // reasoning in full while it streams.
            reasoning_expanded: false,
        });
        // The committed block (Markdown) replaces the streaming tail (plain
        // text) at the same list index, usually with a different height —
        // drop the cached row measurement so the list re-measures it. Only
        // when the tail actually made it into the list (its presence is what
        // makes the cached row count line up with `messages`).
        let count = self.list_state.item_count();
        if count > 0 && count == self.messages.len() {
            self.list_state.splice(count - 1..count, 1);
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
    fn message_items(&self) -> Vec<MessageItem> {
        let mut items: Vec<MessageItem> = self
            .messages
            .iter()
            .map(|msg| match msg.role {
                DisplayRole::User => MessageItem::User {
                    content: msg.content.clone(),
                },
                DisplayRole::Assistant => MessageItem::Assistant {
                    reasoning: msg.reasoning.clone(),
                    content: msg.content.clone(),
                    reasoning_expanded: msg.reasoning_expanded,
                },
            })
            .collect();
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
        let item_count = self.messages.len()
            + usize::from(!self.stream_reasoning.is_empty() || !self.stream_buf.is_empty());
        let known = self.list_state.item_count();
        if known < item_count {
            self.list_state.splice(known..known, item_count - known);
        } else if known > item_count {
            self.list_state.splice(item_count..known, 0);
        }
        // The list's render closure cannot borrow the view, so rows are
        // built from a per-frame snapshot plus a weak handle (used by the
        // thinking disclosures).
        let items: Rc<Vec<MessageItem>> = Rc::new(self.message_items());
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
        reasoning: String,
        content: String,
        reasoning_expanded: bool,
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
        Some(MessageItem::Assistant {
            reasoning,
            content,
            reasoning_expanded,
        }) => row
            .child(assistant_block(
                ix,
                reasoning.clone(),
                content.clone(),
                *reasoning_expanded,
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
