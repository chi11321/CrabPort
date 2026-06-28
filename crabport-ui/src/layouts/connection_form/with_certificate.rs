use gpui::*;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::components::input::{StyledInput, StyledPasswordInput};

#[derive(IntoElement)]
pub struct WithCertificateForm {
    pub passphrase_input: Entity<InputState>,
    pub private_key_input: Entity<InputState>,
    pub passphrase_focused: bool,
    pub private_key_focused: bool,
}

impl RenderOnce for WithCertificateForm {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            // Passphrase
            .child(
                div().child(
                    StyledPasswordInput::new("passphrase", self.passphrase_input)
                        .label(t!("connection_form.passphrase").to_string())
                        .focused(self.passphrase_focused)
                        .on_toggle(|_, _| {}),
                ),
            )
            // Private Key
            .child(
                div().child(
                    StyledInput::new("conn-private-key", self.private_key_input)
                        .label(t!("connection_form.private_key").to_string())
                        .focused(self.private_key_focused)
                        .multi_line(true)
                        .rows(5),
                ),
            )
    }
}
