use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use std::sync::Arc;

use super::{CommandId, HookContext, Result, Workdir};

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommandDescriptor {
    pub name: String,
    pub description: String,
    pub usage: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommandInvocation {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommandCompletion {
    pub value: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommandResult {
    pub content: String,
}

#[derive(Clone)]
pub struct CommandContext {
    pub hook_context: HookContext,
    pub workdir: Arc<dyn Workdir>,
    pub cancellation: CancellationToken,
}

impl CommandContext {
    pub fn history(&self) -> super::SessionHistory {
        self.hook_context.history()
    }
}

#[async_trait]
pub trait Command: Send + Sync {
    fn id(&self) -> CommandId;
    fn descriptor(&self) -> CommandDescriptor;

    async fn complete(
        &self,
        _context: &CommandContext,
        _invocation: &CommandInvocation,
    ) -> Result<Vec<CommandCompletion>> {
        Ok(Vec::new())
    }

    async fn execute(
        &self,
        invocation: CommandInvocation,
        context: CommandContext,
    ) -> Result<CommandResult>;
}

/// Parses `/name arguments` without slicing at a non-character boundary.
pub fn parse_command_invocation(input: &str) -> Option<CommandInvocation> {
    let input = input.strip_prefix('/')?;
    let split = input
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map(|(index, _)| index);
    let (name, arguments) = match split {
        Some(index) => (&input[..index], input[index..].trim_start()),
        None => (input, ""),
    };
    if name.is_empty() {
        return None;
    }
    Some(CommandInvocation {
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    })
}

pub fn command_completion_prefix(input: &str, cursor: usize) -> Option<&str> {
    if cursor > input.len() || !input.is_char_boundary(cursor) {
        return None;
    }
    input[..cursor].strip_prefix('/')
}
