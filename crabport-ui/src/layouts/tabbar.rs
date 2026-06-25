use std::rc::Rc;

use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};

use crate::app::{CrabportApp, Tab, TabKind};
use crate::color::*;
use crate::components::button::Button;
use crate::components::button_with_close::ButtonWithClose;

pub fn render_tab_bar(
    handle: &Entity<CrabportApp>,
    tabs: &[Tab],
    active_tab_id: u64,
    is_home: bool,
    on_close: Rc<dyn Fn(u64, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let h = handle.clone();
    div()
        .id("tabbar")
        .w_full()
        .h_11()
        .bg(rgb(BG_TAB_BAR))
        .border_b_1()
        .border_color(rgb(BORDER))
        .flex()
        .items_center()
        .py_1()
        .gap_1()
        .px_2()
        .with_transition("tabbar")
        .transition_when(
            is_home,
            std::time::Duration::from_millis(150),
            Linear,
            |el| el.pl_1(),
        )
        .transition_when(
            !is_home,
            std::time::Duration::from_millis(150),
            Linear,
            |el| el.pl_20(),
        )
        .children(tabs.iter().map(|tab| {
            let is_active = tab.id == active_tab_id;
            let is_home_tab = tab.kind == TabKind::Home;
            let h2 = handle.clone();
            let tab_id = tab.id;
            let wrapper_id = ElementId::Name(format!("tab-wrapper-{}", tab.id).into());
            let on_close = on_close.clone();
            div()
                .id(wrapper_id.clone())
                .flex()
                .items_center()
                .with_transition(wrapper_id)
                .transition_when(
                    is_active,
                    std::time::Duration::from_millis(150),
                    Linear,
                    |el| el.w_48(),
                )
                .transition_when(
                    !is_active,
                    std::time::Duration::from_millis(150),
                    Linear,
                    |el| el.w_24(),
                )
                .child({
                    let mut btn =
                        ButtonWithClose::new(ElementId::Name(format!("tab-{}", tab.id).into()))
                            .selected(is_active)
                            .child(tab.title.clone())
                            .h_9()
                            .w_full()
                            .border_0()
                            .px_3()
                            .text_sm()
                            .on_click(move |_e, _w, cx| {
                                h2.update(cx, |app, _| {
                                    app.activate_tab(tab_id);
                                });
                            });
                    if !is_home_tab {
                        let tab_id = tab.id;
                        let on_close = on_close.clone();
                        btn = btn.on_close(move |w, cx| {
                            on_close(tab_id, w, cx);
                        });
                    }
                    btn
                })
        }))
        .child(
            Button::new("tab-add")
                .centered(true)
                .child(
                    svg()
                        .path("icons/plus.svg")
                        .size_4()
                        .text_color(rgb(TEXT_MUTED)),
                )
                .h_9()
                .w_9()
                .border_0()
                .px_0()
                .text_sm()
                .on_click(move |_e, _w, cx| {
                    h.update(cx, |app, cx| {
                        app.show_command = true;
                        cx.notify();
                    });
                }),
        )
}
