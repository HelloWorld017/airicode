use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{MessageId, ToolCallId};

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
    },
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: ToolCallId,
        content: String,
        is_error: bool,
    },
    Reasoning {
        text: String,
    },
}

pub type MessageMetadata = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub content: Vec<MessagePart>,
    pub created_at_ms: u64,
    pub metadata: MessageMetadata,
}

impl Message {
    pub fn new(role: Role, content: Vec<MessagePart>) -> Self {
        let created_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            id: MessageId::new(),
            role,
            content,
            created_at_ms,
            metadata: BTreeMap::new(),
        }
    }

    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self::new(role, vec![MessagePart::Text { text: text.into() }])
    }
}
