use crate::core::models::{ModelRef, Usage};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusBarState {
    pub title: String,
    pub selected_model: Option<ModelRef>,
    pub usage: Option<Usage>,
    pub status: String,
}

impl StatusBarState {
    pub fn model_label(&self) -> String {
        self.selected_model
            .as_ref()
            .map(|model| format!("{}:{}", model.provider_id, model.model_id))
            .unwrap_or_else(|| "no model".into())
    }

    pub fn text(&self) -> String {
        let usage = self
            .usage
            .as_ref()
            .map(|usage| format!("tokens {}", usage.total_tokens))
            .unwrap_or_else(|| "tokens -".into());
        format!(
            "{} | {} | {} | {}",
            self.title,
            self.model_label(),
            usage,
            self.status
        )
    }
}
