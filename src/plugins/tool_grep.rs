use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    models::{
        CommandSpec, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
        ToolInputDefinition, ToolOutput,
    },
    registry::PluginRegistryScope,
};

#[allow(dead_code)]
#[derive(JsonSchema)]
struct GrepInputSchema {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    max_results: Option<usize>,
}

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
            description: "Search files visible through the current workdir using a regular-expression pattern. `path` optionally limits the search scope and `glob` optionally filters filenames. Results include file and line references and are size-limited.".into(),
            input: ToolInputDefinition::JsonSchema(
                crate::utils::schema::json_schema::<GrepInputSchema>(),
            ),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Json(input) = input else {
            return Err(Error::Tool("grep input must be an object".into()));
        };
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
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolGrep::new()),
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
