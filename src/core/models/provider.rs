use std::sync::Arc;

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::super::error::Result;
use super::id::ProviderId;
use super::message::Message;
use super::tool::ToolDefinition;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_window: Option<u64>,
    pub tools: bool,
    pub reasoning: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider_id: ProviderId,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProviderEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallDelta {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage {
        usage: Usage,
    },
    Finished {
        reason: FinishReason,
    },
}

pub type ProviderStream = std::pin::Pin<Box<dyn Stream<Item = Result<ProviderEvent>> + Send>>;

#[derive(Clone)]
pub struct ProviderRequest {
    pub model: String,
    pub messages: Vec<Arc<Message>>,
    pub tools: Vec<ToolDefinition>,
    pub cancellation: CancellationToken,
}
