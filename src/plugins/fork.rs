use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{
    Command, CommandCompletion, CommandContext, CommandDescriptor, CommandId, CommandInvocation,
    CommandResult, Error, Message, MessageId, Plugin, PluginId, PluginRegistrar, Result, SessionId,
};

const PLUGIN_ID: &str = "builtin.fork";
const COMMAND_ID: &str = "builtin.fork";
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
        registrar.register_command(
            0,
            Arc::new(ForkCommand {
                storage_dir: self.config.storage_dir.clone(),
            }),
        )
    }
}

struct ForkCommand {
    storage_dir: Option<PathBuf>,
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
impl Command for ForkCommand {
    fn id(&self) -> CommandId {
        CommandId::new(COMMAND_ID)
    }

    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "fork".into(),
            description: "Create a self-contained fork of this session's messages.".into(),
            usage: "/fork [message-id]".into(),
        }
    }

    async fn complete(
        &self,
        context: &CommandContext,
        invocation: &CommandInvocation,
    ) -> Result<Vec<CommandCompletion>> {
        let snapshot = context.history().snapshot().await?;
        Ok(snapshot
            .messages
            .iter()
            .map(|message| message.id.to_string())
            .filter(|id| id.starts_with(invocation.arguments.trim()))
            .map(|value| CommandCompletion {
                value,
                description: Some("Fork through this message".into()),
            })
            .collect())
    }

    async fn execute(
        &self,
        invocation: CommandInvocation,
        context: CommandContext,
    ) -> Result<CommandResult> {
        let through_message_id = parse_optional_message_id(&invocation.arguments)?;
        let snapshot = context.history().snapshot().await?;
        let end = match through_message_id {
            Some(id) => snapshot
                .messages
                .iter()
                .position(|message| message.id == id)
                .map(|index| index + 1)
                .ok_or_else(|| {
                    Error::Plugin(format!(
                        "message {id} is not in session {}",
                        context.hook_context.session_id
                    ))
                })?,
            None => snapshot.messages.len(),
        };
        let artifact = ForkArtifact {
            schema_version: SCHEMA_VERSION,
            session_id: SessionId::new(),
            provenance: ForkProvenance {
                source_session_id: context.hook_context.session_id,
                through_message_id,
            },
            messages: snapshot.messages[..end].to_vec(),
        };

        let content = if let Some(storage_dir) = &self.storage_dir {
            let path = write_artifact(storage_dir, &artifact)?;
            let content = serde_json::to_string(&StoredFork {
                session_id: artifact.session_id,
                provenance: &artifact.provenance,
                artifact_path: &path,
            })
            .map_err(|error| Error::Plugin(format!("could not encode fork result: {error}")))?;
            content
        } else {
            let content = serde_json::to_string(&artifact)
                .map_err(|error| Error::Plugin(format!("could not encode fork: {error}")))?;
            content
        };

        Ok(CommandResult { content })
    }
}

fn parse_optional_message_id(arguments: &str) -> Result<Option<MessageId>> {
    let argument = arguments.trim();
    if argument.is_empty() {
        return Ok(None);
    }
    if argument.split_whitespace().count() != 1 {
        return Err(Error::Plugin("usage: /fork [message-id]".into()));
    }
    serde_json::from_value(serde_json::Value::String(argument.into()))
        .map(Some)
        .map_err(|_| Error::Plugin(format!("invalid message id: {argument}")))
}

fn write_artifact(storage_dir: &Path, artifact: &ForkArtifact) -> Result<PathBuf> {
    fs::create_dir_all(storage_dir).map_err(|error| {
        Error::Plugin(format!(
            "could not create fork storage {}: {error}",
            storage_dir.display()
        ))
    })?;
    let path = storage_dir.join(format!("{}.json", artifact.session_id));
    let encoded = serde_json::to_vec_pretty(artifact)
        .map_err(|error| Error::Plugin(format!("could not encode fork artifact: {error}")))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            Error::Plugin(format!(
                "could not create fork artifact {}: {error}",
                path.display()
            ))
        })?;
    file.write_all(&encoded).map_err(|error| {
        Error::Plugin(format!(
            "could not write fork artifact {}: {error}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        Error::Plugin(format!(
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
    async fn plugin_registers_only_its_fork_command() {
        let core = Core::new()
            .with_plugin(fork_plugin(ForkConfig::default()))
            .build()
            .await
            .unwrap();

        assert_eq!(core.commands().ids(), vec![CommandId::new(COMMAND_ID)]);
        assert_eq!(core.commands().descriptors()[0].usage, "/fork [message-id]");
        assert!(core.tools().ids().is_empty());
        assert!(core.workdir_layers().ids().is_empty());

        let omitted = Core::new().build().await.unwrap();
        assert!(omitted.commands().ids().is_empty());
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
