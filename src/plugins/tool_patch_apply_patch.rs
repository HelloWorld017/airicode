use std::{collections::BTreeMap, path::Path, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        models::{
            NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::{note::add_tool_note, schema::json_schema},
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApplyPatchInput {
    #[schemars(description = "Patch content using the *** Begin Patch/End Patch format.")]
    input: String,
}

pub struct ToolPatchApplyPatch {
    id: ToolId,
    max_bytes: usize,
}

impl ToolPatchApplyPatch {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_bytes: 1024 * 1024,
        }
    }
}

impl Default for ToolPatchApplyPatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolPatchApplyPatch {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "apply_patch".into(),
            description: include_str!("../prompts/tool_patch_apply_patch.txt")
                .trim()
                .into(),
            input: ToolInputDefinition::new(json_schema::<ApplyPatchInput>()),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let result = self.apply(input, &context).await;
        let output = match result {
            Ok(content) => ToolOutput::Success { content },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => ToolOutput::Failure {
                content: error.to_string(),
            },
        };
        add_tool_note(
            &context,
            match &output {
                ToolOutput::Success { content } => NoteContent::Subtle {
                    content: content.clone(),
                },
                ToolOutput::Failure { content } => NoteContent::Alert {
                    content: format!("Apply patch failed: {content}"),
                },
                ToolOutput::Stop => unreachable!(),
            },
            "apply_patch",
        )
        .await?;
        Ok(output)
    }
}

impl ToolPatchApplyPatch {
    async fn apply(&self, input: Value, context: &ToolContext) -> Result<String> {
        let input: ApplyPatchInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid apply_patch input: {error}")))?;
        let operations = parse_apply_patch(&input.input).map_err(Error::Tool)?;
        let mut files = BTreeMap::new();
        let mut added = 0;
        let mut updated = 0;
        let mut deleted = 0;
        let mut moved = 0;
        for operation in operations {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match operation {
                PatchOperation::Add { path, content } => {
                    load_file(context, &mut files, &path, self.max_bytes).await?;
                    let state = files.get_mut(&path).expect("loaded file state");
                    if state.content.is_some() {
                        return Err(Error::Tool(format!("cannot add existing file: {path}")));
                    }
                    if content.len() > self.max_bytes {
                        return Err(Error::Tool(format!(
                            "added file exceeds patch limit of {} bytes: {path}",
                            self.max_bytes
                        )));
                    }
                    state.content = Some(content);
                    added += 1;
                }
                PatchOperation::Update {
                    path,
                    move_to,
                    hunks,
                } => {
                    load_file(context, &mut files, &path, self.max_bytes).await?;
                    let mut content = files
                        .get(&path)
                        .and_then(|state| state.content.clone())
                        .ok_or_else(|| {
                            Error::Tool(format!("cannot update missing file: {path}"))
                        })?;
                    for hunk in hunks {
                        content = apply_hunk(&content, &hunk)
                            .map_err(|error| Error::Tool(format!("{path}: {error}")))?;
                    }
                    if content.len() > self.max_bytes {
                        return Err(Error::Tool(format!(
                            "patched file exceeds limit of {} bytes: {path}",
                            self.max_bytes
                        )));
                    }
                    if let Some(move_to) = move_to {
                        if path == move_to {
                            return Err(Error::Tool(format!(
                                "cannot move a file onto itself: {path}"
                            )));
                        }
                        load_file(context, &mut files, &move_to, self.max_bytes).await?;
                        if files
                            .get(&move_to)
                            .is_some_and(|state| state.content.is_some())
                        {
                            return Err(Error::Tool(format!(
                                "cannot move {path} onto existing file: {move_to}"
                            )));
                        }
                        files.get_mut(&path).expect("loaded file state").content = None;
                        files.get_mut(&move_to).expect("loaded file state").content = Some(content);
                        moved += 1;
                    } else {
                        files.get_mut(&path).expect("loaded file state").content = Some(content);
                    }
                    updated += 1;
                }
                PatchOperation::Delete { path } => {
                    load_file(context, &mut files, &path, self.max_bytes).await?;
                    let state = files.get_mut(&path).expect("loaded file state");
                    if state.content.is_none() {
                        return Err(Error::Tool(format!("cannot delete missing file: {path}")));
                    }
                    state.content = None;
                    deleted += 1;
                }
            }
        }
        for (path, state) in &files {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if state.original == state.content {
                continue;
            }
            if let Some(content) = &state.content {
                context
                    .operations
                    .workdir()?
                    .write(Path::new(path), content.as_bytes())
                    .await?;
            }
        }
        for (path, state) in &files {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if state.original != state.content && state.content.is_none() {
                context
                    .operations
                    .workdir()?
                    .remove(Path::new(path))
                    .await?;
            }
        }
        Ok(format!(
            "Applied patch: {added} added, {updated} updated, {deleted} deleted, {moved} moved"
        ))
    }
}

#[derive(Debug)]
struct Hunk {
    before: String,
    after: String,
}

enum PatchOperation {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
    },
    Delete {
        path: String,
    },
}

struct FileState {
    original: Option<String>,
    content: Option<String>,
}

async fn load_file(
    context: &ToolContext,
    files: &mut BTreeMap<String, FileState>,
    path: &str,
    max_bytes: usize,
) -> Result<()> {
    if files.contains_key(path) {
        return Ok(());
    }
    if context.cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let workdir = context.operations.workdir()?;
    let content = if workdir.exists(Path::new(path)).await? {
        let bytes = workdir.read(Path::new(path)).await?;
        if bytes.contains(&0) {
            return Err(Error::Tool(format!(
                "cannot patch binary/NUL-containing input: {path}"
            )));
        }
        if bytes.len() > max_bytes {
            return Err(Error::Tool(format!(
                "file exceeds patch limit of {max_bytes} bytes: {path}"
            )));
        }
        Some(
            String::from_utf8(bytes)
                .map_err(|_| Error::Tool(format!("cannot patch non-UTF-8 input: {path}")))?,
        )
    } else {
        None
    };
    files.insert(
        path.to_string(),
        FileState {
            original: content.clone(),
            content,
        },
    );
    Ok(())
}

fn parse_apply_patch(input: &str) -> std::result::Result<Vec<PatchOperation>, String> {
    let lines = input.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch")
        || lines.last().copied() != Some("*** End Patch")
    {
        return Err("patch must start with `*** Begin Patch` and end with `*** End Patch`".into());
    }
    let mut operations = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let header = lines[index];
        if let Some(path) = patch_path(header, "Add File") {
            index += 1;
            let mut content = String::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                let line = lines[index];
                let line_content = line
                    .strip_prefix('+')
                    .ok_or_else(|| format!("Add File {path} contains non-added line `{line}`"))?;
                content.push_str(line_content);
                content.push('\n');
                index += 1;
            }
            operations.push(PatchOperation::Add { path, content });
            continue;
        }
        if let Some(path) = patch_path(header, "Delete File") {
            index += 1;
            if index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                return Err(format!("Delete File {path} must not contain file content"));
            }
            operations.push(PatchOperation::Delete { path });
            continue;
        }
        let path = patch_path(header, "Update File")
            .ok_or_else(|| format!("expected patch operation header, found `{header}`"))?;
        index += 1;
        let move_to = if index + 1 < lines.len() {
            if let Some(path) = patch_path(lines[index], "Move to") {
                index += 1;
                Some(path)
            } else {
                None
            }
        } else {
            None
        };
        let hunks = parse_hunks(&lines, &mut index)?;
        if hunks.is_empty() {
            return Err(format!("Update File {path} has no hunks"));
        }
        operations.push(PatchOperation::Update {
            path,
            move_to,
            hunks,
        });
    }
    if operations.is_empty() {
        return Err("patch requires at least one operation".into());
    }
    Ok(operations)
}

fn patch_path(header: &str, operation: &str) -> Option<String> {
    header
        .strip_prefix(&format!("*** {operation}: "))
        .filter(|path| !path.trim().is_empty())
        .map(|path| path.trim().to_string())
}

fn parse_hunks(lines: &[&str], index: &mut usize) -> std::result::Result<Vec<Hunk>, String> {
    let mut hunks = Vec::new();
    while *index + 1 < lines.len() && !lines[*index].starts_with("*** ") {
        if !lines[*index].starts_with("@@") {
            return Err(format!(
                "expected hunk header starting with `@@`, found `{}`",
                lines[*index]
            ));
        }
        *index += 1;
        let mut before = String::new();
        let mut after = String::new();
        while *index + 1 < lines.len()
            && !lines[*index].starts_with("@@")
            && !lines[*index].starts_with("*** ")
        {
            let line = lines[*index];
            let (prefix, content) = line
                .split_at_checked(1)
                .ok_or_else(|| "empty patch line".to_string())?;
            match prefix {
                " " => {
                    before.push_str(content);
                    before.push('\n');
                    after.push_str(content);
                    after.push('\n');
                }
                "-" => {
                    before.push_str(content);
                    before.push('\n');
                }
                "+" => {
                    after.push_str(content);
                    after.push('\n');
                }
                _ => return Err(format!("invalid hunk line `{line}`")),
            }
            *index += 1;
        }
        if before == after {
            return Err("hunk does not change the file".into());
        }
        hunks.push(Hunk { before, after });
    }
    Ok(hunks)
}

fn apply_hunk(source: &str, hunk: &Hunk) -> std::result::Result<String, String> {
    let target = if source.contains(&hunk.before) {
        &hunk.before
    } else {
        hunk.before
            .strip_suffix('\n')
            .ok_or_else(|| "hunk context was not found".to_string())?
    };
    let occurrences = source.match_indices(target).count();
    if occurrences != 1 {
        return Err(format!(
            "hunk context must occur exactly once; found {occurrences} matches"
        ));
    }
    let replacement = if target.ends_with('\n') {
        hunk.after.as_str()
    } else {
        hunk.after.trim_end_matches('\n')
    };
    Ok(source.replacen(target, replacement, 1))
}

pub struct ToolPatchApplyPatchPlugin {
    id: PluginId,
    tool: Arc<ToolPatchApplyPatch>,
}

impl ToolPatchApplyPatchPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolPatchApplyPatch::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolPatchApplyPatchPlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_patch_apply_patch"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_supports_file_lifecycle_operations() {
        assert!(parse_apply_patch(
            "*** Begin Patch\n*** Add File: new.txt\n+new\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-old\n+new\n*** Delete File: deleted.txt\n*** End Patch"
        )
        .is_ok());
    }

    #[test]
    fn definition_requires_described_input() {
        let schema = ToolPatchApplyPatch::new().definition().input.schema;

        assert_eq!(schema["required"], serde_json::json!(["input"]));
        assert_eq!(
            schema["properties"]["input"]["description"],
            "Patch content using the *** Begin Patch/End Patch format."
        );
    }
}
