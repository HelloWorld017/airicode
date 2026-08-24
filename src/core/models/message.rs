use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::id::{MessageId, ProviderId, ToolCallId, TurnId};
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
pub enum MessagePartContent {
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

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MessagePart {
    pub content: Option<MessagePartContent>,

    #[serde(default)]
    pub provider_data: Option<ProviderData>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderData {
    pub provider_id: ProviderId,
    pub data: Value,
}

impl<'de> Deserialize<'de> for MessagePart {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let (content, provider_data) =
            if raw.get("content").is_some() || raw.get("provider_data").is_some() {
                #[derive(Deserialize)]
                struct Current {
                    #[serde(default)]
                    content: Option<MessagePartContent>,
                    #[serde(default)]
                    provider_data: Option<ProviderData>,
                }

                let current = serde_json::from_value::<Current>(raw).map_err(D::Error::custom)?;
                (current.content, current.provider_data)
            } else {
                let content =
                    serde_json::from_value::<MessagePartContent>(raw).map_err(D::Error::custom)?;
                (Some(content), None)
            };

        if content.is_none() && provider_data.is_none() {
            return Err(D::Error::custom(
                "message part must contain content or provider_data",
            ));
        }

        Ok(Self {
            content,
            provider_data,
        })
    }
}

impl MessagePart {
    pub fn is_valid(&self) -> bool {
        self.content.is_some() || self.provider_data.is_some()
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: Some(MessagePartContent::Text { text: text.into() }),
            provider_data: None,
        }
    }

    pub fn reasoning(text: impl Into<String>) -> Self {
        Self {
            content: Some(MessagePartContent::Reasoning { text: text.into() }),
            provider_data: None,
        }
    }

    pub fn tool_call(id: ToolCallId, name: String, arguments: Value) -> Self {
        Self {
            content: Some(MessagePartContent::ToolCall {
                id,
                name,
                arguments,
            }),
            provider_data: None,
        }
    }

    pub fn tool_result(call_id: ToolCallId, summary: String, result: ToolOutput) -> Self {
        Self {
            content: Some(MessagePartContent::ToolResult {
                call_id,
                summary,
                result,
            }),
            provider_data: None,
        }
    }

    pub fn provider_only(provider_id: ProviderId, data: Value) -> Self {
        Self {
            content: None,
            provider_data: Some(ProviderData { provider_id, data }),
        }
    }

    pub fn with_provider_data(mut self, provider_id: ProviderId, data: Value) -> Self {
        self.provider_data = Some(ProviderData { provider_id, data });
        self
    }
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
    pub fn text(
        role: Role,
        text: impl Into<String>,
        mode: impl Into<String>,
        turn_id: Option<TurnId>,
    ) -> Self {
        Self {
            id: MessageId::new(),
            turn_id,
            role,
            mode: mode.into(),
            content: vec![MessagePart::text(text)],
            created_at: TimeSeq::new(),
            metadata: Metadata::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_parts_round_trip_all_content_variants() -> crate::Result<()> {
        let parts = vec![
            MessagePart::text("text"),
            MessagePart::reasoning("reasoning"),
            MessagePart::tool_call(
                ToolCallId::from_external("call_1"),
                "read".into(),
                serde_json::json!({ "path": "README.md" }),
            ),
            MessagePart::tool_result(
                ToolCallId::from_external("call_1"),
                "done".into(),
                ToolOutput::Success {
                    content: "result".into(),
                },
            ),
        ];

        let encoded = serde_json::to_vec(&parts)?;
        let decoded: Vec<MessagePart> = serde_json::from_slice(&encoded)?;
        assert_eq!(decoded, parts);
        Ok(())
    }

    #[test]
    fn provider_only_parts_preserve_opaque_data() -> crate::Result<()> {
        let provider_id = ProviderId::new();
        let part = MessagePart::provider_only(
            provider_id,
            serde_json::json!({
                "type": "reasoning",
                "encrypted_content": "opaque-token"
            }),
        );
        let decoded: MessagePart = serde_json::from_value(serde_json::to_value(&part)?)?;
        assert_eq!(decoded, part);
        assert!(decoded.content.is_none());
        assert!(decoded.provider_data.is_some());
        let omitted_content: MessagePart = serde_json::from_value(serde_json::json!({
            "provider_data": part.provider_data.clone()
        }))?;
        assert_eq!(omitted_content, part);
        Ok(())
    }

    #[test]
    fn legacy_enum_parts_are_still_deserializable() -> crate::Result<()> {
        let part: MessagePart = serde_json::from_value(serde_json::json!({
            "Text": { "text": "legacy" }
        }))?;
        assert_eq!(part, MessagePart::text("legacy"));
        Ok(())
    }

    #[test]
    fn empty_parts_are_rejected() {
        assert!(serde_json::from_value::<MessagePart>(serde_json::json!({
            "content": null,
            "provider_data": null
        }))
        .is_err());
    }
}
