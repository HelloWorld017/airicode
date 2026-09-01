use std::sync::Arc;

use crate::core::{
    error::{Error, Result},
    models::{
        CommandSpec, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
        ToolInputDefinition, ToolOutput,
    },
    registry::PluginRegistryScope,
};
use crate::{core::models::NoteContent, plugins::add_tool_note};
use async_trait::async_trait;

pub struct ToolShell {
    id: ToolId,
    max_output_bytes: usize,
}
impl ToolShell {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_output_bytes: 256 * 1024,
        }
    }
    pub fn with_max_output(mut self, max: usize) -> Self {
        self.max_output_bytes = max;
        self
    }
}
impl Default for ToolShell {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolShell {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".into(),
            description: "Execute raw shell command text in the current workdir. Pass a string, not a JSON object. The command runs through the Workdir abstraction so its root, worktree, and sandbox layers are respected. The result includes the exit status and captured stdout/stderr; a non-zero exit status is a tool failure.".into(),
            input: ToolInputDefinition::Text,
        }
    }
    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Text(command) = input else {
            return Err(Error::Tool("shell input must be text".into()));
        };
        #[cfg(windows)]
        let spec = CommandSpec {
            program: "cmd".into(),
            args: vec!["/C".into(), command.clone()],
            cwd: None,
            env: Default::default(),
            max_output_bytes: self.max_output_bytes,
        };
        #[cfg(not(windows))]
        let spec = CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), command.clone()],
            cwd: None,
            env: Default::default(),
            max_output_bytes: self.max_output_bytes,
        };
        let result = match context
            .workdir
            .execute(spec, context.cancellation.clone())
            .await
        {
            Ok(result) => result,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::Workdir(message)) => {
                let output = ToolOutput::Failure {
                    content: message.clone(),
                };
                add_tool_note(
                    &context,
                    NoteContent::Alert {
                        content: format!("# {command}\n\n{message}"),
                    },
                    "shell",
                )
                .await?;
                return Ok(output);
            }
            Err(error) => return Err(error),
        };
        let mut content = String::new();
        if !result.stdout.is_empty() {
            content.push_str(&result.stdout);
        }
        if !result.stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&result.stderr);
        }
        if result.truncated {
            content.push_str("\n[output truncated]");
        }
        let status = result
            .status
            .map_or_else(|| "signal".into(), |status| status.to_string());
        let content = format!("exit {status}\n{content}");
        let note_content = format!("# {command}\n\n$ {command}\n\n{content}");
        if result.status == Some(0) {
            add_tool_note(
                &context,
                NoteContent::Info {
                    content: note_content,
                },
                "shell",
            )
            .await?;
            Ok(ToolOutput::Success { content })
        } else {
            add_tool_note(
                &context,
                NoteContent::Alert {
                    content: note_content,
                },
                "shell",
            )
            .await?;
            Ok(ToolOutput::Failure { content })
        }
    }
}

pub struct ToolShellPlugin {
    id: PluginId,
    tool: Arc<ToolShell>,
}
impl ToolShellPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolShell::new()),
        }
    }
    pub fn tool(&self) -> Arc<ToolShell> {
        self.tool.clone()
    }
}

#[async_trait]
impl Plugin for ToolShellPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_shell"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
