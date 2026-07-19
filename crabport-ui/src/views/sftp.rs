//! Full-screen dual-panel SFTP file browser tab view.
//!
//! Replaces the placeholder single-panel rendering for `TabKind::Sftp`.
//! Mirrors the Termius-style dual-pane layout: local filesystem on the
//! left, remote (SFTP) filesystem on the right, each with its own path
//! bar, action buttons, virtual-list file table, multi-selection, context
//! menu, and drag-and-drop. The remote panel's SSH connection is driven
//! by a hidden `TerminalView` (passed in via `set_state` each render).

mod drag;
mod helpers;
mod panel;
pub mod toolbar;
pub mod transfer_history;
pub mod view;

pub use drag::{LocalFileDragValue, SftpDragValue};
pub use toolbar::render_sftp_history_toggle;
pub use transfer_history::{TransferHistoryController, TransferRecord};
pub use view::{PanelHost, SftpTabView};
