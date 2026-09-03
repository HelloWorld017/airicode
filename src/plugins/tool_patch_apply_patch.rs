use std::{collections::BTreeMap, path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    core::{
        error::{Error, Result},
        models::{
            NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::note::add_tool_note,
};

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
            description: format!(
                "{}\n\nThe JSON fallback is `{{ \"patch\": \"...\" }}`.",
                include_str!("../prompts/tool_patch_apply_patch.txt").trim()
            ),
            input: ToolInputDefinition::new(json!({
                "type": "object",
                "properties": { "patch": { "type": "string" } },
                "required": ["patch"]
            }))
            .with_freeform_parser(parse_apply_patch_freeform),
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
        let patch = input
            .get("patch")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("apply_patch requires patch".into()))?;
        let updates = parse_apply_patch(patch).map_err(Error::Tool)?;
        let mut planned = BTreeMap::new();
        for (path, hunks) in updates {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let bytes = context.workdir.read(Path::new(&path)).await?;
            if bytes.contains(&0) {
                return Err(Error::Tool(format!(
                    "cannot patch binary/NUL-containing input: {path}"
                )));
            }
            if bytes.len() > self.max_bytes {
                return Err(Error::Tool(format!(
                    "file exceeds patch limit of {} bytes: {path}",
                    self.max_bytes
                )));
            }
            let mut content = String::from_utf8(bytes)
                .map_err(|_| Error::Tool(format!("cannot patch non-UTF-8 input: {path}")))?;
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
            planned.insert(path, content);
        }
        for (path, content) in &planned {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            context
                .workdir
                .write(Path::new(path), content.as_bytes())
                .await?;
        }
        Ok(format!("Applied patch to {} file(s)", planned.len()))
    }
}

pub fn parse_apply_patch_freeform(input: &str) -> Result<Value> {
    Ok(json!({ "patch": input }))
}

#[derive(Debug)]
struct Hunk {
    before: String,
    after: String,
}

fn parse_apply_patch(input: &str) -> std::result::Result<BTreeMap<String, Vec<Hunk>>, String> {
    let lines = input.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch")
        || lines.last().copied() != Some("*** End Patch")
    {
        return Err("patch must start with `*** Begin Patch` and end with `*** End Patch`".into());
    }
    let mut updates = BTreeMap::<String, Vec<Hunk>>::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let header = lines[index];
        if header.starts_with("*** Add File:")
            || header.starts_with("*** Delete File:")
            || header.starts_with("*** Move to:")
        {
            return Err("apply_patch only edits existing files; use fs_write, fs_delete, or fs_rename for file lifecycle operations".into());
        }
        let path = header
            .strip_prefix("*** Update File: ")
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| format!("expected `*** Update File: path`, found `{header}`"))?
            .trim()
            .to_string();
        index += 1;
        let mut hunks = Vec::new();
        while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
            if !lines[index].starts_with("@@") {
                return Err(format!(
                    "expected hunk header starting with `@@`, found `{}`",
                    lines[index]
                ));
            }
            index += 1;
            let mut before = String::new();
            let mut after = String::new();
            while index + 1 < lines.len()
                && !lines[index].starts_with("@@")
                && !lines[index].starts_with("*** ")
            {
                let line = lines[index];
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
                index += 1;
            }
            if before == after {
                return Err("hunk does not change the file".into());
            }
            hunks.push(Hunk { before, after });
        }
        if hunks.is_empty() {
            return Err(format!("Update File {path} has no hunks"));
        }
        updates.entry(path).or_default().extend(hunks);
    }
    if updates.is_empty() {
        return Err("patch requires at least one Update File operation".into());
    }
    Ok(updates)
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
    fn parser_rejects_file_lifecycle_operations() {
        let error =
            parse_apply_patch("*** Begin Patch\n*** Add File: x\n+x\n*** End Patch").unwrap_err();
        assert!(error.contains("fs_write"));
    }
}
