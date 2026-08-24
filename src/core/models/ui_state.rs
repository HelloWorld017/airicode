use serde::{Deserialize, Serialize};

use super::message::Metadata;
use super::provider::ModelRef;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DurableUIState {
    pub selected_model: Option<ModelRef>,
    pub selected_mode: Option<String>,
    pub selected_variant: Option<String>,
    pub plugin_state: Metadata,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EphemeralUIState {
    pub draft: String,
    pub cursor: usize,
    pub scroll: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UIState {
    pub durable: DurableUIState,
    pub ephemeral: EphemeralUIState,
}
