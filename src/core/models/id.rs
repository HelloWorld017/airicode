use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
            pub fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }
            pub fn uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(ProjectId);
id_type!(SessionGroupId);
id_type!(SessionId);
id_type!(TurnId);
id_type!(MessageId);
id_type!(NoteId);
id_type!(ContextPartId);
id_type!(ToolId);
id_type!(ProviderId);
id_type!(PluginId);
id_type!(CommandId);
id_type!(ShellActionId);
id_type!(WorkdirLayerId);
id_type!(CommitId);
id_type!(RegistrationId);

/// Tool call identifiers may originate from a provider protocol (for example
/// OpenAI's `call_...` values), so unlike domain IDs they are opaque strings.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_external(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value.to_string())
    }

    pub fn uuid(&self) -> Uuid {
        self.0
            .parse()
            .unwrap_or_else(|_| Uuid::new_v5(&Uuid::NAMESPACE_OID, self.0.as_bytes()))
    }
}

impl Default for ToolCallId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
