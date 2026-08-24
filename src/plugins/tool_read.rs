use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    core::{
        Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext, ToolDefinition,
        ToolId, ToolOutput,
    },
    hashline,
};

const READ_TOOL_ID: &str = "builtin.read";
const READ_TOOL_NAME: &str = "read";
const READ_PLUGIN_ID: &str = "builtin.tool-read";
const MAX_LINES: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 200_000;

#[derive(Clone, Debug, Default)]
struct ReadTool;

struct ReadPlugin;

pub fn read_plugin() -> Arc<dyn Plugin> {
    Arc::new(ReadPlugin)
}

#[async_trait]
impl Plugin for ReadPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(READ_PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_tool(0, Arc::new(ReadTool))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: PathBuf,
    range: Option<ReadRange>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRange {
    start: usize,
    end: usize,
}

fn validate_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Tool(format!(
            "read path must be project-relative: {}",
            path.display()
        )));
    }
    Ok(())
}

#[async_trait]
impl Tool for ReadTool {
    fn id(&self) -> ToolId {
        ToolId::new(READ_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: READ_TOOL_NAME.into(),
            description: "Read project text as hashed lines for stale-edit-safe patching. The short hashes are stale-edit markers, not security hashes.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "range": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["start", "end"],
                        "properties": {
                            "start": { "type": "integer", "minimum": 1 },
                            "end": { "type": "integer", "minimum": 1 }
                        }
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let input: ReadInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid read input: {error}")))?;
        validate_path(&input.path)?;
        let bytes = context.workdir.read(&input.path).await.map_err(|error| {
            Error::Tool(format!("cannot read {}: {error}", input.path.display()))
        })?;
        hashline::validate_text(&bytes, &input.path)?;
        let lines = hashline::line_ranges(&bytes);

        let explicit_range = input.range.is_some();
        let (start, end) = match input.range {
            Some(range) if range.start == 0 || range.end < range.start => {
                return Err(Error::Tool(
                    "read range must be 1-based with start <= end".into(),
                ))
            }
            Some(range) if range.start > lines.len() => {
                return Err(Error::Tool(format!(
                    "read range starts beyond end of file ({} lines)",
                    lines.len()
                )))
            }
            Some(range) => (range.start, range.end.min(lines.len())),
            None => (1, lines.len()),
        };
        let count = end.saturating_sub(start).saturating_add(1);
        if count > MAX_LINES {
            return Err(Error::Tool(if explicit_range {
                format!("read range exceeds the {MAX_LINES}-line limit; request a smaller range")
            } else {
                format!("file exceeds the {MAX_LINES}-line output limit; use range")
            }));
        }

        let mut output = String::new();
        if !lines.is_empty() {
            for line_number in start..=end {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&hashline::record(
                    line_number,
                    &bytes[lines[line_number - 1].clone()],
                ));
                if output.len() > MAX_OUTPUT_BYTES {
                    return Err(Error::Tool(if explicit_range {
                        "read range exceeds the 200 KB output limit; request a smaller range".into()
                    } else {
                        "file exceeds the 200 KB output limit; use range".into()
                    }));
                }
            }
        }

        Ok(ToolOutput {
            content: output,
            is_error: false,
        })
    }
}
