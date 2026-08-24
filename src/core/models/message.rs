use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::id::{MessageId, ToolCallId, TurnId};
use super::tool::ToolOutput;
use crate::utils::TimeSeq;

pub type Metadata = BTreeMap<String, Value>;
pub type MessageMetadata = Metadata;
pub const DEFAULT_MODE: &str = "build";

fn default_mode() -> String {
    DEFAULT_MODE.into()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessagePart {
    Text {
        text: String,
    },
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: ToolCallId,
        summary: String,
        result: ToolOutput,
    },
    Reasoning {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub turn_id: Option<TurnId>,
    pub role: Role,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub content: Vec<MessagePart>,
    pub created_at: TimeSeq,
    pub metadata: Metadata,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>, turn_id: Option<TurnId>) -> Self {
        Self::text_with_mode(role, text, turn_id, DEFAULT_MODE)
    }

    pub fn text_with_mode(
        role: Role,
        text: impl Into<String>,
        turn_id: Option<TurnId>,
        mode: impl Into<String>,
    ) -> Self {
        Self {
            id: MessageId::new(),
            turn_id,
            role,
            mode: mode.into(),
            content: vec![MessagePart::Text { text: text.into() }],
            created_at: TimeSeq::new(),
            metadata: Metadata::new(),
        }
    }
}
