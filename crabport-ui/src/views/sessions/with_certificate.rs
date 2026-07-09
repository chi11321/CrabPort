use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::input::{StyledInput, StyledPasswordInput};

#[derive(IntoElement)]
pub struct WithCertificateForm {
    pub passphrase_input: Entity<InputState>,
    pub private_key_input: Entity<InputState>,
    /// Read-only file path filled by the "Browse…" button. Either this or
    /// `private_key_input` (pasted key content) must be set to pass validation.
    pub private_key_path_input: Entity<InputState>,
    pub passphrase_focused: bool,
    pub private_key_focused: bool,
    pub private_key_path_focused: bool,
    /// Per-field validation error for the private key (passphrase is optional).
    /// Shown on the content textarea; the path field is read-only so the same
    /// error string is surfaced there too when applicable.
    pub private_key_error: Option<SharedString>,
    /// App entity used to reach the form state when the Browse button fires.
    pub app: Entity<CrabportApp>,
}

impl RenderOnce for WithCertificateForm {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let private_key_error = self.private_key_error.clone();
        let app = self.app.clone();
        let browse_label = t!("connection_form.browse").to_string();

        div()
            .flex()
            .flex_col()
            .gap_4()
            // Passphrase (optional)
            .child(
                div().child(
                    StyledPasswordInput::new("passphrase", self.passphrase_input)
                        .label(t!("connection_form.passphrase").to_string())
                        .focused(self.passphrase_focused)
                        .on_toggle(|_, _| {}),
                ),
            )
            // Private Key file path (read-only input + Browse button).
            //
            // The input is disabled so the user cannot type a path manually —
            // the only way to fill it is via the native file picker opened by
            // the Browse button. This keeps the path authoritative (always a
            // real picked file) and avoids drift between a hand-typed path
            // and the actual file on disk.
            .child(
                div().child(
                    StyledInput::new("conn-private-key-path", self.private_key_path_input)
                        .label(t!("connection_form.private_key_file").to_string())
                        .focused(self.private_key_path_focused)
                        // Block keyboard editing but keep the shell visually
                        // enabled so the inline Browse suffix button stays
                        // bright and clickable.
                        .input_disabled(true)
                        .prefix(
                            svg()
                                .path("icons/file.svg")
                                .size_3p5()
                                .text_color(rgb(text_muted())),
                        )
                        .suffix(
                            // Compact inline button (the full `Button` component
                            // is `w_full` + `h_8`, which is too tall for a
                            // suffix slot). We render a small padded clickable
                            // div with a hover background instead.
                            div()
                                .id("conn-private-key-browse")
                                .flex()
                                .items_center()
                                .h(px(22.0))
                                .px_2()
                                .rounded_sm()
                                .text_xs()
                                .text_color(rgb(text_primary()))
                                .bg(rgb(surface_hover()))
                                .hover(|s| s.bg(rgb(surface_active())))
                                .child(browse_label)
                                // The click handler drives the whole flow:
                                //   1. open the native picker (GPUI's
                                //      `prompt_for_paths`, same API the SFTP
                                //      panel uses) and capture its result channel,
                                //   2. capture the window handle so the
                                //      write-back (which needs `&mut Window`
                                //      for `InputState::set_value`) can re-enter
                                //      this window,
                                //   3. spawn a background task that awaits the
                                //      picker and, on success, writes the path
                                //      into the read-only field and clears any
                                //      stale pasted key content.
                                .on_click(move |_, w, cx| {
                                    app_pick_private_key(&app, w, cx);
                                }),
                        )
                        .when_some(private_key_error.clone(), |el, e| el.error(e)),
                ),
            )
            // Private Key content (required — alternatively to the path above)
            .child(
                div().child(
                    StyledInput::new("conn-private-key", self.private_key_input)
                        .label(t!("connection_form.private_key_content").to_string())
                        .focused(self.private_key_focused)
                        .multi_line(true)
                        .rows(5)
                        .when_some(private_key_error, |el, e| el.error(e)),
                ),
            )
    }
}

/// Open the native single-file picker and write the chosen path into the
/// connection form's read-only `private_key_path_input`, clearing any pasted
/// key content so the two fields stay mutually exclusive.
///
/// `prompt_for_paths` runs the platform dialog off the main thread; the
/// returned channel is awaited on a `cx.spawn` task. The write-back routes
/// through `update_window` (rather than `AsyncApp::update`, which only yields
/// `&mut App`) because `InputState::set_value` requires `&mut Window`.
fn app_pick_private_key(app: &Entity<CrabportApp>, window: &mut Window, cx: &mut App) {
    let window_handle = window.window_handle();
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("connection_form.private_key_file").to_string().into()),
    });
    let app = app.clone();

    cx.spawn(async move |cx| {
        // Outer Result = channel state; inner Result = platform error;
        // Option = None when the user cancels.
        let picked = match rx.await {
            Ok(Ok(Some(mut paths))) => paths.pop(),
            Ok(Ok(None)) => {
                // User cancelled — silent.
                return;
            }
            Ok(Err(e)) => {
                tracing::warn!("private-key file picker error: {e}");
                return;
            }
            Err(e) => {
                tracing::warn!("private-key file picker channel closed: {e}");
                return;
            }
        };
        let Some(path) = picked else {
            return;
        };
        let path_str = path.to_string_lossy().to_string();

        // `set_value` needs `&mut Window`; `AsyncApp::update` only yields
        // `&mut App`, so route through `update_window` which provides both.
        let _ = cx.update_window(window_handle, |_view, window, cx| {
            let _ = app.update(cx, |app, cx| {
                if let Some(ref form) = app.connection_form {
                    // Write the picked path into the read-only field.
                    form.private_key_path_input.update(cx, |state, cx| {
                        state.set_value(&path_str, window, cx);
                    });
                    // Clear stale pasted content so the path wins —
                    // `private_key_value()` prefers pasted content.
                    form.private_key_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                    cx.notify();
                }
            });
        });
    })
    .detach();
}
