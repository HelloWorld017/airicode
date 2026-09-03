use std::{fmt, path::Path, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const PROJECT_ID_BYTES: usize = 12;
const SESSION_GROUP_ID_BYTES: usize = 12;
const SESSION_ID_BYTES: usize = 18;

fn decode_base64<const N: usize>(value: &str, name: &str) -> Result<[u8; N], String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| format!("invalid {name}: {error}"))?;
    let length = bytes.len();
    bytes
        .try_into()
        .map_err(|_: Vec<u8>| format!("invalid {name} length: expected {N} bytes, got {length}"))
}

fn serialize_base64<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectId([u8; PROJECT_ID_BYTES]);

impl ProjectId {
    pub fn from_workdir(path: &Path) -> Self {
        let mut digest = Sha256::new();
        digest.update(path.as_os_str().to_string_lossy().as_bytes());
        let digest = digest.finalize();
        let mut value = [0; PROJECT_ID_BYTES];
        value.copy_from_slice(&digest[..PROJECT_ID_BYTES]);
        Self(value)
    }

    pub fn from_base64(value: &str) -> Result<Self, String> {
        decode_base64(value, "project id").map(Self)
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl FromStr for ProjectId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_base64(value)
    }
}

impl Serialize for ProjectId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_base64(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for ProjectId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_base64(&value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionGroupId([u8; SESSION_GROUP_ID_BYTES]);

impl SessionGroupId {
    pub fn new() -> Self {
        let mut value = [0; SESSION_GROUP_ID_BYTES];
        value.copy_from_slice(&Uuid::new_v4().as_bytes()[..SESSION_GROUP_ID_BYTES]);
        Self(value)
    }

    pub fn from_base64(value: &str) -> Result<Self, String> {
        decode_base64(value, "session group id").map(Self)
    }
}

impl Default for SessionGroupId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl FromStr for SessionGroupId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_base64(value)
    }
}

impl Serialize for SessionGroupId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_base64(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for SessionGroupId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_base64(&value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId([u8; SESSION_ID_BYTES]);

impl SessionId {
    pub fn new(group_id: SessionGroupId) -> Self {
        let mut value = [0; SESSION_ID_BYTES];
        value[..SESSION_GROUP_ID_BYTES].copy_from_slice(&group_id.0);
        value[SESSION_GROUP_ID_BYTES..].copy_from_slice(&Uuid::new_v4().as_bytes()[..6]);
        Self(value)
    }

    pub fn group_id(self) -> SessionGroupId {
        let mut value = [0; SESSION_GROUP_ID_BYTES];
        value.copy_from_slice(&self.0[..SESSION_GROUP_ID_BYTES]);
        SessionGroupId(value)
    }

    pub fn from_base64(value: &str) -> Result<Self, String> {
        decode_base64(value, "session id").map(Self)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl FromStr for SessionId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_base64(value)
    }
}

impl Serialize for SessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_base64(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_base64(&value).map_err(D::Error::custom)
    }
}

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
