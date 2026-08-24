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
id_type!(ToolCallId);
id_type!(ProviderId);
id_type!(PluginId);
id_type!(CommandId);
id_type!(ShellActionId);
id_type!(WorkdirLayerId);
id_type!(CommitId);
id_type!(RegistrationId);
