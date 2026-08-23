use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{
    Error, Message, MessageId, Plugin, PluginId, PluginRegistrar, Result, SessionId, Tool,
    ToolContext, ToolDefinition, ToolId, ToolOutput,
};

const PLUGIN_ID: &str = "builtin.fork";
const TOOL_ID: &str = "builtin.fork";
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default)]
pub struct ForkConfig {
    /// When set, each fork is written as a self-contained JSON artifact in this directory.
    pub storage_dir: Option<PathBuf>,
}

impl ForkConfig {
    pub fn stored_in(path: impl Into<PathBuf>) -> Self {
        Self {
            storage_dir: Some(path.into()),
        }
    }
}

pub fn fork_plugin(config: ForkConfig) -> Arc<dyn Plugin> {
    Arc::new(ForkPlugin { config })
}

struct ForkPlugin {
    config: ForkConfig,
}

#[async_trait]
impl Plugin for ForkPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_tool(
            0,
            Arc::new(ForkTool {
                storage_dir: self.config.storage_dir.clone(),
            }),
        )
    }
}

struct ForkTool {
    storage_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkInput {
    #[serde(default)]
    through_message_id: Option<MessageId>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ForkProvenance {
    source_session_id: SessionId,
    through_message_id: Option<MessageId>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ForkArtifact {
    schema_version: u32,
    session_id: SessionId,
    provenance: ForkProvenance,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct StoredFork<'a> {
    session_id: SessionId,
    provenance: &'a ForkProvenance,
    artifact_path: &'a Path,
}

#[async_trait]
impl Tool for ForkTool {
    fn id(&self) -> ToolId {
        ToolId::new(TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fork".into(),
            description: "Create a self-contained fork of this session's messages.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "through_message_id": {
                        "type": "string",
                        "description": "Last message to include; omit to include all current messages."
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let input: ForkInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid fork input: {error}")))?;
        let end = match input.through_message_id {
            Some(id) => context
                .messages()
                .iter()
                .position(|message| message.id == id)
                .map(|index| index + 1)
                .ok_or_else(|| {
                    Error::Tool(format!(
                        "message {id} is not in session {}",
                        context.session_id
                    ))
                })?,
            None => context.messages().len(),
        };
        let artifact = ForkArtifact {
            schema_version: SCHEMA_VERSION,
            session_id: SessionId::new(),
            provenance: ForkProvenance {
                source_session_id: context.session_id,
                through_message_id: input.through_message_id,
            },
            messages: context.messages()[..end].to_vec(),
        };

        let (content, artifact_path) = if let Some(storage_dir) = &self.storage_dir {
            let path = write_artifact(storage_dir, &artifact)?;
            let content = serde_json::to_string(&StoredFork {
                session_id: artifact.session_id,
                provenance: &artifact.provenance,
                artifact_path: &path,
            })
            .map_err(|error| Error::Tool(format!("could not encode fork result: {error}")))?;
            (content, Some(path))
        } else {
            let content = serde_json::to_string(&artifact)
                .map_err(|error| Error::Tool(format!("could not encode fork: {error}")))?;
            (content, None)
        };

        context
            .emit_feature(
                "fork.created",
                serde_json::json!({
                    "source_session_id": context.session_id,
                    "session_id": artifact.session_id,
                    "through_message_id": artifact.provenance.through_message_id,
                    "message_count": artifact.messages.len(),
                    "artifact_path": artifact_path,
                }),
            )
            .await?;
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

fn write_artifact(storage_dir: &Path, artifact: &ForkArtifact) -> Result<PathBuf> {
    fs::create_dir_all(storage_dir).map_err(|error| {
        Error::Tool(format!(
            "could not create fork storage {}: {error}",
            storage_dir.display()
        ))
    })?;
    let path = storage_dir.join(format!("{}.json", artifact.session_id));
    let encoded = serde_json::to_vec_pretty(artifact)
        .map_err(|error| Error::Tool(format!("could not encode fork artifact: {error}")))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            Error::Tool(format!(
                "could not create fork artifact {}: {error}",
                path.display()
            ))
        })?;
    file.write_all(&encoded).map_err(|error| {
        Error::Tool(format!(
            "could not write fork artifact {}: {error}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        Error::Tool(format!(
            "could not sync fork artifact {}: {error}",
            path.display()
        ))
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Core, Role};

    #[tokio::test]
    async fn plugin_registers_only_its_fork_tool() {
        let core = Core::new()
            .with_plugin(fork_plugin(ForkConfig::default()))
            .build()
            .await
            .unwrap();

        assert_eq!(core.tools().ids(), vec![ToolId::new(TOOL_ID)]);
        assert!(core.workdir_layers().ids().is_empty());
    }

    #[test]
    fn artifact_is_independent_and_serializable() {
        let first = Message::text(Role::User, "one");
        let second = Message::text(Role::Assistant, "two");
        let source = SessionId::new();
        let artifact = ForkArtifact {
            schema_version: SCHEMA_VERSION,
            session_id: SessionId::new(),
            provenance: ForkProvenance {
                source_session_id: source,
                through_message_id: Some(first.id),
            },
            messages: vec![first.clone()],
        };

        assert_ne!(artifact.session_id, source);
        assert_eq!(artifact.messages, vec![first]);
        let encoded = serde_json::to_vec(&artifact).unwrap();
        assert_eq!(
            serde_json::from_slice::<ForkArtifact>(&encoded).unwrap(),
            artifact
        );
        assert_ne!(artifact.messages, vec![second]);
    }

    #[test]
    fn writes_a_complete_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = ForkArtifact {
            schema_version: SCHEMA_VERSION,
            session_id: SessionId::new(),
            provenance: ForkProvenance {
                source_session_id: SessionId::new(),
                through_message_id: None,
            },
            messages: vec![Message::text(Role::User, "stored")],
        };

        let path = write_artifact(directory.path(), &artifact).unwrap();
        let decoded: ForkArtifact = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(decoded, artifact);
    }
}
