use crate::components::segmented_control::{Segment, SegmentedControl};
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_animation::transition::general::EaseInOutQuad;
use std::time::Duration;

// Tabs::new("my-tabs")
//     .active(self.active_tab)
//     .on_change(cx.listener(|this, idx, _, cx| {
//         this.active_tab = *idx;
//         cx.notify();
//     }))
//     .pane(TabPane::new("Overview", my_overview_component()).height(px(400.)))
//     .pane(TabPane::new("Settings", my_settings_component()).height(px(300.)))
//     .pane(TabPane::new("History",  my_history_component()))

// ---------------------------------------------------------------------------
// TabPane
// ---------------------------------------------------------------------------
pub struct TabPane {
    pub label: AnyElement,
    /// Optional icon path forwarded to the segment so the SegmentedControl
    /// can drive the svg's color transition itself.
    pub icon: Option<SharedString>,
    pub content: AnyElement,
    /// Optional height for this pane's content area. When set on every pane,
    /// the Tabs component animates `max_height` to the active pane's height
    /// on tab switch (ease in/out). When `None` on any pane, the content
    /// area falls back to `flex_1` (fill remaining space, no height anim).
    pub height: Option<DefiniteLength>,
}

impl TabPane {
    /// `label` accepts any element. Pass a `&str` / `SharedString` for a
    /// plain text tab, or an `svg()` for a composite tab.
    ///
    /// For an icon-only tab whose color animates with active/hover, use
    /// `.new("", content).icon("icons/folder.svg")`.
    pub fn new(label: impl IntoElement + 'static, content: impl IntoElement + 'static) -> Self {
        Self {
            label: label.into_any_element(),
            icon: None,
            content: content.into_any_element(),
            height: None,
        }
    }

    /// Attach an icon to this pane's tab. See [`Segment::icon`].
    pub fn icon(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(path.into());
        self
    }

    /// Set the content area height for this pane. When all panes specify a
    /// height, the Tabs component eases `max_height` between values on tab
    /// switch. Use `px(...)` for a fixed pixel height.
    pub fn height(mut self, h: impl Into<DefiniteLength>) -> Self {
        self.height = Some(h.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------
#[derive(IntoElement)]
pub struct Tabs {
    id: ElementId,
    style: StyleRefinement,
    ctrl_style: StyleRefinement,
    panes: Vec<TabPane>,
    active: usize,
    on_change: Option<std::rc::Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>>,
}

impl Styled for Tabs {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Tabs {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: Default::default(),
            ctrl_style: Default::default(),
            panes: Vec::new(),
            active: 0,
            on_change: None,
        }
    }

    pub fn pane(mut self, pane: TabPane) -> Self {
        self.panes.push(pane);
        self
    }

    pub fn active(mut self, index: usize) -> Self {
        self.active = index;
        self
    }

    pub fn on_change(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(std::rc::Rc::new(f));
        self
    }

    /// Apply custom styles to the internal SegmentedControl (tab bar).
    pub fn ctrl_style(mut self, f: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Self {
        self.ctrl_style = f(self.ctrl_style);
        self
    }

    /// Clean up all gpui-animation state associated with this Tabs component,
    /// including its internal SegmentedControl, track, panels, and content area.
    /// Call this when the component is removed from the render tree.
    pub fn cleanup_animation(id: &ElementId, pane_count: usize) {
        gpui_animation::reset_transition(id);

        // Internal SegmentedControl
        let ctrl_id = ElementId::Name(format!("{}-ctrl", id).into());
        SegmentedControl::cleanup_animation(&ctrl_id, pane_count);

        // Track
        let track_id = ElementId::Name(format!("{}-slide-track", id).into());
        gpui_animation::reset_transition(&track_id);

        // Content area (height transition)
        let content_id = ElementId::Name(format!("{}-content", id).into());
        gpui_animation::reset_transition(&content_id);

        // Panels
        for i in 0..pane_count {
            let panel_id = ElementId::Name(format!("{}-panel-{}", id, i).into());
            gpui_animation::reset_transition(&panel_id);
        }
    }
}

impl RenderOnce for Tabs {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let count = self.panes.len().max(1);
        let active = self.active.min(count - 1);

        // If every pane specifies a height, drive the content area's
        // `max_height` via a transition (eases between heights on switch).
        // Otherwise fall back to flex_1 (fill remaining space, original
        // behavior) so callers who don't care about height aren't affected.
        let height_driven = !self.panes.is_empty() && self.panes.iter().all(|p| p.height.is_some());
        // Snapshot the per-pane heights before we move `panes` below.
        let pane_heights: Vec<DefiniteLength> = if height_driven {
            self.panes.iter().map(|p| p.height.unwrap()).collect()
        } else {
            Vec::new()
        };

        // -----------------------------------------------------------------------
        // SegmentedControl
        // -----------------------------------------------------------------------
        let on_change_rc: Option<std::rc::Rc<dyn Fn(usize, &mut Window, &mut App)>> =
            self.on_change;

        let mut ctrl = SegmentedControl::new(ElementId::Name(format!("{}-ctrl", self.id).into()))
            .active(active);
        ctrl.style().refine(&self.ctrl_style);

        let track_id = ElementId::Name(format!("{}-slide-track", self.id).into());
        let panel_w = DefiniteLength::Fraction(1.0_f32 / count as f32);

        // Decompose panes into (segment, panel_element) pairs in one pass.
        // `AnyElement` isn't `Clone`, so we can't share labels/contents
        // across two separate loops — we move them once here and feed the
        // two halves into SegmentedControl / panels below.
        let mut segments: Vec<Segment> = Vec::new();
        let mut panels: Vec<AnyElement> = Vec::new();
        for (i, pane) in self.panes.into_iter().enumerate() {
            let cb = on_change_rc.clone();
            let is_active = i == active;
            let panel_id = ElementId::Name(format!("{}-panel-{}", self.id, i).into());

            let icon = pane.icon;
            let mut seg = Segment::new(pane.label).on_select(move |w, cx| {
                if let Some(f) = &cb {
                    f(i, w, cx);
                }
            });
            if let Some(path) = icon {
                seg = seg.icon(path);
            }
            segments.push(seg);
            panels.push(
                div()
                    .id(panel_id.clone())
                    .flex_none()
                    .w(panel_w)
                    .h_full()
                    .overflow_hidden()
                    .opacity(0.)
                    .with_transition(panel_id)
                    .transition_when_else(
                        is_active,
                        Duration::from_millis(280),
                        EaseInOutQuad,
                        |state| state.opacity(1.),
                        |state| state.opacity(0.),
                    )
                    .child(pane.content)
                    .into_any_element(),
            );
        }
        for seg in segments {
            ctrl = ctrl.segment(seg);
        }

        let mut track = div()
            .id(track_id.clone())
            .absolute()
            .flex()
            .flex_row()
            .h_full()
            .w(DefiniteLength::Fraction(count as f32))
            .with_transition(track_id)
            .children(panels);

        for i in 0..count {
            let target = DefiniteLength::Fraction(-(i as f32));
            track = track.transition_when_else(
                active == i,
                Duration::from_millis(320),
                EaseInOutQuad,
                move |state| state.left(target),
                |state| state,
            );
        }

        // -----------------------------------------------------------------------
        // Content area
        //
        // height-driven mode: a sizing div (whose height transitions to the
        // active pane's height) occupies layout space and sizes the content
        // area. The absolute track floats on top. `max_h` on the content
        // area clamps during the ease so content never overflows mid-
        // transition.
        // flex-fill mode: `flex_1` fills remaining space (original behavior).
        // -----------------------------------------------------------------------
        let content_id = ElementId::Name(format!("{}-content", self.id).into());

        let content: AnyElement = if height_driven {
            let sizing_id = ElementId::Name(format!("{}-content-sizing", self.id).into());
            let mut sizing = div()
                .id(sizing_id.clone())
                .w_full()
                .with_transition(sizing_id);
            let sizing_heights = pane_heights.clone();
            for (i, h) in sizing_heights.into_iter().enumerate() {
                sizing = sizing.transition_when_else(
                    active == i,
                    Duration::from_millis(320),
                    EaseInOutQuad,
                    move |state| state.h(h).max_h(h),
                    |state| state,
                );
            }

            let mut area = div()
                .id(content_id.clone())
                .relative()
                .w_full()
                .overflow_hidden()
                .with_transition(content_id);
            for (i, h) in pane_heights.into_iter().enumerate() {
                area = area.transition_when_else(
                    active == i,
                    Duration::from_millis(320),
                    EaseInOutQuad,
                    move |state| state.max_h(h),
                    |state| state,
                );
            }
            area.child(track).child(sizing).into_any_element()
        } else {
            div()
                .id(content_id)
                .relative()
                .w_full()
                .flex_1()
                .overflow_hidden()
                .child(track)
                .into_any_element()
        };

        // -----------------------------------------------------------------------
        // Root
        //
        // height-driven mode: root has no `h_full` — it sizes to the ctrl
        //   bar + the content area's animated `max_h`, so the whole
        //   component grows/shrinks with the active pane's height.
        // flex-fill mode: `h_full` so the content area's `flex_1` can fill
        //   the remaining vertical space.
        // -----------------------------------------------------------------------
        let mut root = div().id(self.id).flex().flex_col().w_full().gap_2();

        if !height_driven {
            root = root.h_full();
        }

        root = root.child(ctrl).child(content);

        root.style().refine(&self.style);
        root
    }
}
