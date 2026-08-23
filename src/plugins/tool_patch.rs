use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::Deserialize;

use crate::core::{
    Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext, ToolDefinition, ToolId,
    ToolOutput, Workdir,
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
        content: String,
        #[serde(default)]
        expected_old_text: Option<String>,
    },
    Delete {
        path: PathBuf,
        #[serde(default)]
        expected_old_text: Option<String>,
    },
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
    },
    Update {
        path: PathBuf,
        old: Vec<u8>,
        content: Vec<u8>,
    },
    Delete {
        path: PathBuf,
        old: Vec<u8>,
    },
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
                                { "type": "object", "additionalProperties": false, "required": ["op", "path", "content"], "properties": { "op": { "const": "update" }, "path": { "type": "string" }, "content": { "type": "string" }, "expected_old_text": { "type": "string" } } },
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
                    staged.push(StagedOperation::Add {
                        path,
                        content: content.into_bytes(),
                    });
                }
                PatchOperation::Update {
                    path,
                    content,
                    expected_old_text,
                } => {
                    let old = context.workdir.read(&path).await.map_err(|error| {
                        Error::Tool(format!("cannot update {}: {error}", path.display()))
                    })?;
                    if let Some(expected) = expected_old_text {
                        if old != expected.as_bytes() {
                            return Err(Error::Tool(format!(
                                "expected old text did not match: {}",
                                path.display()
                            )));
                        }
                    }
                    staged.push(StagedOperation::Update {
                        path,
                        old,
                        content: content.into_bytes(),
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
                StagedOperation::Add { path, content }
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

        Ok(ToolOutput {
            content: serde_json::json!({
                "changed": staged.iter().map(|operation| operation_path(operation).to_string_lossy()).collect::<Vec<_>>()
            }).to_string(),
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
