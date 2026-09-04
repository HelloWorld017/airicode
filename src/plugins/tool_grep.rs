use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    hooks::{ConfigReadContext, ConfigReadHook},
    models::{
        CommandSpec, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
        ToolInputDefinition, ToolOutput,
    },
    registry::PluginRegistryScope,
};
use crate::utils::{note::add_output_note, schema::json_schema};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GrepInput {
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
            description: "Search files visible through the current workdir using a regular-expression pattern. `path` optionally limits the search scope and `glob` optionally filters filenames. Results use `<path>:<line anchor>|<content>` and are size-limited."
            .into(),
            input: ToolInputDefinition::new(json_schema::<GrepInput>()),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let input: GrepInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid grep input: {error}")))?;
        if input.pattern.is_empty() {
            let output = ToolOutput::Failure {
                content: "grep pattern cannot be empty".into(),
            };
            add_output_note(&context, "grep", "Search failed", &output).await?;
            return Ok(output);
        }
        let pattern = input.pattern;
        let path = input
            .path
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| ".".into());
        let max_results = input
            .max_results
            .unwrap_or(self.max_results)
            .min(self.max_results);
        let mut args = vec![
            "--json".into(),
            "--color=never".into(),
            "--hidden".into(),
            "--glob".into(),
            "!.git".into(),
        ];
        if let Some(glob) = input.glob {
            args.extend(["--glob".into(), glob]);
        }
        args.extend(["--".into(), pattern.clone(), path.clone()]);
        let workdir = context.operations.workdir()?;
        let result = workdir
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
            Err(Error::Workdir(message)) => {
                let output = ToolOutput::Failure { content: message };
                add_output_note(&context, "grep", "Search failed", &output).await?;
                return Ok(output);
            }
            Err(error) => return Err(error),
        };
        let matches = parse_matches(&result.stdout);
        if result.status == Some(1) && matches.is_empty() {
            let output = ToolOutput::Failure {
                content: "no matches".into(),
            };
            add_output_note(
                &context,
                "grep",
                format!("Searched \"{pattern}\" in {path} - no matches"),
                &output,
            )
            .await?;
            return Ok(output);
        }
        if result.status != Some(0) {
            let output = ToolOutput::Failure {
                content: if result.stderr.is_empty() {
                    format!("grep exited {:?}", result.status)
                } else {
                    result.stderr
                },
            };
            add_output_note(
                &context,
                "grep",
                format!("Search failed for \"{pattern}\" in {path}"),
                &output,
            )
            .await?;
            return Ok(output);
        }

        let selected = matches.iter().take(max_results).collect::<Vec<_>>();
        let mut paths = Vec::new();
        let mut ranges = HashMap::new();
        for grep_match in &selected {
            let range = ranges
                .entry(grep_match.path.clone())
                .or_insert_with(|| (grep_match.line, grep_match.line));
            range.0 = range.0.min(grep_match.line);
            range.1 = range.1.max(grep_match.line);
            if !paths.contains(&grep_match.path) {
                paths.push(grep_match.path.clone());
            }
        }
        let mut rendered_files = HashMap::new();
        for path in paths {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let (start_line, end_line) = ranges[&path];
            let file_context = match context
                .operations
                .build_file_context(crate::core::models::BuildFileContextRequest {
                    path: path.clone(),
                    start_line: Some(start_line),
                    end_line: Some(end_line),
                    max_lines: None,
                    max_bytes: None,
                })
                .await
            {
                Ok(file_context) => file_context,
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(error) => {
                    let output = ToolOutput::Failure {
                        content: error.to_string(),
                    };
                    add_output_note(&context, "grep", "Search failed", &output).await?;
                    return Ok(output);
                }
            };
            rendered_files.insert(
                path,
                file_context
                    .lines
                    .into_iter()
                    .map(|line| (line.line_number, line))
                    .collect::<HashMap<_, _>>(),
            );
        }
        let mut lines = Vec::new();
        for grep_match in selected {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let file_lines = rendered_files
                .get(&grep_match.path)
                .expect("rendered grep file should be cached");
            let Some(line) = file_lines.get(&grep_match.line) else {
                let output = ToolOutput::Failure {
                    content: format!(
                        "grep result became stale while reading file context: {}:{}",
                        grep_match.path.display(),
                        grep_match.line
                    ),
                };
                add_output_note(&context, "grep", "Search failed", &output).await?;
                return Ok(output);
            };
            lines.push(format!(
                "{}:{}|{}",
                grep_match.path.display(),
                line.display_prefix,
                line.text
            ));
        }
        if result.truncated || matches.len() > max_results {
            lines.push("[results truncated]".into());
        }
        let output = ToolOutput::Success {
            content: lines.join("\n"),
        };
        add_output_note(
            &context,
            "grep",
            format!(
                "Searched \"{pattern}\" in {path} - {} matches",
                matches.len()
            ),
            &output,
        )
        .await?;
        Ok(output)
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
}
impl ToolGrepPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
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
        let hook: Arc<dyn ConfigReadHook> = self;
        registry.register_hook(hook)
    }
}

#[async_trait]
impl ConfigReadHook for ToolGrepPlugin {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()> {
        context
            .registry
            .register_tool(
                Arc::new(ToolGrep::new()),
                0,
            )
            .map(|_| ())
    }
}
