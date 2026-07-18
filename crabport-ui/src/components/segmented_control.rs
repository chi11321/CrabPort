use crate::color::*;
use crate::motion::{DURATION_BASE, DURATION_SLOW, EASE_STANDARD, RADIUS_MD, RADIUS_SM, RADIUS_XS};
use gpui::*;
use gpui_animation::animation::TransitionExt;
use std::rc::Rc;

pub struct Segment {
    pub label: AnyElement,
    /// Optional icon path (e.g. `"icons/folder.svg"`). When set, the
    /// SegmentedControl renders an `svg` alongside `label` and drives its
    /// `text_color` transition itself — GPUI's `svg` element does NOT
    /// inherit `text_color` from the parent div, so a svg passed via
    /// `label` alone can't animate color with the tab's active/hover
    /// state. Keeping the icon as a first-class field lets the control
    /// attach its own transition to the svg.
    pub icon: Option<SharedString>,
    pub on_select: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl Segment {
    /// `label` accepts any element. Pass a `&str` / `SharedString` for a
    /// plain text tab, or an `svg()` / `div()` for a composite tab.
    ///
    /// For an icon-only tab whose color animates with active/hover state,
    /// prefer `.new("", ...).icon("icons/folder.svg")` — see `icon`.
    pub fn new(label: impl IntoElement + 'static) -> Self {
        Self {
            label: label.into_any_element(),
            icon: None,
            on_select: None,
        }
    }

    /// Attach an icon to this segment. The svg's `text_color` is driven by
    /// the SegmentedControl (animated between `text_primary()` when active /
    /// hovered and `text_muted()` otherwise), so pass an empty label if you
    /// want an icon-only tab.
    pub fn icon(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(path.into());
        self
    }

    pub fn on_select(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(f));
        self
    }
}

#[derive(IntoElement)]
pub struct SegmentedControl {
    id: ElementId,
    style: StyleRefinement,
    segments: Vec<Segment>,
    active: usize,
}

impl Styled for SegmentedControl {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl SegmentedControl {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: Default::default(),
            segments: Vec::new(),
            active: 0,
        }
    }

    pub fn segment(mut self, seg: Segment) -> Self {
        self.segments.push(seg);
        self
    }

    pub fn active(mut self, index: usize) -> Self {
        self.active = index;
        self
    }

    /// Clean up all gpui-animation state associated with this SegmentedControl.
    /// Call this when the component is removed from the render tree.
    pub fn cleanup_animation(id: &ElementId, segment_count: usize) {
        let spacer_id: ElementId = ElementId::Name(format!("{}-spacer", id).into());
        gpui_animation::reset_transition(&spacer_id);
        for i in 0..segment_count {
            let tab_id: ElementId = ElementId::Name(format!("{}-tab-{}", id, i).into());
            gpui_animation::reset_transition(&tab_id);
        }
    }
}

impl RenderOnce for SegmentedControl {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let count = self.segments.len().max(1);
        let active = self.active.min(count - 1);

        // Each segment occupies an equal fraction of the total width.
        let seg_width = 1.0_f32 / count as f32;

        // ---------------------------------------------------------------------------
        // Sliding indicator
        //
        // Layout: an absolute track layer fills the inner container. Inside it, a
        // spacer div is animated to width = active * seg_width, pushing the
        // indicator body to the correct position. This avoids percentage `left` on
        // absolute children, which would be sized against the wrong ancestor.
        // ---------------------------------------------------------------------------
        let spacer_id: ElementId = ElementId::Name(format!("{}-spacer", self.id).into());

        let mut spacer = div()
            .id(spacer_id.clone())
            .flex_none()
            .h_full()
            .w(DefiniteLength::Fraction(0.0))
            .with_transition(spacer_id);

        // Register one transition_when_else per segment. The last rule whose
        // condition is true wins. transition_when_else (vs transition_when) gives
        // the library explicit knowledge of both branches, which is required for
        // correct interpolation in both the forward and reverse directions.
        for i in 0..count {
            let target = DefiniteLength::Fraction(i as f32 * seg_width);
            spacer = spacer.transition_when_else(
                active == i,
                DURATION_SLOW,
                EASE_STANDARD,
                move |state| state.w(target),
                |state| state,
            );
        }

        let indicator_id: ElementId = ElementId::Name(format!("{}-indicator", self.id).into());

        // The indicator body is exactly one segment wide. flex_none prevents it
        // from participating in flex growth/shrink. h_full fills the track height.
        let indicator = div()
            .id(indicator_id)
            .flex_none()
            .w(DefiniteLength::Fraction(seg_width))
            .h_full()
            .rounded(RADIUS_SM)
            .bg(rgb(bg_base()));

        // The track is absolute and fills the inner container via inset_0.
        // Children use h_full to match the track height explicitly.
        let track = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .child(spacer)
            .child(indicator);

        // ---------------------------------------------------------------------------
        // Tab labels
        // ---------------------------------------------------------------------------
        let tabs: Vec<AnyElement> = self
            .segments
            .into_iter()
            .enumerate()
            .map(|(i, seg)| {
                let is_active = i == active;
                let on_select = seg.on_select.clone();

                let tab_id: ElementId = ElementId::Name(format!("{}-tab-{}", self.id, i).into());
                let icon_path = seg.icon.clone();

                // Background-hover easing: default bg is fully transparent so
                // the tab blends with the SegmentedControl's own bg. On hover
                // we ease into a semi-transparent surface_hover() (alpha ~0.5)
                // for a subtle wash rather than a solid block. Active state is
                // signalled by the sliding indicator, not bg.
                //
                // text_color is also driven through `transition_when_else`
                // (active → text_primary, inactive → text_muted) because
                // `with_transition` caches the element's style state and
                // replays `state.cur` on each render — a statically-set
                // `.text_color(...)` would be overwritten by the cached value
                // and never follow `is_active` changes after the first render.
                let active_color = text_primary();
                let inactive_color = text_muted();
                let mut tab = div()
                    .id(tab_id.clone())
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .px_3()
                    .py_1()
                    .rounded(RADIUS_XS)
                    .text_sm()
                    .text_color(rgb(if is_active {
                        active_color
                    } else {
                        inactive_color
                    }))
                    .bg(rgba(0x24273a00))
                    .with_transition(tab_id)
                    .transition_on_hover(DURATION_BASE, EASE_STANDARD, move |hovered, state| {
                        if *hovered {
                            state.bg(rgba(0x24273a80))
                        } else {
                            state.bg(rgba(0x24273a00))
                        }
                    })
                    .transition_when_else(
                        is_active,
                        DURATION_BASE,
                        EASE_STANDARD,
                        move |state| state.text_color(rgb(active_color)),
                        move |state| state.text_color(rgb(inactive_color)),
                    );

                // Optional icon. Color is static — svg doesn't implement
                // `ParentElement` so it can't use gpui-animation's
                // `with_transition` / `transition_when_else` yet.
                if let Some(path) = icon_path {
                    let icon_color = if is_active {
                        text_primary()
                    } else {
                        text_muted()
                    };
                    tab = tab.child(
                        svg()
                            .path(path)
                            .size_4()
                            .flex_shrink_0()
                            .text_color(rgb(icon_color)),
                    );
                }

                tab = tab.child(seg.label);

                if let Some(cb) = on_select {
                    tab.on_click(move |_e, w, cx| {
                        cb(w, cx);
                    })
                    .into_any_element()
                } else {
                    tab.into_any_element()
                }
            })
            .collect();

        // ---------------------------------------------------------------------------
        // Root
        // ---------------------------------------------------------------------------
        // inner is `relative` so the absolute track layer positions itself against
        // it rather than a higher ancestor.
        let inner = div()
            .relative()
            .flex()
            .flex_row()
            .w_full()
            .child(track)
            .children(tabs);

        let mut root = div()
            .id(self.id)
            .bg(rgb(surface_active()))
            .rounded(RADIUS_MD)
            .p_0p5()
            .child(inner);

        root.style().refine(&self.style);
        root
    }
}
