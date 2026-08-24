use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{Context, Message, ProviderId, Result, ToolDefinition};

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Model {
    pub id: String,
    pub display_name: String,
}

#[derive(Clone, Debug)]
pub struct ProviderRequest {
    pub mode: ProviderMode,
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub context: Context,
    pub cancellation: CancellationToken,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMode {
    #[default]
    Normal,
    Compaction,
    SideQuery,
    Subagent,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent>> + Send>>;

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn get_models(&self) -> Result<Vec<Model>>;
    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream>;
}
