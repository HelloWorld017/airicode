use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    models::{
        CommandSpec, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput,
    },
    registry::PluginRegistryScope,
};

pub struct ToolGrep {
    id: ToolId,
    max_output_bytes: usize,
    max_results: usize,
}

impl ToolGrep {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_output_bytes: 128 * 1024,
            max_results: 500,
        }
    }

    pub fn with_limits(mut self, max_results: usize, max_output_bytes: usize) -> Self {
        self.max_results = max_results;
        self.max_output_bytes = max_output_bytes;
        self
    }
}

impl Default for ToolGrep {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolGrep {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: "Search files visible through the current workdir.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "max_results": { "type": "integer", "minimum": 1 }
                }
            }),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("grep input must be an object".into()))?;
        let pattern = object
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("grep requires pattern".into()))?;
        if pattern.is_empty() {
            return Ok(ToolOutput::Failure {
                content: "grep pattern cannot be empty".into(),
            });
        }
        let path = object.get("path").and_then(Value::as_str).unwrap_or(".");
        let max_results = object
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(self.max_results)
            .min(self.max_results);
        let mut args = vec![
            "--line-number".into(),
            "--no-heading".into(),
            "--color=never".into(),
            "--hidden".into(),
            "--glob".into(),
            "!.git".into(),
        ];
        if let Some(glob) = object.get("glob").and_then(Value::as_str) {
            args.extend(["--glob".into(), glob.into()]);
        }
        args.extend(["--".into(), pattern.into(), path.into()]);
        let result = context
            .workdir
            .execute(
                CommandSpec {
                    program: "rg".into(),
                    args,
                    cwd: None::<PathBuf>,
                    env: Default::default(),
                    max_output_bytes: self.max_output_bytes,
                },
                context.cancellation.clone(),
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::Workdir(message)) => return Ok(ToolOutput::Failure { content: message }),
            Err(error) => return Err(error),
        };
        if result.status == Some(1) && result.stdout.is_empty() {
            return Ok(ToolOutput::Failure {
                content: "no matches".into(),
            });
        }
        if result.status != Some(0) {
            return Ok(ToolOutput::Failure {
                content: if result.stderr.is_empty() {
                    format!("grep exited {:?}", result.status)
                } else {
                    result.stderr
                },
            });
        }
        let mut lines = result.stdout.lines().take(max_results).collect::<Vec<_>>();
        if result.truncated || result.stdout.lines().count() > max_results {
            lines.push("[results truncated]");
        }
        Ok(ToolOutput::Success {
            content: lines.join("\n"),
        })
    }
}

pub struct ToolGrepPlugin {
    id: PluginId,
    tool: Arc<ToolGrep>,
}
impl ToolGrepPlugin {
    pub fn new(tool: Arc<ToolGrep>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
        }
    }
}

#[async_trait]
impl Plugin for ToolGrepPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_grep"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
