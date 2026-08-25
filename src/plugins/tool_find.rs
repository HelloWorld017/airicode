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
struct FindInputSchema {
    name: Option<String>,
    pattern: Option<String>,
    path: Option<String>,
    max_results: Option<usize>,
}

pub struct ToolFind {
    id: ToolId,
    max_output_bytes: usize,
    max_results: usize,
}

impl ToolFind {
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

impl Default for ToolFind {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolFind {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "find".into(),
            description: "Find files by exact filename with fd and return their workdir-relative paths. Pass the filename as `name` (or `pattern`); `path` optionally limits the search root and `max_results` limits output.".into(),
            input: ToolInputDefinition::JsonSchema(
                crate::utils::schema::json_schema::<FindInputSchema>(),
            ),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Json(input) = input else {
            return Err(Error::Tool("find input must be an object".into()));
        };
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("find input must be an object".into()))?;
        let name = object
            .get("name")
            .or_else(|| object.get("pattern"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("find requires name".into()))?;
        if name.is_empty() {
            return Ok(ToolOutput::Failure {
                content: "find name cannot be empty".into(),
            });
        }
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .unwrap_or(".");
        let max_results = object
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(self.max_results)
            .min(self.max_results);

        let result = context
            .workdir
            .execute(
                CommandSpec {
                    program: "fd".into(),
                    args: vec![
                        "--type".into(),
                        "f".into(),
                        "--hidden".into(),
                        "--exclude".into(),
                        ".git".into(),
                        "--glob".into(),
                        "--max-results".into(),
                        max_results.to_string(),
                        "--".into(),
                        name.into(),
                        path.into(),
                    ],
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
                content: "no files found".into(),
            });
        }
        if result.status != Some(0) {
            return Ok(ToolOutput::Failure {
                content: if result.stderr.is_empty() {
                    format!("find exited {:?}", result.status)
                } else {
                    result.stderr
                },
            });
        }

        let mut lines = result
            .stdout
            .lines()
            .take(max_results)
            .map(|line| line.strip_prefix("./").unwrap_or(line))
            .collect::<Vec<_>>();
        if result.truncated || result.stdout.lines().count() > max_results {
            lines.push("[results truncated]");
        }
        Ok(ToolOutput::Success {
            content: lines.join("\n"),
        })
    }
}

pub struct ToolFindPlugin {
    id: PluginId,
    tool: Arc<ToolFind>,
}

impl ToolFindPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolFind::new()),
        }
    }

    pub fn tool(&self) -> Arc<ToolFind> {
        self.tool.clone()
    }
}

#[async_trait]
impl Plugin for ToolFindPlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_find"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
