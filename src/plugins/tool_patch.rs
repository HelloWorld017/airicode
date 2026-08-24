use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    core::{
        Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext, ToolDefinition,
        ToolId, ToolOutput, Workdir,
    },
    hashline,
};

const PATCH_TOOL_ID: &str = "builtin.patch";
const PATCH_TOOL_NAME: &str = "patch";
const PATCH_PLUGIN_ID: &str = "builtin.tool-patch";

#[derive(Clone, Debug, Default)]
struct PatchTool;

struct PatchPlugin;

pub fn patch_plugin() -> Arc<dyn Plugin> {
    Arc::new(PatchPlugin)
}

#[async_trait]
impl Plugin for PatchPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PATCH_PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_tool(0, Arc::new(PatchTool))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchInput {
    operations: Vec<PatchOperation>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum PatchOperation {
    Add {
        path: PathBuf,
        content: String,
    },
    Update {
        path: PathBuf,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        edits: Option<Vec<HashlineEdit>>,
        #[serde(default)]
        expected_old_text: Option<String>,
    },
    Delete {
        path: PathBuf,
        #[serde(default)]
        expected_old_text: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HashlineEdit {
    hashline: String,
    new_content: serde_json::Value,
}

impl PatchOperation {
    fn path(&self) -> &Path {
        match self {
            Self::Add { path, .. } | Self::Update { path, .. } | Self::Delete { path, .. } => path,
        }
    }
}

enum StagedOperation {
    Add {
        path: PathBuf,
        content: Vec<u8>,
        result_lines: Vec<usize>,
    },
    Update {
        path: PathBuf,
        old: Vec<u8>,
        content: Vec<u8>,
        result_lines: Vec<usize>,
    },
    Delete {
        path: PathBuf,
        old: Vec<u8>,
    },
}

fn parse_hashline(value: &str) -> Result<(usize, &str)> {
    let (line, hash) = value
        .split_once(':')
        .ok_or_else(|| Error::Tool(format!("malformed hashline: {value}")))?;
    if line.is_empty() || line.starts_with('0') || !line.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Tool(format!("malformed hashline: {value}")));
    }
    let line = line
        .parse::<usize>()
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| Error::Tool(format!("malformed hashline: {value}")))?;
    if hash.len() != 2
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::Tool(format!("malformed hashline: {value}")));
    }
    Ok((line, hash))
}

fn replacement_lines(content: &str) -> Vec<&str> {
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') && lines.len() > 1 {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push("");
    }
    lines
}

fn apply_hashline_edits(
    path: &Path,
    old: &[u8],
    edits: Vec<HashlineEdit>,
) -> Result<(Vec<u8>, Vec<usize>)> {
    hashline::validate_text(old, path)?;
    if edits.is_empty() {
        return Err(Error::Tool("hashline edits may not be empty".into()));
    }
    let ranges = hashline::line_ranges(old);
    let mut by_line = BTreeMap::new();
    for edit in edits {
        let (line_number, supplied_hash) = parse_hashline(&edit.hashline)?;
        if line_number > ranges.len() {
            return Err(Error::Tool(format!(
                "hashline references missing line {line_number}: {}",
                path.display()
            )));
        }
        let new_content = match edit.new_content {
            serde_json::Value::Null => None,
            serde_json::Value::String(content) => Some(content),
            _ => {
                return Err(Error::Tool(format!(
                    "new_content must be a string or null: {}",
                    path.display()
                )))
            }
        };
        if by_line.insert(line_number, new_content).is_some() {
            return Err(Error::Tool(format!(
                "duplicate hashline edit for line {line_number}: {}",
                path.display()
            )));
        }
        let raw = &old[ranges[line_number - 1].clone()];
        if hashline::short_hash(line_number, raw) != supplied_hash {
            return Err(Error::Tool(format!(
                "stale hashline for line {line_number}: {}",
                path.display()
            )));
        }
    }

    let default_eol = ranges
        .iter()
        .map(|range| hashline::eol(&old[range.clone()]))
        .find(|eol| !eol.is_empty())
        .unwrap_or(b"\n");
    let had_final_newline = old.ends_with(b"\n");
    let mut output = Vec::new();
    let mut result_lines = Vec::new();
    let mut output_line = 0;
    for (index, range) in ranges.iter().enumerate() {
        let line_number = index + 1;
        let raw = &old[range.clone()];
        match by_line.remove(&line_number) {
            None => {
                output.extend_from_slice(raw);
                output_line += 1;
            }
            Some(None) => {}
            Some(Some(new_content)) => {
                if new_content.contains('\0') {
                    return Err(Error::Tool(format!(
                        "replacement contains a NUL byte: {}",
                        path.display()
                    )));
                }
                let replacement = new_content.replace("\r\n", "\n");
                let replacement = replacement_lines(&replacement);
                for (replacement_index, text) in replacement.iter().enumerate() {
                    output.extend_from_slice(text.as_bytes());
                    let is_last = replacement_index + 1 == replacement.len();
                    if is_last {
                        output.extend_from_slice(hashline::eol(raw));
                    } else {
                        output.extend_from_slice(default_eol);
                    }
                    output_line += 1;
                    result_lines.push(output_line);
                }
            }
        }
    }

    if !had_final_newline && output.ends_with(b"\n") {
        output.pop();
        if output.ends_with(b"\r") {
            output.pop();
        }
    } else if had_final_newline && !output.is_empty() && !output.ends_with(b"\n") {
        output.extend_from_slice(default_eol);
    }
    hashline::validate_text(&output, path)?;
    Ok((output, result_lines))
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
            "patch path must be project-relative: {}",
            path.display()
        )));
    }
    Ok(())
}

async fn rollback(workdir: &dyn Workdir, applied: &[&StagedOperation]) {
    for operation in applied.iter().rev() {
        match operation {
            StagedOperation::Add { path, .. } => {
                let _ = workdir.remove(path).await;
            }
            StagedOperation::Update { path, old, .. } | StagedOperation::Delete { path, old } => {
                let _ = workdir.write(path, old).await;
            }
        }
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn id(&self) -> ToolId {
        ToolId::new(PATCH_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: PATCH_TOOL_NAME.into(),
            description: "Atomically write a structured set of project file additions, updates, and deletions.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["operations"],
                "properties": {
                    "operations": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "oneOf": [
                                { "type": "object", "additionalProperties": false, "required": ["op", "path", "content"], "properties": { "op": { "const": "add" }, "path": { "type": "string" }, "content": { "type": "string" } } },
                                { "type": "object", "additionalProperties": false, "required": ["op", "path"], "properties": { "op": { "const": "update" }, "path": { "type": "string" }, "content": { "type": "string" }, "edits": { "type": "array", "minItems": 1, "items": { "type": "object", "additionalProperties": false, "required": ["hashline", "new_content"], "properties": { "hashline": { "type": "string", "pattern": "^[1-9][0-9]*:[0-9a-f]{2}$" }, "new_content": { "type": ["string", "null"] } } } }, "expected_old_text": { "type": "string" } }, "oneOf": [{ "required": ["content"] }, { "required": ["edits"] }] },
                                { "type": "object", "additionalProperties": false, "required": ["op", "path"], "properties": { "op": { "const": "delete" }, "path": { "type": "string" }, "expected_old_text": { "type": "string" } } }
                            ]
                        }
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let input: PatchInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid patch input: {error}")))?;
        if input.operations.is_empty() {
            return Err(Error::Tool("patch operations may not be empty".into()));
        }
        let mut paths = BTreeSet::new();
        for operation in &input.operations {
            validate_path(operation.path())?;
            if !paths.insert(operation.path().to_path_buf()) {
                return Err(Error::Tool(format!(
                    "patch contains duplicate path: {}",
                    operation.path().display()
                )));
            }
        }

        let mut staged = Vec::with_capacity(input.operations.len());
        for operation in input.operations {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match operation {
                PatchOperation::Add { path, content } => {
                    match std::fs::symlink_metadata(context.workdir.root().join(&path)) {
                        Ok(_) => {
                            return Err(Error::Tool(format!(
                                "cannot add existing file: {}",
                                path.display()
                            )))
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(Error::Tool(format!(
                                "cannot inspect add path {}: {error}",
                                path.display()
                            )))
                        }
                    }
                    let content = content.into_bytes();
                    hashline::validate_text(&content, &path)?;
                    let result_lines = (1..=hashline::line_ranges(&content).len()).collect();
                    staged.push(StagedOperation::Add {
                        path,
                        content,
                        result_lines,
                    });
                }
                PatchOperation::Update {
                    path,
                    content,
                    edits,
                    expected_old_text,
                } => {
                    let old = context.workdir.read(&path).await.map_err(|error| {
                        Error::Tool(format!("cannot update {}: {error}", path.display()))
                    })?;
                    if let Some(expected) = expected_old_text.as_ref() {
                        if old != expected.as_bytes() {
                            return Err(Error::Tool(format!(
                                "expected old text did not match: {}",
                                path.display()
                            )));
                        }
                    }
                    let (content, result_lines) = match (content, edits) {
                        (Some(content), None) => {
                            let content = content.into_bytes();
                            hashline::validate_text(&content, &path)?;
                            let lines = (1..=hashline::line_ranges(&content).len()).collect();
                            (content, lines)
                        }
                        (None, Some(edits)) if expected_old_text.is_none() => {
                            apply_hashline_edits(&path, &old, edits)?
                        }
                        (None, Some(_)) => {
                            return Err(Error::Tool(
                                "expected_old_text is only valid with content updates".into(),
                            ))
                        }
                        _ => {
                            return Err(Error::Tool(
                                "update requires exactly one of content or edits".into(),
                            ))
                        }
                    };
                    staged.push(StagedOperation::Update {
                        path,
                        old,
                        content,
                        result_lines,
                    });
                }
                PatchOperation::Delete {
                    path,
                    expected_old_text,
                } => {
                    let old = context.workdir.read(&path).await.map_err(|error| {
                        Error::Tool(format!("cannot delete {}: {error}", path.display()))
                    })?;
                    if let Some(expected) = expected_old_text {
                        if old != expected.as_bytes() {
                            return Err(Error::Tool(format!(
                                "expected old text did not match: {}",
                                path.display()
                            )));
                        }
                    }
                    staged.push(StagedOperation::Delete { path, old });
                }
            }
        }

        let mut applied = Vec::new();
        for operation in &staged {
            if context.cancellation.is_cancelled() {
                rollback(context.workdir.as_ref(), &applied).await;
                return Err(Error::Cancelled);
            }
            let result = match operation {
                StagedOperation::Add { path, content, .. }
                | StagedOperation::Update { path, content, .. } => {
                    context.workdir.write(path, content).await
                }
                StagedOperation::Delete { path, .. } => context.workdir.remove(path).await,
            };
            if let Err(error) = result {
                rollback(context.workdir.as_ref(), &applied).await;
                return Err(Error::Tool(format!(
                    "patch failed and was rolled back: {error}"
                )));
            }
            applied.push(operation);
        }

        let hashlines = staged
            .iter()
            .filter_map(operation_hashlines)
            .collect::<BTreeMap<_, _>>();
        Ok(ToolOutput {
            content: serde_json::json!({
                "changed": staged.iter().map(|operation| operation_path(operation).to_string_lossy()).collect::<Vec<_>>(),
                "hashlines": hashlines,
            })
            .to_string(),
            is_error: false,
        })
    }
}

fn operation_path(operation: &StagedOperation) -> &Path {
    match operation {
        StagedOperation::Add { path, .. }
        | StagedOperation::Update { path, .. }
        | StagedOperation::Delete { path, .. } => path,
    }
}

fn operation_hashlines(operation: &StagedOperation) -> Option<(String, Vec<String>)> {
    let (path, content, result_lines) = match operation {
        StagedOperation::Add {
            path,
            content,
            result_lines,
        }
        | StagedOperation::Update {
            path,
            content,
            result_lines,
            ..
        } => (path, content, result_lines),
        StagedOperation::Delete { .. } => return None,
    };
    let ranges = hashline::line_ranges(content);
    Some((
        path.to_string_lossy().into_owned(),
        result_lines
            .iter()
            .map(|line_number| {
                hashline::record(*line_number, &content[ranges[*line_number - 1].clone()])
            })
            .collect(),
    ))
}
