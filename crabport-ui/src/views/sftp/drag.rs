//! Drag payloads for SFTP tab file rows.
//!
//! Both drag types carry a `source_side: PanelSide` so that `on_drop`
//! handlers can short-circuit when a file is dropped back onto the same
//! panel it came from (a no-op that would otherwise trigger spurious
//! transfers).

use gpui::*;

use crate::color::*;
use crate::components::host_selector::PanelSide;

/// Drag payload for a local filesystem row being dragged from a local
/// panel. Dropped onto the remote panel, it triggers an upload.
#[derive(Clone, Debug)]
pub struct LocalFileDragValue {
    pub local_path: String,
    pub name: String,
    pub is_dir: bool,
    /// Which panel originated the drag, so `on_drop` handlers can
    /// short-circuit same-panel drops.
    pub source_side: PanelSide,
}

impl Render for LocalFileDragValue {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(rgb(bg_base()))
            .border_1()
            .border_color(rgb(border()))
            .shadow_sm()
            .child(
                svg()
                    .path(if self.is_dir {
                        "icons/folder.svg"
                    } else {
                        "icons/file.svg"
                    })
                    .size_3()
                    .text_color(rgb(text_muted())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_primary()))
                    .child(self.name.clone()),
            )
    }
}

/// Drag payload for a remote SFTP row being dragged from a remote
/// panel. Dropped onto the local panel, it triggers a download.
/// Dropped onto another remote panel, it triggers a remote-to-remote
/// transfer (download to temp, then upload).
#[derive(Clone, Debug)]
pub struct SftpDragValue {
    pub remote_path: String,
    pub name: String,
    pub is_dir: bool,
    /// Which panel originated the drag, so `on_drop` handlers can
    /// short-circuit same-panel drops.
    pub source_side: PanelSide,
}

impl Render for SftpDragValue {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(rgb(bg_base()))
            .border_1()
            .border_color(rgb(border()))
            .shadow_sm()
            .child(
                svg()
                    .path(if self.is_dir {
                        "icons/folder.svg"
                    } else {
                        "icons/file.svg"
                    })
                    .size_3()
                    .text_color(rgb(text_muted())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_primary()))
                    .child(self.name.clone()),
            )
    }
}
