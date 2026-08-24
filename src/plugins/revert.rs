use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

use crate::core::{
    AfterToolExecutionHook, BeforeHookResult, BeforeToolExecutionHook, Command, CommandContext,
    CommandDescriptor, CommandId, CommandInvocation, CommandResult, Error, MessageId, MessagePart,
    Plugin, PluginId, PluginRegistrar, Result, SessionId, ToolCallId, ToolExecutionContext,
    ToolOutput, Workdir,
};

const PLUGIN_ID: &str = "builtin.revert";
const COMMAND_ID: &str = "builtin.revert";
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
        registrar.register_command(0, service)
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
    assistant_message_id: MessageId,
    history_anchor: Option<MessageId>,
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
    assistant_message_id: MessageId,
    history_anchor: Option<MessageId>,
    entries: Vec<RevertEntry>,
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
        let paths = explicit_paths(&context.call.arguments)?;
        if paths.is_empty() {
            return Ok(BeforeHookResult::Continue);
        }

        let history = context.hook_context.history().snapshot().await?;
        let assistant_index = history
            .messages
            .iter()
            .position(|message| message.id == context.assistant_message_id)
            .ok_or_else(|| {
                Error::Plugin(format!(
                    "could not locate assistant message {} for revert capture",
                    context.assistant_message_id
                ))
            })?;
        if !history.messages[assistant_index]
            .content
            .iter()
            .any(|part| matches!(part, MessagePart::ToolCall { id, .. } if id == &context.call.id))
        {
            return Err(Error::Plugin(format!(
                "assistant message {} does not contain tool call {}",
                context.assistant_message_id, context.call.id
            )));
        }
        let history_anchor = assistant_index
            .checked_sub(1)
            .map(|index| history.messages[index].id);

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
                    assistant_message_id: context.assistant_message_id,
                    history_anchor,
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
            assistant_message_id: capture.assistant_message_id,
            history_anchor: capture.history_anchor,
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
impl Command for RevertService {
    fn id(&self) -> CommandId {
        CommandId::new(COMMAND_ID)
    }

    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "revert".into(),
            description:
                "Restore files changed by a previous tool call when they have not changed again."
                    .into(),
            usage: "/revert [record-id]".into(),
        }
    }

    async fn execute(
        &self,
        invocation: CommandInvocation,
        context: CommandContext,
    ) -> Result<CommandResult> {
        let record_id = parse_optional_record_id(&invocation.arguments)?;
        let record = {
            let state = self.state.lock().expect("revert state lock poisoned");
            let records = state
                .records
                .get(&context.hook_context.session_id)
                .ok_or_else(|| {
                    Error::Plugin("this session has no recorded file changes to revert".into())
                })?;
            match record_id {
                Some(id) => records.iter().find(|record| record.id == id),
                None => records.last(),
            }
            .cloned()
            .ok_or_else(|| Error::Plugin("revert record was not found in this session".into()))?
        };

        let history = context.history();
        let snapshot = history.snapshot().await?;
        let assistant_index = snapshot
            .messages
            .iter()
            .position(|message| message.id == record.assistant_message_id)
            .ok_or_else(|| {
                Error::Plugin(format!(
                    "cannot revert record {}: its assistant message is no longer in canonical history",
                    record.id
                ))
            })?;
        match record.history_anchor {
            Some(anchor) => {
                if assistant_index == 0 || snapshot.messages[assistant_index - 1].id != anchor {
                    return Err(Error::Plugin(format!(
                        "cannot revert record {}: its canonical history anchor changed",
                        record.id
                    )));
                }
                history.truncate_after(snapshot.revision, anchor).await?;
            }
            None => {
                if assistant_index != 0 {
                    return Err(Error::Plugin(format!(
                        "cannot revert record {} without its original history anchor",
                        record.id
                    )));
                }
                let last = snapshot.messages.last().ok_or_else(|| {
                    Error::Plugin("cannot clear an empty canonical history".into())
                })?;
                history
                    .replace_range(
                        snapshot.revision,
                        record.assistant_message_id,
                        last.id,
                        Vec::new(),
                    )
                    .await?;
            }
        }

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
            .get_mut(&context.hook_context.session_id)
            .expect("selected revert record disappeared")
            .retain(|candidate| candidate.id != record.id);
        Ok(CommandResult {
            content: serde_json::to_string(&outcome).map_err(|error| {
                Error::Plugin(format!("could not encode revert result: {error}"))
            })?,
        })
    }
}

fn parse_optional_record_id(arguments: &str) -> Result<Option<Uuid>> {
    let argument = arguments.trim();
    if argument.is_empty() {
        return Ok(None);
    }
    if argument.split_whitespace().count() != 1 {
        return Err(Error::Plugin("usage: /revert [record-id]".into()));
    }
    Uuid::parse_str(argument)
        .map(Some)
        .map_err(|_| Error::Plugin(format!("invalid revert record id: {argument}")))
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
            assistant_message_id: crate::core::MessageId::new(),
            workdir,
            call: crate::core::ToolCallDraft {
                id: ToolCallId::new(id),
                name: "any-tool".into(),
                arguments,
            },
        }
    }

    #[tokio::test]
    async fn plugin_registers_command_and_both_hooks() {
        let core = Core::new()
            .with_plugin(revert_plugin(RevertConfig::default()))
            .build()
            .await
            .unwrap();
        assert_eq!(core.commands().ids(), vec![CommandId::new(COMMAND_ID)]);
        assert_eq!(
            core.commands().descriptors()[0].usage,
            "/revert [record-id]"
        );
        assert!(core.tools().ids().is_empty());
        assert!(core.workdir_layers().ids().is_empty());

        let omitted = Core::new().build().await.unwrap();
        assert!(omitted.commands().ids().is_empty());
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
        let execution = execution_context(
            session_id,
            workdir.clone(),
            "call",
            serde_json::json!({"paths": ["changed.txt", "created.txt", "conflict.txt"]}),
        );
        let paths = explicit_paths(&execution.call.arguments).unwrap();
        let mut preimages = Vec::new();
        for path in &paths {
            preimages.push(read_state(workdir.as_ref(), path).await.unwrap());
        }
        service.state.lock().unwrap().pending.insert(
            (session_id, execution.call.id.clone()),
            RevertCapture {
                workdir: workdir.clone(),
                assistant_message_id: execution.assistant_message_id,
                history_anchor: Some(MessageId::new()),
                paths,
                preimages,
            },
        );
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
                assistant_message_id: MessageId::new(),
                history_anchor: Some(MessageId::new()),
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
