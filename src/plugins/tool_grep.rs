use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{
    Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext, ToolDefinition, ToolId,
    ToolOutput,
};

const GREP_TOOL_ID: &str = "builtin.grep";
const GREP_TOOL_NAME: &str = "grep";
const GREP_PLUGIN_ID: &str = "builtin.tool-grep";

const DEFAULT_MAX_RESULTS: usize = 200;
const HARD_MAX_RESULTS: usize = 2_000;
const DEFAULT_MAX_FILE_BYTES: usize = 1024 * 1024;
const HARD_MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 200_000;
const HARD_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_VISITED_FILES: usize = 20_000;

#[derive(Clone, Debug, Default)]
struct GrepTool;

struct GrepPlugin;

pub fn grep_plugin() -> Arc<dyn Plugin> {
    Arc::new(GrepPlugin)
}

#[async_trait]
impl Plugin for GrepPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(GREP_PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_tool(0, Arc::new(GrepTool))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepInput {
    query: String,
    #[serde(default = "default_path")]
    path: PathBuf,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default = "default_max_file_bytes")]
    max_file_bytes: usize,
    #[serde(default = "default_max_output_bytes")]
    max_output_bytes: usize,
}

#[derive(Debug, Serialize)]
struct GrepMatch {
    path: String,
    line: usize,
    text: String,
}

#[derive(Debug, Serialize)]
struct GrepResponse {
    matches: Vec<GrepMatch>,
    truncated: bool,
}

fn default_path() -> PathBuf {
    PathBuf::from(".")
}

fn default_max_results() -> usize {
    DEFAULT_MAX_RESULTS
}

fn default_max_file_bytes() -> usize {
    DEFAULT_MAX_FILE_BYTES
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

fn validate_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Tool(format!(
            "grep path must be project-relative: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_skipped_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "target")
    )
}

#[async_trait]
impl Tool for GrepTool {
    fn id(&self) -> ToolId {
        ToolId::new(GREP_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: GREP_TOOL_NAME.into(),
            description: "Recursively search project text files for a literal string.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "path": { "type": "string", "default": "." },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": HARD_MAX_RESULTS, "default": DEFAULT_MAX_RESULTS },
                    "max_file_bytes": { "type": "integer", "minimum": 1, "maximum": HARD_MAX_FILE_BYTES, "default": DEFAULT_MAX_FILE_BYTES },
                    "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": HARD_MAX_OUTPUT_BYTES, "default": DEFAULT_MAX_OUTPUT_BYTES }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let input: GrepInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid grep input: {error}")))?;
        if input.query.is_empty() {
            return Err(Error::Tool("grep query may not be empty".into()));
        }
        if input.max_results == 0 || input.max_results > HARD_MAX_RESULTS {
            return Err(Error::Tool(format!(
                "max_results must be between 1 and {HARD_MAX_RESULTS}"
            )));
        }
        if input.max_file_bytes == 0 || input.max_file_bytes > HARD_MAX_FILE_BYTES {
            return Err(Error::Tool(format!(
                "max_file_bytes must be between 1 and {HARD_MAX_FILE_BYTES}"
            )));
        }
        if input.max_output_bytes < 1024 || input.max_output_bytes > HARD_MAX_OUTPUT_BYTES {
            return Err(Error::Tool(format!(
                "max_output_bytes must be between 1024 and {HARD_MAX_OUTPUT_BYTES}"
            )));
        }
        validate_path(&input.path)?;

        let root = fs::canonicalize(context.workdir.root())
            .map_err(|error| Error::Tool(format!("could not resolve workdir root: {error}")))?;
        let start = fs::canonicalize(root.join(&input.path)).map_err(|error| {
            Error::Tool(format!(
                "could not resolve grep path {}: {error}",
                input.path.display()
            ))
        })?;
        if !start.starts_with(&root) {
            return Err(Error::Tool("grep path escapes the workdir".into()));
        }
        let cancellation = context.cancellation;
        let query = input.query;
        let max_results = input.max_results;
        let max_file_bytes = input.max_file_bytes;
        let max_output_bytes = input.max_output_bytes;
        let mut response = tokio::task::spawn_blocking(move || -> Result<GrepResponse> {
            let mut pending = vec![start];
            let mut matches = Vec::new();
            let mut visited = 0_usize;
            let mut truncated = false;

            while let Some(path) = pending.pop() {
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    Error::Tool(format!("could not inspect {}: {error}", path.display()))
                })?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.is_dir() {
                    if path != root && is_skipped_directory(&path) {
                        continue;
                    }
                    let mut entries = fs::read_dir(&path)
                        .map_err(|error| {
                            Error::Tool(format!("could not list {}: {error}", path.display()))
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|error| {
                            Error::Tool(format!("could not list {}: {error}", path.display()))
                        })?;
                    entries.sort_by_key(|entry| entry.file_name());
                    pending.extend(entries.into_iter().rev().map(|entry| entry.path()));
                    continue;
                }
                if !metadata.is_file() {
                    continue;
                }
                visited += 1;
                if visited > MAX_VISITED_FILES {
                    truncated = true;
                    break;
                }
                if metadata.len() > max_file_bytes as u64 {
                    continue;
                }
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                let bytes = match fs::File::open(&path).and_then(|file| {
                    file.take(max_file_bytes as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    Ok(bytes)
                }) {
                    Ok(bytes) if bytes.len() <= max_file_bytes => bytes,
                    Err(_) => continue,
                    Ok(_) => continue,
                };
                if bytes.contains(&0) {
                    continue;
                }
                let text = match std::str::from_utf8(&bytes) {
                    Ok(text) => text,
                    Err(_) => continue,
                };
                for (index, line) in text.lines().enumerate() {
                    if line.contains(&query) {
                        matches.push(GrepMatch {
                            path: path
                                .strip_prefix(&root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .into_owned(),
                            line: index + 1,
                            text: line.to_owned(),
                        });
                        if matches.len() == max_results {
                            truncated = true;
                            break;
                        }
                    }
                }
                if truncated {
                    break;
                }
            }
            Ok(GrepResponse { matches, truncated })
        })
        .await
        .map_err(|error| Error::Tool(format!("grep task failed: {error}")))??;

        let content = loop {
            let content = serde_json::to_string(&response)
                .map_err(|error| Error::Tool(format!("could not encode grep output: {error}")))?;
            if content.len() <= max_output_bytes {
                break content;
            }
            response.truncated = true;
            match response.matches.last_mut() {
                Some(last) if !last.text.is_empty() => {
                    let midpoint = last.text.len() / 2;
                    let boundary = last
                        .text
                        .char_indices()
                        .map(|(index, _)| index)
                        .take_while(|index| *index <= midpoint)
                        .last()
                        .unwrap_or(0);
                    last.text.truncate(boundary);
                }
                Some(_) => {
                    response.matches.pop();
                }
                None => break content,
            }
        };

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}
