#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditBarState {
    pub mode: String,
    pub model: String,
    pub variant: String,
    pub input_state: String,
}

impl EditBarState {
    pub fn text(&self) -> String {
        format!(
            "{}  {}  {}  {}",
            self.mode, self.model, self.variant, self.input_state
        )
    }
}
