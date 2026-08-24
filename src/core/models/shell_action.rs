use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::error::Result;
use super::id::{ProjectId, ShellActionId};
use super::workdir::Workdir;

/// The argument scheme exposed by a shell action.
///
/// The shape is intentionally open-ended so plugins can describe their own
/// command-line arguments without coupling the core to a parser library.
pub type ShellActionScheme = Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShellActionDefinition {
    pub name: String,
    pub description: String,
    pub scheme: ShellActionScheme,
}

impl ShellActionDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        scheme: ShellActionScheme,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            scheme,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellActionInput {
    pub arguments: Vec<String>,
}

impl ShellActionInput {
    pub fn new(arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            arguments: arguments.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellActionInvocation {
    pub name: String,
    pub arguments: Vec<String>,
}

impl ShellActionInvocation {
    pub fn new(
        name: impl Into<String>,
        arguments: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
        }
    }

    pub fn into_input(self) -> ShellActionInput {
        ShellActionInput::new(self.arguments)
    }
}

#[derive(Clone)]
pub struct ShellActionContext {
    pub project_id: ProjectId,
    pub workdir: Arc<dyn Workdir>,
    pub cancellation: CancellationToken,
}

pub type ShellActionOutput = String;

#[async_trait]
pub trait ShellAction: Send + Sync {
    fn id(&self) -> ShellActionId;
    fn definition(&self) -> ShellActionDefinition;
    async fn execute(
        &self,
        input: ShellActionInput,
        context: ShellActionContext,
    ) -> Result<ShellActionOutput>;
}
