use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::id::{MessageId, ToolCallId, TurnId};
use super::now_ms;
use super::tool::ToolOutput;

pub type Metadata = BTreeMap<String, Value>;
pub type MessageMetadata = Metadata;

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
    pub content: Vec<MessagePart>,
    pub created_at_ms: u64,
    pub metadata: Metadata,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>, turn_id: Option<TurnId>) -> Self {
        Self {
            id: MessageId::new(),
            turn_id,
            role,
            content: vec![MessagePart::Text { text: text.into() }],
            created_at_ms: now_ms(),
            metadata: Metadata::new(),
        }
    }
}
