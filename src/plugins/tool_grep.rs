use std::{collections::HashMap, path::PathBuf, sync::Arc};

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
use crate::utils::hashline;

#[allow(dead_code)]
#[derive(JsonSchema)]
struct GrepInputSchema {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    max_results: Option<usize>,
}

struct GrepMatch {
    path: PathBuf,
    line: usize,
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
        let mut args = vec![
            "--json".into(),
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
        let matches = parse_matches(&result.stdout);
        if result.status == Some(1) && matches.is_empty() {
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

        let mut rendered_files = HashMap::new();
        let mut lines = Vec::new();
        for grep_match in matches.iter().take(max_results) {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !rendered_files.contains_key(&grep_match.path) {
                let bytes = match context.workdir.read(&grep_match.path).await {
                    Ok(bytes) => bytes,
                    Err(Error::Workdir(message)) => {
                        return Ok(ToolOutput::Failure { content: message });
                    }
                    Err(error) => return Err(error),
                };
                if bytes.contains(&0) {
                    return Ok(ToolOutput::Failure {
                        content: format!(
                            "cannot create hashline for binary/NUL-containing input: {}",
                            grep_match.path.display()
                        ),
                    });
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    Error::Tool(format!(
                        "cannot create hashline for non-UTF-8 input: {}",
                        grep_match.path.display()
                    ))
                })?;
                rendered_files.insert(grep_match.path.clone(), hashline::render(text));
            }
            let file_lines = rendered_files
                .get(&grep_match.path)
                .expect("rendered grep file should be cached");
            let Some(line) = file_lines.iter().find(|line| line.line == grep_match.line) else {
                return Ok(ToolOutput::Failure {
                    content: format!(
                        "grep result became stale while creating hashline: {}:{}",
                        grep_match.path.display(),
                        grep_match.line
                    ),
                });
            };
            lines.push(format!(
                "{}:{}:{}|{}",
                grep_match.path.display(),
                line.line,
                line.tag,
                line.text
            ));
        }
        if result.truncated || matches.len() > max_results {
            lines.push("[results truncated]".into());
        }
        Ok(ToolOutput::Success {
            content: lines.join("\n"),
        })
    }
}

fn parse_matches(stdout: &str) -> Vec<GrepMatch> {
    stdout
        .lines()
        .filter_map(|line| {
            let event = serde_json::from_str::<Value>(line).ok()?;
            if event.get("type").and_then(Value::as_str) != Some("match") {
                return None;
            }
            let data = event.get("data")?;
            let path = data
                .get("path")
                .and_then(|path| path.get("text"))
                .and_then(Value::as_str)?;
            let line = data
                .get("line_number")
                .and_then(Value::as_u64)
                .and_then(|line| usize::try_from(line).ok())?;
            Some(GrepMatch {
                path: PathBuf::from(path),
                line,
            })
        })
        .collect()
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
