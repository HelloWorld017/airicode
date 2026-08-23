use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::{
    AfterToolExecutionHook, BeforeHookResult, BeforeToolExecutionHook, Error, Plugin, PluginId,
    PluginRegistrar, Result, SessionId, Tool, ToolCallId, ToolContext, ToolDefinition,
    ToolExecutionContext, ToolId, ToolOutput, Workdir,
};

const PLUGIN_ID: &str = "builtin.revert";
const TOOL_ID: &str = "builtin.revert";
const BEFORE_HOOK_ID: &str = "builtin.revert.capture-preimages";
const AFTER_HOOK_ID: &str = "builtin.revert.capture-postimages";

#[derive(Clone, Debug)]
pub struct RevertConfig {
    /// Maximum completed operations retained for each session.
    pub max_records_per_session: usize,
}

impl Default for RevertConfig {
    fn default() -> Self {
        Self {
            max_records_per_session: 100,
        }
    }
}

pub fn revert_plugin(config: RevertConfig) -> Arc<dyn Plugin> {
    Arc::new(RevertPlugin {
        config,
        state: Arc::new(Mutex::new(RevertState::default())),
    })
}

struct RevertPlugin {
    config: RevertConfig,
    state: Arc<Mutex<RevertState>>,
}

#[async_trait]
impl Plugin for RevertPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        if self.config.max_records_per_session == 0 {
            return Err(Error::Plugin(
                "revert max_records_per_session must be greater than zero".into(),
            ));
        }
        let service = Arc::new(RevertService {
            max_records_per_session: self.config.max_records_per_session,
            state: self.state.clone(),
        });
        registrar.register_before_tool_execution(BEFORE_HOOK_ID, 0, service.clone())?;
        registrar.register_after_tool_execution(AFTER_HOOK_ID, 0, service.clone())?;
        registrar.register_tool(0, service)
    }
}

#[derive(Default)]
struct RevertState {
    pending: BTreeMap<(SessionId, ToolCallId), RevertCapture>,
    records: BTreeMap<SessionId, Vec<RevertRecord>>,
}

struct RevertService {
    max_records_per_session: usize,
    state: Arc<Mutex<RevertState>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FileState {
    /// `None` means the path did not exist.
    contents: Option<Vec<u8>>,
}

struct RevertCapture {
    workdir: Arc<dyn Workdir>,
    paths: Vec<PathBuf>,
    preimages: Vec<FileState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RevertEntry {
    path: PathBuf,
    preimage: FileState,
    postimage: FileState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RevertRecord {
    id: Uuid,
    tool_name: String,
    recorded_at_ms: u64,
    entries: Vec<RevertEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevertInput {
    #[serde(default)]
    record_id: Option<Uuid>,
}

#[derive(Default, Serialize)]
struct RevertOutcome {
    record_id: Option<Uuid>,
    restored: Vec<PathBuf>,
    conflicts: Vec<PathBuf>,
}

#[async_trait]
impl BeforeToolExecutionHook for RevertService {
    async fn before_tool_execution(
        &self,
        context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult> {
        if context.call.name == "revert" {
            return Ok(BeforeHookResult::Continue);
        }
        let paths = explicit_paths(&context.call.arguments)?;
        if paths.is_empty() {
            return Ok(BeforeHookResult::Continue);
        }

        let mut captured_paths = Vec::new();
        let mut preimages = Vec::new();
        for path in paths {
            if context.workdir.root().join(&path).is_dir() {
                continue;
            }
            preimages.push(read_state(context.workdir.as_ref(), &path).await?);
            captured_paths.push(path);
        }
        if captured_paths.is_empty() {
            return Ok(BeforeHookResult::Continue);
        }

        self.state
            .lock()
            .expect("revert state lock poisoned")
            .pending
            .insert(
                (context.hook_context.session_id, context.call.id.clone()),
                RevertCapture {
                    workdir: context.workdir.clone(),
                    paths: captured_paths,
                    preimages,
                },
            );
        Ok(BeforeHookResult::Continue)
    }
}

#[async_trait]
impl AfterToolExecutionHook for RevertService {
    async fn after_tool_execution(
        &self,
        context: &ToolExecutionContext,
        _output: &mut ToolOutput,
    ) -> Result<()> {
        let capture = self
            .state
            .lock()
            .expect("revert state lock poisoned")
            .pending
            .remove(&(context.hook_context.session_id, context.call.id.clone()));
        let Some(capture) = capture else {
            return Ok(());
        };

        let mut entries = Vec::with_capacity(capture.paths.len());
        for (path, preimage) in capture.paths.into_iter().zip(capture.preimages) {
            let postimage = read_state(capture.workdir.as_ref(), &path).await?;
            if preimage != postimage {
                entries.push(RevertEntry {
                    path,
                    preimage,
                    postimage,
                });
            }
        }
        if entries.is_empty() {
            return Ok(());
        }

        let record = RevertRecord {
            id: Uuid::new_v4(),
            tool_name: context.call.name.clone(),
            recorded_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            entries,
        };
        let mut state = self.state.lock().expect("revert state lock poisoned");
        let records = state
            .records
            .entry(context.hook_context.session_id)
            .or_default();
        records.push(record);
        if records.len() > self.max_records_per_session {
            records.remove(0);
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for RevertService {
    fn id(&self) -> ToolId {
        ToolId::new(TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "revert".into(),
            description:
                "Restore files changed by a previous tool call when they have not changed again."
                    .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "record_id": {
                        "type": "string",
                        "description": "Operation record to restore; omit to restore the latest operation in this session."
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let input: RevertInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid revert input: {error}")))?;
        let record = {
            let state = self.state.lock().expect("revert state lock poisoned");
            let records = state.records.get(&context.session_id).ok_or_else(|| {
                Error::Tool("this session has no recorded file changes to revert".into())
            })?;
            match input.record_id {
                Some(id) => records.iter().find(|record| record.id == id),
                None => records.last(),
            }
            .cloned()
            .ok_or_else(|| Error::Tool("revert record was not found in this session".into()))?
        };

        let mut outcome = RevertOutcome {
            record_id: Some(record.id),
            ..RevertOutcome::default()
        };
        for entry in record.entries.iter().rev() {
            if read_state(context.workdir.as_ref(), &entry.path).await? != entry.postimage {
                outcome.conflicts.push(entry.path.clone());
                continue;
            }
            match &entry.preimage.contents {
                Some(contents) => context.workdir.write(&entry.path, contents).await?,
                None => context.workdir.remove(&entry.path).await?,
            }
            outcome.restored.push(entry.path.clone());
        }

        self.state
            .lock()
            .expect("revert state lock poisoned")
            .records
            .get_mut(&context.session_id)
            .expect("selected revert record disappeared")
            .retain(|candidate| candidate.id != record.id);
        Ok(ToolOutput {
            content: serde_json::to_string(&outcome)
                .map_err(|error| Error::Tool(format!("could not encode revert result: {error}")))?,
            is_error: false,
        })
    }
}

fn explicit_paths(arguments: &serde_json::Value) -> Result<Vec<PathBuf>> {
    fn visit(
        value: &serde_json::Value,
        path_value: bool,
        paths: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    let key = key.to_ascii_lowercase();
                    let is_path = key == "path"
                        || key == "paths"
                        || key.ends_with("_path")
                        || key.ends_with("_paths");
                    visit(value, is_path, paths)?;
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, path_value, paths)?;
                }
            }
            serde_json::Value::String(value) if path_value => {
                let path = PathBuf::from(value);
                validate_path(&path)?;
                paths.insert(path);
            }
            _ => {}
        }
        Ok(())
    }

    let mut paths = BTreeSet::new();
    visit(arguments, false, &mut paths)?;
    Ok(paths.into_iter().collect())
}

async fn read_state(workdir: &dyn Workdir, path: &Path) -> Result<FileState> {
    match workdir.read(path).await {
        Ok(contents) => Ok(FileState {
            contents: Some(contents),
        }),
        Err(read_error) => match fs::symlink_metadata(workdir.root().join(path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(FileState { contents: None })
            }
            _ => Err(Error::Plugin(format!(
                "could not capture revert state for {}: {read_error}",
                path.display()
            ))),
        },
    }
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
        return Err(Error::Plugin(format!(
            "revert path must be project-relative: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Core, HookContext, NativeWorkdir, ProjectId, TurnId};

    fn execution_context(
        session_id: SessionId,
        workdir: Arc<dyn Workdir>,
        id: &str,
        arguments: serde_json::Value,
    ) -> ToolExecutionContext {
        ToolExecutionContext {
            hook_context: HookContext {
                project_id: ProjectId::new(),
                session_id,
            },
            turn_id: TurnId::new(),
            workdir,
            call: crate::core::ToolCallDraft {
                id: ToolCallId::new(id),
                name: "any-tool".into(),
                arguments,
            },
        }
    }

    #[tokio::test]
    async fn plugin_registers_tool_and_both_hooks() {
        let core = Core::new()
            .with_plugin(revert_plugin(RevertConfig::default()))
            .build()
            .await
            .unwrap();
        assert_eq!(core.tools().ids(), vec![ToolId::new(TOOL_ID)]);
        assert!(core.workdir_layers().ids().is_empty());
    }

    #[test]
    fn discovers_nested_explicit_path_arguments_only() {
        let paths = explicit_paths(&serde_json::json!({
            "path": "one.txt",
            "options": { "output_paths": ["two.txt", "one.txt"] },
            "script": "not-a-path"
        }))
        .unwrap();
        assert_eq!(
            paths,
            vec![PathBuf::from("one.txt"), PathBuf::from("two.txt")]
        );
        assert!(explicit_paths(&serde_json::json!({"path": "../outside"})).is_err());
    }

    #[tokio::test]
    async fn hooks_restore_matching_postimages_without_overwriting_later_changes() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("changed.txt"), b"before").unwrap();
        fs::write(directory.path().join("conflict.txt"), b"before").unwrap();
        let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let service = RevertService {
            max_records_per_session: 10,
            state: Arc::new(Mutex::new(RevertState::default())),
        };
        let session_id = SessionId::new();
        let mut execution = execution_context(
            session_id,
            workdir.clone(),
            "call",
            serde_json::json!({"paths": ["changed.txt", "created.txt", "conflict.txt"]}),
        );
        service.before_tool_execution(&mut execution).await.unwrap();
        workdir
            .write(Path::new("changed.txt"), b"operation")
            .await
            .unwrap();
        workdir
            .write(Path::new("created.txt"), b"operation")
            .await
            .unwrap();
        workdir
            .write(Path::new("conflict.txt"), b"operation")
            .await
            .unwrap();
        service
            .after_tool_execution(
                &execution,
                &mut ToolOutput {
                    content: String::new(),
                    is_error: false,
                },
            )
            .await
            .unwrap();
        workdir
            .write(Path::new("conflict.txt"), b"user edit")
            .await
            .unwrap();

        let record = service
            .state
            .lock()
            .unwrap()
            .records
            .get(&session_id)
            .unwrap()[0]
            .clone();
        let mut outcome = RevertOutcome::default();
        for entry in record.entries.iter().rev() {
            if read_state(workdir.as_ref(), &entry.path).await.unwrap() != entry.postimage {
                outcome.conflicts.push(entry.path.clone());
                continue;
            }
            match &entry.preimage.contents {
                Some(contents) => workdir.write(&entry.path, contents).await.unwrap(),
                None => workdir.remove(&entry.path).await.unwrap(),
            }
            outcome.restored.push(entry.path.clone());
        }

        assert_eq!(
            fs::read(directory.path().join("changed.txt")).unwrap(),
            b"before"
        );
        assert!(!directory.path().join("created.txt").exists());
        assert_eq!(
            fs::read(directory.path().join("conflict.txt")).unwrap(),
            b"user edit"
        );
        assert_eq!(outcome.conflicts, vec![PathBuf::from("conflict.txt")]);
    }

    #[tokio::test]
    async fn records_are_isolated_by_session() {
        let state = Arc::new(Mutex::new(RevertState::default()));
        let first = SessionId::new();
        state.lock().unwrap().records.insert(
            first,
            vec![RevertRecord {
                id: Uuid::new_v4(),
                tool_name: "tool".into(),
                recorded_at_ms: 0,
                entries: Vec::new(),
            }],
        );
        assert!(!state
            .lock()
            .unwrap()
            .records
            .contains_key(&SessionId::new()));
    }
}
