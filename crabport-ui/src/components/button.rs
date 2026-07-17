use crate::color::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use std::{rc::Rc, time::Duration};

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    style: StyleRefinement,
    children: Vec<AnyElement>,
    icon: Option<SharedString>,
    on_hover: Option<Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>>,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    selected: Option<bool>,
    disabled: Option<bool>,
    centered: bool,
    /// When true, the content area allows text to wrap across multiple
    /// lines instead of being clamped to a single line with an ellipsis.
    /// Useful for buttons whose label is longer than the button width
    /// (e.g. the host-key confirmation buttons in the connection overlay).
    multiline: bool,
    // Colors
    bg: u32,
    bg_hover: u32,
    bg_selected: u32,
    bg_disabled: u32,
    border: u32,
    text_disabled: u32,
    /// Override for the icon's `text_color`. When `Some`, the svg uses this
    /// instead of the default selected/muted heuristic — used by `.primary()`
    /// so the icon stays light on a colored background.
    icon_color: Option<u32>,
    /// Whether to call [`InteractiveElement::occlude_mouse`] on the root div.
    /// See [`Self::occlude_mouse`].
    occlude_mouse: bool,
    /// Optional platform window-control area to register on the root div.
    /// See [`Self::window_control_area`].
    window_control: Option<gpui::WindowControlArea>,
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: Default::default(),
            children: Default::default(),
            icon: None,
            on_hover: None,
            on_click: None,
            on_close: None,
            selected: None,
            disabled: None,
            centered: false,
            multiline: false,
            bg: btn_bg(),
            bg_hover: btn_bg_hover(),
            bg_selected: btn_bg_selected(),
            bg_disabled: btn_bg_disabled(),
            border: btn_border(),
            text_disabled: btn_text_disabled(),
            icon_color: None,
            occlude_mouse: false,
            window_control: None,
        }
    }

    // -- Color presets --

    pub fn tab(self) -> Self {
        Self {
            bg: tab_btn_bg(),
            bg_hover: tab_btn_bg_hover(),
            bg_selected: tab_btn_bg_selected(),
            bg_disabled: tab_btn_bg_disabled(),
            border: tab_btn_border(),
            text_disabled: tab_btn_text_disabled(),
            ..self
        }
    }

    pub fn primary(self) -> Self {
        Self {
            bg: btn_primary_bg(),
            bg_hover: btn_primary_bg_hover(),
            bg_selected: btn_primary_bg_selected(),
            bg_disabled: btn_primary_bg_disabled(),
            border: btn_primary_border(),
            text_disabled: btn_primary_text_disabled(),
            // The primary button sits on a saturated accent fill, so the
            // icon must be light — same family as text_primary — regardless
            // of selection state. Without this, the default muted icon
            // disappears against the colored background.
            icon_color: Some(text_primary()),
            ..self
        }
    }

    /// Ghost button: transparent background, no visible border. Hover reveals
    /// a subtle surface background. Good for icon-only action buttons nested
    /// inside rows (edit / delete / etc.).
    pub fn ghost(mut self) -> Self {
        self.bg = btn_ghost_bg();
        self.bg_hover = btn_ghost_bg_hover();
        self.bg_selected = btn_ghost_bg_selected();
        self.bg_disabled = btn_ghost_bg_disabled();
        self.border = btn_ghost_border();
        self.text_disabled = btn_ghost_text_disabled();
        self
    }

    // -- Color overrides --

    pub fn bg(mut self, color: u32) -> Self {
        self.bg = color;
        self
    }

    pub fn bg_hover(mut self, color: u32) -> Self {
        self.bg_hover = color;
        self
    }

    pub fn bg_selected(mut self, color: u32) -> Self {
        self.bg_selected = color;
        self
    }

    pub fn bg_disabled(mut self, color: u32) -> Self {
        self.bg_disabled = color;
        self
    }

    pub fn border_color(mut self, color: u32) -> Self {
        self.border = color;
        self
    }

    pub fn text_disabled_color(mut self, color: u32) -> Self {
        self.text_disabled = color;
        self
    }

    // -- Content & behavior --

    pub fn icon(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(path.into());
        self
    }

    /// Mark this button's hitbox as blocking the mouse for elements behind
    /// it. Forwarded to the button's root `div` during render.
    ///
    /// Used by window-control buttons (and tab buttons) so they win the
    /// hit-test over a window-drag region registered on a parent container
    /// (see [`crate::layouts::tabbar::render_tab_bar`] and
    /// [`crate::components::window_layout::render_sidebar_window`]). Without
    /// this, Windows' `WM_NCHITTEST` would return `HTCAPTION` over the
    /// button area and the button's `on_click` would never fire.
    pub fn occlude_mouse(mut self) -> Self {
        self.occlude_mouse = true;
        self
    }

    /// Register this button's hitbox as a platform window-control area so
    /// Windows' `WM_NCHITTEST` returns the matching `HT*BUTTON` code and the
    /// OS handles the NC interaction (correct hover highlight, double-click
    /// maximize, etc.). Forwarded to the button's root `div` during render.
    ///
    /// Only meaningful for the minimize / maximize / close buttons; the
    /// auxiliary About/Settings launcher buttons do not register a control
    /// area (they dispatch an app action via `on_click`).
    pub fn window_control_area(mut self, area: gpui::WindowControlArea) -> Self {
        self.window_control = Some(area);
        self
    }

    /// Override the icon's `text_color`. Use when a button sits on a
    /// colored background where the default muted icon would be unreadable
    /// (e.g. a primary button on a saturated accent fill).
    pub fn icon_color(mut self, color: u32) -> Self {
        self.icon_color = Some(color);
        self
    }

    pub fn on_hover(mut self, f: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Rc::new(f));
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }

    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(f));
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = Some(disabled);
        self
    }

    pub fn centered(mut self, centered: bool) -> Self {
        self.centered = centered;
        self
    }

    /// Allow the button's text content to wrap across multiple lines.
    ///
    /// By default the content area is `whitespace_nowrap` + `text_ellipsis`,
    /// so a long label is truncated to a single line. Enabling multiline
    /// swaps that for `whitespace_normal` and drops the ellipsis so the
    /// label wraps inside the button's width. Pair with an explicit height
    /// (e.g. `.h_10()`) tall enough to fit two lines.
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }

    /// Clean up all gpui-animation state associated with this Button.
    /// Call this when the Button is removed from the render tree
    /// (e.g. when closing a tab) to prevent stale hover/transition state
    /// from persisting in the global DashMap.
    pub fn cleanup_animation(id: &ElementId, has_close: bool) {
        gpui_animation::reset_transition(id);
        if has_close {
            let close_bg_id = ElementId::Name(format!("{}-close-bg", id).into());
            let close_opacity_id = ElementId::Name(format!("{}-close-opacity", id).into());
            gpui_animation::reset_transition(&close_bg_id);
            gpui_animation::reset_transition(&close_opacity_id);
        }
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let has_close = self.on_close.is_some();

        // Move colors out of self so closures can capture by value
        let bg = self.bg;
        let bg_hover = self.bg_hover;
        let bg_selected = self.bg_selected;
        let bg_disabled = self.bg_disabled;
        let border = self.border;
        let text_disabled = self.text_disabled;
        // Interpret each color as rgba() so the alpha channel is respected.
        // Constants ≤ 0xFFFFFF (3-byte RGB) get an implicit opaque alpha
        // (0xff) appended; 4-byte 0xRRGGBBAA constants use their own alpha.
        let to_color = |c: u32| rgba(if c <= 0xFFFFFF { (c << 8) | 0xFF } else { c });

        let mut root = div()
            .id(self.id.clone())
            .flex()
            .items_center()
            .when_else(
                self.centered,
                |this| this.justify_center(),
                |this| this.justify_start(),
            )
            .w_full()
            .border_1()
            .border_color(to_color(border))
            .rounded_md()
            .h_8()
            .overflow_hidden()
            .bg(to_color(bg))
            .when(self.occlude_mouse, |el| el.occlude())
            .when_some(self.window_control, |el, area| el.window_control_area(area));
        root.style().refine(&self.style);

        let multiline = self.multiline;
        // Icon color follows the selection state so unselected tabs read as
        // muted and the active tab pops. GPUI's svg does not inherit
        // text_color from the parent div, so this must be set on the svg
        // itself. A primary button overrides this to keep the icon light on
        // a colored fill.
        let icon_color = self.icon_color.unwrap_or_else(|| {
            if self.selected.unwrap_or_default() {
                text_primary()
            } else {
                text_muted()
            }
        });

        // Icon
        let icon_el = self.icon.map(|path| {
            svg()
                .path(path)
                .size_4()
                .flex_shrink_0()
                .text_color(rgb(icon_color))
                .into_any_element()
        });

        // Content area
        let mut content = div()
            .items_center()
            .gap_2()
            .min_w_0()
            .overflow_hidden()
            .when_else(
                multiline,
                |this| {
                    // Multiline: use a column layout with full width so the
                    // text node is constrained to the button's content box
                    // and actually wraps. `whitespace_normal` only takes
                    // effect when the text element has a bounded width.
                    this.flex_col().w_full().whitespace_normal().text_center()
                },
                |this| this.flex().text_ellipsis().whitespace_nowrap(),
            );

        if has_close {
            content = content.flex_1();
        }

        if let Some(icon) = icon_el {
            content = content.child(icon);
        }

        let content = content.children(self.children);

        // Close area
        // The close container is sized to the icon plus a little padding so
        // the hover region is a comfortable click target without spanning
        // the whole right half of the button.
        let close_el = if has_close {
            let close_bg_id = ElementId::Name(format!("{}-close-bg", self.id).into());
            let close_opacity_id = ElementId::Name(format!("{}-close-opacity", self.id).into());

            Some(
                div()
                    .id(close_opacity_id.clone())
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .mr_1()
                    .opacity(0.)
                    .child(
                        div()
                            .id(close_bg_id)
                            .flex()
                            .items_center()
                            .justify_center()
                            .h_5()
                            .w_5()
                            .rounded_sm()
                            .child(
                                svg()
                                    .path("icons/close.svg")
                                    .size_3p5()
                                    .text_color(rgb(text_primary())),
                            )
                            .on_click({
                                let on_close = self.on_close.clone();
                                move |_e, w, cx| {
                                    if let Some(ref cb) = on_close {
                                        cb(w, cx);
                                    }
                                    cx.stop_propagation();
                                }
                            })
                            .bg(rgb(surface_active())),
                    )
                    .with_transition(close_opacity_id)
                    .transition_on_hover(Duration::from_millis(100), Linear, |hovered, el| {
                        if *hovered {
                            el.opacity(1.)
                        } else {
                            el.opacity(0.)
                        }
                    })
                    .into_any_element(),
            )
        } else {
            None
        };

        let mut root = root.child(content);
        if let Some(close) = close_el {
            root = root.child(close);
        }

        // State transitions — use `move` closures so u32 values are captured by value ('static)
        root.with_transition(self.id).when_else(
            self.disabled.unwrap_or_default(),
            move |this| {
                this.bg(to_color(bg_disabled))
                    .text_color(to_color(text_disabled))
                    .cursor_not_allowed()
            },
            move |this| {
                this.text_color(rgb(text_primary()))
                    .when_some(self.on_hover, |this, on_hover| {
                        this.on_hover(move |h, w, a| (on_hover)(h, w, a))
                    })
                    .when_some(self.on_click, |this, on_click| {
                        this.on_click(move |e, w, a| {
                            (on_click)(e, w, a);
                            // Prevent the click from bubbling to parent
                            // elements (e.g. row double-click handlers).
                            a.stop_propagation();
                        })
                    })
                    .transition_when_else(
                        self.selected.unwrap_or_default(),
                        Duration::from_millis(250),
                        Linear,
                        move |this| this.bg(to_color(bg_selected)),
                        move |this| this.bg(to_color(bg)),
                    )
                    .transition_on_hover(
                        Duration::from_millis(250),
                        Linear,
                        move |hovered, this| {
                            if *hovered {
                                this.bg(to_color(bg_hover))
                            } else {
                                this
                            }
                        },
                    )
            },
        )
    }
}
