use serde::{Deserialize, Serialize};

use super::provider::ModelRef;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UIState {
    pub selected_model: Option<ModelRef>,
    pub selected_mode: Option<String>,
    pub selected_variant: Option<String>,
}
