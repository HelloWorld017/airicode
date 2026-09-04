use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    core::{
        error::{Error, Result},
        hooks::{ConfigReadContext, ConfigReadHook},
        models::{
            NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::note::add_tool_note,
};

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
            description: "Apply one or more exact, non-overlapping replacements to an existing UTF-8 file. Every `oldText` must occur exactly once in the original file. All edits are matched against the same original snapshot and are applied atomically.".into(),
            input: ToolInputDefinition::new(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": { "type": "string", "description": "Exact text for one unique targeted replacement in the original file." },
                                "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                            },
                            "required": ["oldText", "newText"]
                        }
                    }
                },
                "required": ["path", "edits"]
            })),
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
                    content: format!("Patch failed: {content}"),
                },
                ToolOutput::Stop => unreachable!(),
            },
            "patch",
        )
        .await?;
        Ok(output)
    }
}

impl ToolPatch {
    async fn apply(&self, input: Value, context: &ToolContext) -> Result<String> {
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("patch input must be an object".into()))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::Tool("patch requires a non-empty path".into()))?;
        let edits = object
            .get("edits")
            .and_then(Value::as_array)
            .filter(|edits| !edits.is_empty())
            .ok_or_else(|| Error::Tool("patch requires one or more edits".into()))?;
        let workdir = context.operations.workdir()?;
        let bytes = workdir.read(Path::new(path)).await?;
        if bytes.contains(&0) {
            return Err(Error::Tool(
                "cannot patch binary/NUL-containing input".into(),
            ));
        }
        if bytes.len() > self.max_bytes {
            return Err(Error::Tool(format!(
                "file exceeds patch limit of {} bytes",
                self.max_bytes
            )));
        }
        let original = String::from_utf8(bytes)
            .map_err(|_| Error::Tool("cannot patch non-UTF-8 input".into()))?;
        let mut replacements = Vec::with_capacity(edits.len());
        for (index, edit) in edits.iter().enumerate() {
            let edit = edit
                .as_object()
                .ok_or_else(|| Error::Tool(format!("edit {} must be an object", index + 1)))?;
            let old = edit
                .get("oldText")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    Error::Tool(format!("edit {} requires non-empty oldText", index + 1))
                })?;
            let new = edit
                .get("newText")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Tool(format!("edit {} requires newText", index + 1)))?;
            let occurrences = occurrences(&original, old);
            if occurrences.len() != 1 {
                return Err(Error::Tool(format!(
                    "edit {} oldText must occur exactly once in the original file; found {} matches",
                    index + 1,
                    occurrences.len()
                )));
            }
            let start = occurrences[0];
            replacements.push((start, start + old.len(), new));
        }
        replacements.sort_by_key(|(start, _, _)| *start);
        for pair in replacements.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(Error::Tool(
                    "patch edits overlap in the original file".into(),
                ));
            }
        }
        let mut patched = original;
        for (start, end, replacement) in replacements.into_iter().rev() {
            patched.replace_range(start..end, replacement);
        }
        if patched.len() > self.max_bytes {
            return Err(Error::Tool(format!(
                "patched file exceeds limit of {} bytes",
                self.max_bytes
            )));
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        workdir.write(Path::new(path), patched.as_bytes()).await?;
        Ok(format!("Applied {} replacement(s) to {path}", edits.len()))
    }
}

fn occurrences(source: &str, needle: &str) -> Vec<usize> {
    source
        .char_indices()
        .filter_map(|(index, _)| source[index..].starts_with(needle).then_some(index))
        .collect()
}

pub struct ToolPatchPlugin {
    id: PluginId,
}

impl ToolPatchPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
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
        let hook: Arc<dyn ConfigReadHook> = self;
        registry.register_hook(hook)
    }
}

#[async_trait]
impl ConfigReadHook for ToolPatchPlugin {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()> {
        if !context.config.tool.enable_hashline {
            context
                .registry
                .register_tool(Arc::new(ToolPatch::new()), 0)
                .map(|_| ())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_requires_path_and_edits() {
        let input = ToolPatch::new().definition().input;
        assert_eq!(input.schema["required"], json!(["path", "edits"]));
    }
}
