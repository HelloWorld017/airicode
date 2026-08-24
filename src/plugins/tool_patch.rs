use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        models::{
            NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::hashline::{self, Anchor},
};

#[derive(Clone, Debug)]
struct Edit {
    start: Anchor,
    end: Anchor,
    replacement: String,
}

pub struct ToolPatch {
    id: ToolId,
    max_bytes: usize,
}

impl ToolPatch {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_bytes: 1024 * 1024,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Default for ToolPatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolPatch {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "patch".into(),
            description: "Apply hashline-anchored edits after revalidating the current file."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string" },
                    "start": { "type": "string", "description": "line:tag anchor" },
                    "end": { "type": "string", "description": "line:tag anchor" },
                    "replacement": { "type": "string" },
                    "edits": { "type": "array" }
                }
            }),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("patch input must be an object".into()))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("patch requires path".into()))?;
        let bytes = match context.workdir.read(Path::new(path)).await {
            Ok(bytes) => bytes,
            Err(Error::Workdir(message)) => return Ok(ToolOutput::Failure { content: message }),
            Err(error) => return Err(error),
        };
        if bytes.len() > self.max_bytes {
            return Ok(ToolOutput::Failure {
                content: format!("file exceeds patch limit of {} bytes", self.max_bytes),
            });
        }
        if bytes.contains(&0) {
            return Ok(ToolOutput::Failure {
                content: "cannot patch binary/NUL-containing input".into(),
            });
        }
        let old = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Tool("cannot patch non-UTF-8 input".into()))?
            .to_string();
        let edits = match parse_edits(object) {
            Ok(edits) => edits,
            Err(message) => return Ok(ToolOutput::Failure { content: message }),
        };
        if edits.is_empty() {
            return Ok(ToolOutput::Failure {
                content: "patch requires at least one edit".into(),
            });
        }
        for edit in &edits {
            if !hashline::verify_anchor(&old, &edit.start)
                || !hashline::verify_anchor(&old, &edit.end)
                || edit.start.line > edit.end.line
            {
                return Ok(ToolOutput::Failure {
                    content: format!(
                        "stale patch: file changed since read ({}:{}-{}:{})",
                        edit.start.line, edit.start.tag, edit.end.line, edit.end.tag
                    ),
                });
            }
        }
        let mut sorted = edits.clone();
        sorted.sort_by_key(|edit| std::cmp::Reverse(edit.start.line));
        for pair in sorted.windows(2) {
            if pair[0].start.line <= pair[1].end.line {
                return Ok(ToolOutput::Failure {
                    content: "patch edits overlap".into(),
                });
            }
        }
        let mut updated = old.clone();
        for edit in sorted {
            updated = hashline::replace_lines(
                &updated,
                edit.start.line,
                edit.end.line,
                &edit.replacement,
            )
            .ok_or_else(|| Error::Tool("patch line range is out of bounds".into()))?;
        }
        if updated.len() > self.max_bytes {
            return Ok(ToolOutput::Failure {
                content: format!("patched file exceeds limit of {} bytes", self.max_bytes),
            });
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        context
            .workdir
            .write(Path::new(path), updated.as_bytes())
            .await?;
        let diff = unified_diff(path, &old, &updated);
        let (added, removed) = diff_stats(&diff);
        context
            .operations
            .add_note(
                NoteContent::Diff {
                    file: path.to_string(),
                    content: diff,
                },
                [("tool".into(), Value::String("patch".into()))],
            )
            .await?;
        Ok(ToolOutput::Success {
            content: format!("Updated {path} (+{added}/-{removed})"),
        })
    }
}

fn parse_edits(object: &serde_json::Map<String, Value>) -> std::result::Result<Vec<Edit>, String> {
    if let Some(values) = object.get("edits") {
        let values = values
            .as_array()
            .ok_or_else(|| "patch edits must be an array".to_string())?;
        return values.iter().map(parse_edit).collect();
    }
    parse_edit(&Value::Object(object.clone())).map(|edit| vec![edit])
}

fn parse_edit(value: &Value) -> std::result::Result<Edit, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "patch edit must be an object".to_string())?;
    let start = object
        .get("start")
        .or_else(|| object.get("start_anchor"))
        .and_then(Value::as_str)
        .and_then(hashline::parse_anchor_value)
        .or_else(|| parse_line_tag(object, "start_line", "start_tag"))
        .ok_or_else(|| "patch requires a valid start anchor".to_string())?;
    let end = object
        .get("end")
        .or_else(|| object.get("end_anchor"))
        .and_then(Value::as_str)
        .and_then(hashline::parse_anchor_value)
        .or_else(|| parse_line_tag(object, "end_line", "end_tag"))
        .unwrap_or_else(|| start.clone());
    let replacement = object
        .get("replacement")
        .or_else(|| object.get("new_text"))
        .and_then(Value::as_str)
        .ok_or_else(|| "patch requires replacement".to_string())?
        .to_string();
    Ok(Edit {
        start,
        end,
        replacement,
    })
}

fn parse_line_tag(
    object: &serde_json::Map<String, Value>,
    line_key: &str,
    tag_key: &str,
) -> Option<Anchor> {
    Some(Anchor {
        line: object.get(line_key)?.as_u64()? as usize,
        tag: object.get(tag_key)?.as_str()?.to_string(),
    })
}

fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines = old.lines().collect::<Vec<_>>();
    let new_lines = new.lines().collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - suffix - 1] == new_lines[new_lines.len() - suffix - 1]
    {
        suffix += 1;
    }
    let old_end = old_lines.len().saturating_sub(suffix);
    let new_end = new_lines.len().saturating_sub(suffix);
    let mut diff = format!(
        "--- {path}\n+++ {path}\n@@ -{},{} +{},{} @@\n",
        prefix + 1,
        old_end - prefix,
        prefix + 1,
        new_end - prefix
    );
    for line in &old_lines[prefix..old_end] {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in &new_lines[prefix..new_end] {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn diff_stats(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

pub struct ToolPatchPlugin {
    id: PluginId,
    tool: Arc<ToolPatch>,
}

impl ToolPatchPlugin {
    pub fn new(tool: Arc<ToolPatch>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
        }
    }
}

#[async_trait]
impl Plugin for ToolPatchPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_patch"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
