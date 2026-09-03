use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
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
use crate::utils::{hashline, note::add_output_note, schema::json_schema};

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
    text: String,
}

pub struct ToolGrep {
    id: ToolId,
    max_output_bytes: usize,
    max_results: usize,
    enable_hashline: bool,
}

impl ToolGrep {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_output_bytes: 128 * 1024,
            max_results: 500,
            enable_hashline: false,
        }
    }

    pub fn with_limits(mut self, max_results: usize, max_output_bytes: usize) -> Self {
        self.max_results = max_results;
        self.max_output_bytes = max_output_bytes;
        self
    }
    pub fn with_hashline(mut self, enable_hashline: bool) -> Self {
        self.enable_hashline = enable_hashline;
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
            description: if self.enable_hashline {
                "Search files visible through the current workdir using a regular-expression pattern. `path` optionally limits the search scope and `glob` optionally filters filenames. Results use `path:line:hash|content`, where the hashline anchor is compatible with patch_hashline, and are size-limited."
            } else {
                "Search files visible through the current workdir using a regular-expression pattern. `path` optionally limits the search scope and `glob` optionally filters filenames. Results use `path:line|content` and are size-limited."
            }
            .into(),
            input: ToolInputDefinition::new(json_schema::<GrepInputSchema>()),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("grep input must be an object".into()))?;
        let pattern = object
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("grep requires pattern".into()))?;
        if pattern.is_empty() {
            let output = ToolOutput::Failure {
                content: "grep pattern cannot be empty".into(),
            };
            add_output_note(&context, "grep", "Search failed", &output).await?;
            return Ok(output);
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

        let mut rendered_files = HashMap::new();
        let mut lines = Vec::new();
        for grep_match in matches.iter().take(max_results) {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !self.enable_hashline {
                lines.push(format!(
                    "{}:{}|{}",
                    grep_match.path.display(),
                    grep_match.line,
                    grep_match.text.trim_end_matches(['\r', '\n'])
                ));
                continue;
            }
            if !rendered_files.contains_key(&grep_match.path) {
                let bytes = match context.workdir.read(&grep_match.path).await {
                    Ok(bytes) => bytes,
                    Err(Error::Workdir(message)) => {
                        let output = ToolOutput::Failure { content: message };
                        add_output_note(&context, "grep", "Search failed", &output).await?;
                        return Ok(output);
                    }
                    Err(error) => return Err(error),
                };
                if bytes.contains(&0) {
                    let output = ToolOutput::Failure {
                        content: format!(
                            "cannot create hashline for binary/NUL-containing input: {}",
                            grep_match.path.display()
                        ),
                    };
                    add_output_note(&context, "grep", "Search failed", &output).await?;
                    return Ok(output);
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
                let output = ToolOutput::Failure {
                    content: format!(
                        "grep result became stale while creating hashline: {}:{}",
                        grep_match.path.display(),
                        grep_match.line
                    ),
                };
                add_output_note(&context, "grep", "Search failed", &output).await?;
                return Ok(output);
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
            let text = data
                .get("lines")
                .and_then(|lines| lines.get("text"))
                .and_then(Value::as_str)?
                .to_string();
            Some(GrepMatch {
                path: PathBuf::from(path),
                line,
                text,
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
                Arc::new(ToolGrep::new().with_hashline(context.config.tool.enable_hashline)),
                0,
            )
            .map(|_| ())
    }
}
