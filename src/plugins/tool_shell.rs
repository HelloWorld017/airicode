use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    models::{
        CommandSpec, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput,
    },
    registry::PluginRegistryScope,
};

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
            description: "Execute raw shell command text in the current workdir. Pass a string, not a JSON object. The command runs through the Workdir abstraction so its root, worktree, and sandbox layers are respected. The result includes the exit status and captured stdout/stderr; a non-zero exit status is a tool failure, and cancellation stops the process when supported.".into(),
            input_schema: crate::utils::schema::json_schema::<String>(),
        }
    }
    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let command = input
            .as_str()
            .ok_or_else(|| Error::Tool("shell input must be text".into()))?;
        #[cfg(windows)]
        let spec = CommandSpec {
            program: "cmd".into(),
            args: vec!["/C".into(), command.into()],
            cwd: None,
            env: Default::default(),
            max_output_bytes: self.max_output_bytes,
        };
        #[cfg(not(windows))]
        let spec = CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), command.into()],
            cwd: None,
            env: Default::default(),
            max_output_bytes: self.max_output_bytes,
        };
        let result = context
            .workdir
            .execute(spec, context.cancellation.clone())
            .await?;
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
        if result.status == Some(0) {
            Ok(ToolOutput::Success { content })
        } else {
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
