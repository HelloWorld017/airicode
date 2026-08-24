use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Output, Stdio},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::{
    AfterToolExecutionHook, BeforeHookResult, BeforeToolExecutionHook, BeforeUserMessageHook,
    Command, CommandContext, CommandDescriptor, CommandId, CommandInvocation, CommandOutput,
    CommandResult, CommandSpec, Error, HookContext, Message, MessageId, NativeWorkdir, Plugin,
    PluginId, PluginRegistrar, ProjectId, Result, SessionId, ToolExecutionContext, ToolOutput,
    Workdir, WorkdirLayer, WorkdirLayerContext, WorkdirLayerId,
};

const PLUGIN_ID: &str = "builtin.git-worktree";
const LAYER_ID: &str = "builtin.git-worktree";
const APPLY_COMMAND_ID: &str = "builtin.git-worktree.apply";
const DISCARD_COMMAND_ID: &str = "builtin.git-worktree.discard";
const REVERT_COMMAND_ID: &str = "builtin.git-worktree.revert";
const USER_HOOK_ID: &str = "builtin.git-worktree.checkpoint";
const BEFORE_TOOL_HOOK_ID: &str = "builtin.git-worktree.before-tool";
const AFTER_TOOL_HOOK_ID: &str = "builtin.git-worktree.after-tool";

#[derive(Clone, Debug)]
pub struct GitWorktreeConfig {
    pub project: PathBuf,
    pub data_dir: PathBuf,
}

impl GitWorktreeConfig {
    pub fn new(project: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            project: project.into(),
            data_dir: data_dir.into(),
        }
    }
}

pub fn git_worktree_plugin(config: GitWorktreeConfig) -> Arc<dyn Plugin> {
    Arc::new(GitWorktreePlugin { config })
}

struct GitWorktreePlugin {
    config: GitWorktreeConfig,
}

#[async_trait]
impl Plugin for GitWorktreePlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        let service = Arc::new(WorktreeService::new(&self.config)?);

        // The worktree replaces the host root before policy/sandbox layers wrap it.
        registrar.register_workdir_layer(-100, service.clone())?;
        registrar.register_before_user_message(USER_HOOK_ID, 100, service.clone())?;
        // Approval and other cancelling hooks run before a pending operation is recorded.
        registrar.register_before_tool_execution(BEFORE_TOOL_HOOK_ID, -100, service.clone())?;
        // Commit before unrelated after-hooks can fail.
        registrar.register_after_tool_execution(AFTER_TOOL_HOOK_ID, -100, service.clone())?;
        registrar.register_command(
            0,
            Arc::new(WorktreeCommand::new(CommandKind::Apply, service.clone())),
        )?;
        registrar.register_command(
            0,
            Arc::new(WorktreeCommand::new(CommandKind::Discard, service.clone())),
        )?;
        registrar.register_command(
            0,
            Arc::new(WorktreeCommand::new(CommandKind::Revert, service)),
        )
    }
}

struct WorktreeService {
    source_project: PathBuf,
    repository: PathBuf,
    project_relative: PathBuf,
    common_dir: PathBuf,
    managed_root: PathBuf,
    project_hash: String,
    sessions: Mutex<BTreeMap<SessionId, ManagedState>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedState {
    project_id: ProjectId,
    session_id: SessionId,
    source_project: PathBuf,
    repository: PathBuf,
    common_dir: PathBuf,
    path: PathBuf,
    state_file: PathBuf,
    base_head: String,
    expected_head: String,
    checkpoints: BTreeMap<MessageId, String>,
    pending: Option<PendingTool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PendingTool {
    call_id: String,
    expected_head: String,
}

impl WorktreeService {
    fn new(config: &GitWorktreeConfig) -> Result<Self> {
        let source_project = fs::canonicalize(&config.project).map_err(|error| {
            Error::Plugin(format!(
                "could not resolve Git project {}: {error}",
                config.project.display()
            ))
        })?;
        let repository = git_stdout(
            &source_project,
            [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        )?;
        let repository = fs::canonicalize(repository.trim()).map_err(|error| {
            Error::Plugin(format!("could not resolve Git repository root: {error}"))
        })?;
        let project_relative = source_project
            .strip_prefix(&repository)
            .map_err(|_| Error::Plugin("Git project is outside its repository root".into()))?
            .to_path_buf();
        let common_dir = canonical_git_path(&repository, "--git-common-dir")?;

        let data_dir = if config.data_dir.is_absolute() {
            config.data_dir.clone()
        } else {
            std::env::current_dir()
                .map_err(|error| {
                    Error::Plugin(format!("could not resolve current directory: {error}"))
                })?
                .join(&config.data_dir)
        };
        fs::create_dir_all(&data_dir).map_err(|error| {
            Error::Plugin(format!(
                "could not create Airicode data directory {}: {error}",
                data_dir.display()
            ))
        })?;
        let data_dir = fs::canonicalize(data_dir).map_err(|error| {
            Error::Plugin(format!(
                "could not resolve Airicode data directory: {error}"
            ))
        })?;
        let managed_root = data_dir.join("worktrees");
        fs::create_dir_all(&managed_root).map_err(|error| {
            Error::Plugin(format!(
                "could not create managed worktree root {}: {error}",
                managed_root.display()
            ))
        })?;

        Ok(Self {
            project_hash: sha256_hex(source_project.as_os_str().as_encoded_bytes()),
            source_project,
            repository,
            project_relative,
            common_dir,
            managed_root,
            sessions: Mutex::new(BTreeMap::new()),
        })
    }

    fn session_path(&self, session_id: SessionId) -> PathBuf {
        self.managed_root
            .join(&self.project_hash)
            .join(sha256_hex(session_id.to_string().as_bytes()))
    }

    fn state_file(&self, session_id: SessionId) -> PathBuf {
        let session_hash = sha256_hex(session_id.to_string().as_bytes());
        self.managed_root
            .join(&self.project_hash)
            .join(format!("{session_hash}.json"))
    }

    fn open_session(&self, project_id: ProjectId, session_id: SessionId) -> Result<PathBuf> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("Git worktree state lock poisoned");
        if let Some(state) = sessions.get(&session_id) {
            self.verify_identity(state, project_id, session_id)?;
            self.verify_worktree(state)?;
            return Ok(state.path.join(&self.project_relative));
        }

        let path = self.session_path(session_id);
        let state_file = self.state_file(session_id);
        let state = if state_file.exists() {
            let bytes = fs::read(&state_file).map_err(|error| {
                Error::Workdir(format!("could not read {}: {error}", state_file.display()))
            })?;
            let mut state: ManagedState = serde_json::from_slice(&bytes).map_err(|error| {
                Error::Workdir(format!(
                    "invalid managed worktree state {}: {error}",
                    state_file.display()
                ))
            })?;
            self.verify_persisted_identity(&state, session_id)?;
            self.verify_worktree(&state)?;
            state.project_id = project_id;
            write_state(&state)?;
            state
        } else {
            if path.exists() {
                return Err(Error::Workdir(format!(
                    "refusing to use untracked managed worktree path {}",
                    path.display()
                )));
            }
            let base_head = head(&self.repository)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    Error::Workdir(format!("could not create {}: {error}", parent.display()))
                })?;
            }
            let output = git(
                &self.repository,
                [
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    OsStr::new("--detach"),
                    path.as_os_str(),
                    OsStr::new(&base_head),
                ],
            )?;
            ensure_success("create detached Git worktree", &output)?;
            let path = fs::canonicalize(&path).map_err(|error| {
                Error::Workdir(format!("could not resolve managed worktree: {error}"))
            })?;
            let state = ManagedState {
                project_id,
                session_id,
                source_project: self.source_project.clone(),
                repository: self.repository.clone(),
                common_dir: self.common_dir.clone(),
                path,
                state_file,
                base_head: base_head.clone(),
                expected_head: base_head,
                checkpoints: BTreeMap::new(),
                pending: None,
            };
            if let Err(error) = write_state(&state) {
                let _ = git(
                    &self.repository,
                    [
                        OsStr::new("worktree"),
                        OsStr::new("remove"),
                        OsStr::new("--force"),
                        state.path.as_os_str(),
                    ],
                );
                return Err(error);
            }
            state
        };
        let root = state.path.join(&self.project_relative);
        if !root.is_dir() {
            return Err(Error::Workdir(format!(
                "project path {} is missing from managed worktree",
                root.display()
            )));
        }
        sessions.insert(session_id, state);
        Ok(root)
    }

    fn verify_persisted_identity(&self, state: &ManagedState, session_id: SessionId) -> Result<()> {
        if state.session_id != session_id
            || state.source_project != self.source_project
            || state.repository != self.repository
            || state.common_dir != self.common_dir
            || state.path != self.session_path(session_id)
            || state.state_file != self.state_file(session_id)
        {
            return Err(Error::Workdir(
                "managed worktree state does not match this project/session".into(),
            ));
        }
        Ok(())
    }

    fn verify_identity(
        &self,
        state: &ManagedState,
        project_id: ProjectId,
        session_id: SessionId,
    ) -> Result<()> {
        self.verify_persisted_identity(state, session_id)?;
        if state.project_id != project_id {
            return Err(Error::Workdir(
                "managed worktree belongs to another runtime project".into(),
            ));
        }
        Ok(())
    }

    fn verify_worktree(&self, state: &ManagedState) -> Result<()> {
        if !state.path.is_dir() {
            return Err(Error::Workdir(format!(
                "managed worktree is missing: {}",
                state.path.display()
            )));
        }
        let root = fs::canonicalize(
            git_stdout(
                &state.path,
                [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
            )?
            .trim(),
        )
        .map_err(|error| Error::Workdir(format!("could not resolve worktree root: {error}")))?;
        let common_dir = canonical_git_path(&state.path, "--git-common-dir")?;
        if root != state.path || common_dir != self.common_dir {
            return Err(Error::Workdir(
                "managed path is not the expected repository worktree".into(),
            ));
        }
        Ok(())
    }

    fn with_state<T>(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        operation: impl FnOnce(&mut ManagedState) -> Result<T>,
    ) -> Result<T> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("Git worktree state lock poisoned");
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| Error::Workdir("session has no managed Git worktree".into()))?;
        self.verify_identity(state, project_id, session_id)?;
        self.verify_worktree(state)?;
        operation(state)
    }

    fn require_resolved_clean(&self, state: &ManagedState) -> Result<()> {
        if let Some(pending) = &state.pending {
            return Err(Error::Workdir(format!(
                "worktree has unresolved tool call {}; discard or recover it explicitly",
                pending.call_id
            )));
        }
        let actual = head(&state.path)?;
        if actual != state.expected_head {
            return Err(Error::Workdir(format!(
                "managed worktree HEAD changed unexpectedly (expected {}, actual {actual})",
                state.expected_head
            )));
        }
        if !status(&state.path)?.is_empty() {
            return Err(Error::Workdir(
                "managed worktree is dirty outside a recorded tool operation".into(),
            ));
        }
        Ok(())
    }

    fn checkpoint(&self, context: &HookContext, message_id: MessageId) -> Result<()> {
        self.with_state(context.project_id, context.session_id, |state| {
            self.require_resolved_clean(state)?;
            state
                .checkpoints
                .insert(message_id, state.expected_head.clone());
            write_state(state)
        })
    }

    fn before_tool(&self, context: &ToolExecutionContext) -> Result<()> {
        self.with_state(
            context.hook_context.project_id,
            context.hook_context.session_id,
            |state| {
                self.require_resolved_clean(state)?;
                state.pending = Some(PendingTool {
                    call_id: context.call.id.to_string(),
                    expected_head: state.expected_head.clone(),
                });
                write_state(state)
            },
        )
    }

    fn after_tool(&self, context: &ToolExecutionContext) -> Result<()> {
        self.with_state(
            context.hook_context.project_id,
            context.hook_context.session_id,
            |state| {
                let pending = state.pending.as_ref().ok_or_else(|| {
                    Error::Workdir("tool operation has no matching pending record".into())
                })?;
                if pending.call_id != context.call.id.to_string() {
                    return Err(Error::Workdir(format!(
                        "pending tool call {} does not match {}",
                        pending.call_id, context.call.id
                    )));
                }
                let actual = head(&state.path)?;
                if actual != pending.expected_head {
                    return Err(Error::Workdir(format!(
                        "tool changed managed worktree HEAD (expected {}, actual {actual})",
                        pending.expected_head
                    )));
                }
                if has_unmerged_entries(&state.path)? {
                    return Err(Error::Workdir(
                        "tool left unresolved Git index entries; worktree is blocked".into(),
                    ));
                }
                ensure_success(
                    "stage tool changes",
                    &git(&state.path, [OsStr::new("add"), OsStr::new("-A")])?,
                )?;
                let message = format!(
                    "Airicode tool: {}\n\nAiricode-Session: {}\nAiricode-Turn: {}\nAiricode-Assistant-Message: {}\nAiricode-Tool-Call: {}",
                    context.call.name,
                    context.hook_context.session_id,
                    context.turn_id,
                    context.assistant_message_id,
                    context.call.id
                );
                ensure_success(
                    "commit tool changes",
                    &git(
                        &state.path,
                        [
                            OsStr::new("commit"),
                            OsStr::new("--allow-empty"),
                            OsStr::new("--no-verify"),
                            OsStr::new("-m"),
                            OsStr::new(&message),
                        ],
                    )?,
                )?;
                state.expected_head = head(&state.path)?;
                state.pending = None;
                write_state(state)
            },
        )
    }

    fn apply(&self, context: &CommandContext) -> Result<CommandResult> {
        let session_id = context.hook_context.session_id;
        let result = self.with_state(context.hook_context.project_id, session_id, |state| {
            self.require_resolved_clean(state)?;
            if head(&self.repository)? != state.base_head {
                return Err(Error::Workdir(
                    "refusing to apply: source HEAD moved since the session began".into(),
                ));
            }
            if !status(&self.repository)?.is_empty() {
                return Err(Error::Workdir(
                    "refusing to apply: source worktree or index is dirty".into(),
                ));
            }
            let tree = git_stdout(
                &state.path,
                [OsStr::new("rev-parse"), OsStr::new("HEAD^{tree}")],
            )?;
            let message = format!("Apply Airicode session {}", state.session_id);
            let commit = git_stdout(
                &self.repository,
                [
                    OsStr::new("commit-tree"),
                    OsStr::new(tree.trim()),
                    OsStr::new("-p"),
                    OsStr::new(&state.base_head),
                    OsStr::new("-m"),
                    OsStr::new(&message),
                ],
            )?;
            // merge --ff-only checks the source index/worktree again and refuses collisions.
            ensure_success(
                "fast-forward source worktree",
                &git(
                    &self.repository,
                    [
                        OsStr::new("merge"),
                        OsStr::new("--ff-only"),
                        OsStr::new("--no-edit"),
                        OsStr::new(commit.trim()),
                    ],
                )?,
            )?;
            self.remove_worktree(state)?;
            Ok(CommandResult {
                content: format!("Applied session as squash commit {}", commit.trim()),
            })
        })?;
        self.remove_session_state(session_id)?;
        Ok(result)
    }

    fn discard(&self, context: &CommandContext) -> Result<CommandResult> {
        let session_id = context.hook_context.session_id;
        let result = self.with_state(context.hook_context.project_id, session_id, |state| {
            self.remove_worktree(state)?;
            Ok(CommandResult {
                content: format!("Discarded managed worktree {}", state.path.display()),
            })
        })?;
        self.remove_session_state(session_id)?;
        Ok(result)
    }

    fn remove_worktree(&self, state: &ManagedState) -> Result<()> {
        let expected = self.session_path(state.session_id);
        let managed_root = fs::canonicalize(&self.managed_root)
            .map_err(|error| Error::Workdir(format!("could not resolve managed root: {error}")))?;
        let path = fs::canonicalize(&state.path).map_err(|error| {
            Error::Workdir(format!("could not resolve managed worktree: {error}"))
        })?;
        if path != expected || !path.starts_with(&managed_root) {
            return Err(Error::Workdir(
                "refusing to remove a path outside the managed session root".into(),
            ));
        }
        self.verify_worktree(state)?;
        ensure_success(
            "remove managed Git worktree",
            &git(
                &self.repository,
                [
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    OsStr::new("--force"),
                    path.as_os_str(),
                ],
            )?,
        )
    }

    fn remove_session_state(&self, session_id: SessionId) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("Git worktree state lock poisoned");
        let state = sessions
            .remove(&session_id)
            .ok_or_else(|| Error::Workdir("session has no managed Git worktree".into()))?;
        fs::remove_file(&state.state_file).map_err(|error| {
            Error::Workdir(format!(
                "could not remove managed state {}: {error}",
                state.state_file.display()
            ))
        })?;
        if let Some(parent) = state.state_file.parent() {
            let _ = fs::remove_dir(parent);
        }
        Ok(())
    }

    async fn revert(
        &self,
        message_id: MessageId,
        context: &CommandContext,
    ) -> Result<CommandResult> {
        let history = context.history();
        let snapshot = history.snapshot().await?;
        let index = snapshot
            .messages
            .iter()
            .position(|message| message.id == message_id)
            .ok_or(Error::MessageNotFound(message_id))?;

        let commit = self.with_state(
            context.hook_context.project_id,
            context.hook_context.session_id,
            |state| {
                self.require_resolved_clean(state)?;
                let checkpoint = state.checkpoints.get(&message_id).cloned().ok_or_else(|| {
                    Error::Workdir(format!("message {message_id} has no worktree checkpoint"))
                })?;
                ensure_success(
                    "remove non-ignored untracked files",
                    &git(&state.path, [OsStr::new("clean"), OsStr::new("-fd")])?,
                )?;
                ensure_success(
                    "restore checkpoint tree",
                    &git(
                        &state.path,
                        [
                            OsStr::new("restore"),
                            OsStr::new("--source"),
                            OsStr::new(&checkpoint),
                            OsStr::new("--staged"),
                            OsStr::new("--worktree"),
                            OsStr::new("--"),
                            OsStr::new("."),
                        ],
                    )?,
                )?;
                ensure_success(
                    "commit checkpoint restoration",
                    &git(
                        &state.path,
                        [
                            OsStr::new("commit"),
                            OsStr::new("--allow-empty"),
                            OsStr::new("--no-verify"),
                            OsStr::new("-m"),
                            OsStr::new(&format!(
                                "Revert Airicode worktree to message {message_id}"
                            )),
                        ],
                    )?,
                )?;
                state.expected_head = head(&state.path)?;
                write_state(state)?;
                Ok(state.expected_head.clone())
            },
        )?;

        if index == 0 {
            let last = snapshot
                .messages
                .last()
                .expect("target message proves history is non-empty")
                .id;
            history
                .replace_range(snapshot.revision, message_id, last, Vec::new())
                .await?;
        } else {
            history
                .truncate_after(snapshot.revision, snapshot.messages[index - 1].id)
                .await?;
        }
        Ok(CommandResult {
            content: format!("Restored checkpoint for {message_id} as audit commit {commit}"),
        })
    }
}

impl WorkdirLayer for WorktreeService {
    fn id(&self) -> WorkdirLayerId {
        WorkdirLayerId::new(LAYER_ID)
    }

    fn layer(&self, context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        let Some(session_id) = context.session_id else {
            return inner;
        };
        match self.open_session(context.project_id, session_id) {
            Ok(path) => match NativeWorkdir::new(path) {
                Ok(workdir) => Arc::new(workdir),
                Err(error) => Arc::new(FailedWorkdir::new(inner.root(), error)),
            },
            Err(error) => Arc::new(FailedWorkdir::new(inner.root(), error)),
        }
    }
}

#[async_trait]
impl BeforeUserMessageHook for WorktreeService {
    async fn before_user_message(
        &self,
        context: &HookContext,
        message: &mut Message,
    ) -> Result<BeforeHookResult> {
        self.checkpoint(context, message.id)?;
        Ok(BeforeHookResult::Continue)
    }
}

#[async_trait]
impl BeforeToolExecutionHook for WorktreeService {
    async fn before_tool_execution(
        &self,
        context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult> {
        self.before_tool(context)?;
        Ok(BeforeHookResult::Continue)
    }
}

#[async_trait]
impl AfterToolExecutionHook for WorktreeService {
    async fn after_tool_execution(
        &self,
        context: &ToolExecutionContext,
        _output: &mut ToolOutput,
    ) -> Result<()> {
        self.after_tool(context)
    }
}

#[derive(Clone, Copy)]
enum CommandKind {
    Apply,
    Discard,
    Revert,
}

struct WorktreeCommand {
    kind: CommandKind,
    service: Arc<WorktreeService>,
}

impl WorktreeCommand {
    fn new(kind: CommandKind, service: Arc<WorktreeService>) -> Self {
        Self { kind, service }
    }
}

#[async_trait]
impl Command for WorktreeCommand {
    fn id(&self) -> CommandId {
        CommandId::new(match self.kind {
            CommandKind::Apply => APPLY_COMMAND_ID,
            CommandKind::Discard => DISCARD_COMMAND_ID,
            CommandKind::Revert => REVERT_COMMAND_ID,
        })
    }

    fn descriptor(&self) -> CommandDescriptor {
        match self.kind {
            CommandKind::Apply => CommandDescriptor {
                name: "apply-worktree".into(),
                description: "Squash and fast-forward this session into the clean source project."
                    .into(),
                usage: "/apply-worktree".into(),
            },
            CommandKind::Discard => CommandDescriptor {
                name: "discard-worktree".into(),
                description: "Explicitly discard this session's managed worktree.".into(),
                usage: "/discard-worktree".into(),
            },
            CommandKind::Revert => CommandDescriptor {
                name: "revert-worktree".into(),
                description:
                    "Restore the checkpoint before a user message and truncate later history."
                        .into(),
                usage: "/revert-worktree <message-id>".into(),
            },
        }
    }

    async fn execute(
        &self,
        invocation: CommandInvocation,
        context: CommandContext,
    ) -> Result<CommandResult> {
        match self.kind {
            CommandKind::Apply => {
                require_no_arguments(&invocation)?;
                self.service.apply(&context)
            }
            CommandKind::Discard => {
                require_no_arguments(&invocation)?;
                self.service.discard(&context)
            }
            CommandKind::Revert => {
                let id = serde_json::from_value::<MessageId>(serde_json::Value::String(
                    invocation.arguments.trim().to_owned(),
                ))
                .map_err(|_| Error::Workdir("usage: /revert-worktree <message-id>".into()))?;
                self.service.revert(id, &context).await
            }
        }
    }
}

fn require_no_arguments(invocation: &CommandInvocation) -> Result<()> {
    if invocation.arguments.trim().is_empty() {
        Ok(())
    } else {
        Err(Error::Workdir(format!(
            "/{} does not accept arguments",
            invocation.name
        )))
    }
}

struct FailedWorkdir {
    root: PathBuf,
    error: String,
}

impl FailedWorkdir {
    fn new(root: &Path, error: Error) -> Self {
        Self {
            root: root.to_path_buf(),
            error: error.to_string(),
        }
    }

    fn failure(&self) -> Error {
        Error::Workdir(self.error.clone())
    }
}

#[async_trait]
impl Workdir for FailedWorkdir {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn read(&self, _path: &Path) -> Result<Vec<u8>> {
        Err(self.failure())
    }

    async fn write(&self, _path: &Path, _data: &[u8]) -> Result<()> {
        Err(self.failure())
    }

    async fn remove(&self, _path: &Path) -> Result<()> {
        Err(self.failure())
    }

    async fn execute(
        &self,
        _command: CommandSpec,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<CommandOutput> {
        Err(self.failure())
    }
}

fn write_state(state: &ManagedState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| Error::Workdir(format!("could not encode worktree state: {error}")))?;
    let temporary = state.state_file.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|error| {
        Error::Workdir(format!("could not write {}: {error}", temporary.display()))
    })?;
    fs::rename(&temporary, &state.state_file).map_err(|error| {
        Error::Workdir(format!(
            "could not install state {}: {error}",
            state.state_file.display()
        ))
    })
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn canonical_git_path(cwd: &Path, argument: &str) -> Result<PathBuf> {
    let value = git_stdout(cwd, [OsStr::new("rev-parse"), OsStr::new(argument)])?;
    let path = PathBuf::from(value.trim());
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    fs::canonicalize(path)
        .map_err(|error| Error::Workdir(format!("could not resolve Git metadata: {error}")))
}

fn head(cwd: &Path) -> Result<String> {
    Ok(
        git_stdout(cwd, [OsStr::new("rev-parse"), OsStr::new("HEAD")])?
            .trim()
            .to_owned(),
    )
}

fn status(cwd: &Path) -> Result<Vec<u8>> {
    let output = git(
        cwd,
        [
            OsStr::new("status"),
            OsStr::new("--porcelain=v1"),
            OsStr::new("--untracked-files=normal"),
        ],
    )?;
    ensure_success("inspect Git status", &output)?;
    Ok(output.stdout)
}

fn has_unmerged_entries(cwd: &Path) -> Result<bool> {
    let output = git(
        cwd,
        [
            OsStr::new("diff"),
            OsStr::new("--name-only"),
            OsStr::new("--diff-filter=U"),
        ],
    )?;
    ensure_success("inspect Git index conflicts", &output)?;
    Ok(!output.stdout.is_empty())
}

fn git_stdout<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(cwd, args)?;
    ensure_success("run Git command", &output)?;
    String::from_utf8(output.stdout)
        .map_err(|error| Error::Workdir(format!("Git returned non-UTF-8 output: {error}")))
}

fn git<I, S>(cwd: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = ProcessCommand::new("git");
    command
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("commit.gpgSign=false")
        .arg("-c")
        .arg("tag.gpgSign=false");
    command.args(args).current_dir(cwd);
    git_environment(&mut command);
    command
        .output()
        .map_err(|error| Error::Workdir(format!("could not run git: {error}")))
}

fn git_environment(command: &mut ProcessCommand) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "Airicode")
        .env("GIT_AUTHOR_EMAIL", "airicode@localhost")
        .env("GIT_COMMITTER_NAME", "Airicode")
        .env("GIT_COMMITTER_EMAIL", "airicode@localhost")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

fn ensure_success(action: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(Error::Workdir(format!(
        "could not {action}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{Core, OpenSession, ProviderId, Role, ToolCallDraft, ToolCallId, TurnId},
        testkit::{stub_provider_plugin, StubProvider},
    };

    fn run(repo: &Path, args: &[&str]) -> String {
        let mut command = ProcessCommand::new("git");
        command.args(args).current_dir(repo);
        git_environment(&mut command);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn repository() -> tempfile::TempDir {
        let repository = tempfile::tempdir().unwrap();
        run(repository.path(), &["init", "-q"]);
        fs::write(repository.path().join("tracked.txt"), b"initial").unwrap();
        fs::write(repository.path().join(".gitignore"), b"ignored.log\n").unwrap();
        run(repository.path(), &["add", "tracked.txt", ".gitignore"]);
        run(repository.path(), &["commit", "-qm", "initial"]);
        repository
    }

    fn service(repository: &Path, data: &Path) -> WorktreeService {
        WorktreeService::new(&GitWorktreeConfig::new(repository, data)).unwrap()
    }

    #[test]
    fn paths_use_exact_canonical_project_and_session_hashes() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let session = SessionId::new();
        let project_hash = sha256_hex(
            fs::canonicalize(repository.path())
                .unwrap()
                .as_os_str()
                .as_encoded_bytes(),
        );
        let session_hash = sha256_hex(session.to_string().as_bytes());
        assert_eq!(
            service.session_path(session),
            fs::canonicalize(data.path())
                .unwrap()
                .join("worktrees")
                .join(project_hash)
                .join(session_hash)
        );
    }

    #[test]
    fn separate_sessions_are_isolated_and_source_is_untouched() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let project = ProjectId::new();
        let first = SessionId::new();
        let second = SessionId::new();
        let first_root = service.open_session(project, first).unwrap();
        let second_root = service.open_session(project, second).unwrap();
        fs::write(first_root.join("tracked.txt"), b"first").unwrap();

        assert_ne!(first_root, second_root);
        assert_eq!(
            fs::read(second_root.join("tracked.txt")).unwrap(),
            b"initial"
        );
        assert_eq!(
            fs::read(repository.path().join("tracked.txt")).unwrap(),
            b"initial"
        );
        service
            .discard(&command_context(project, first, &first_root))
            .unwrap();
        service
            .discard(&command_context(project, second, &second_root))
            .unwrap();
    }

    #[test]
    fn session_safely_reopens_with_a_new_runtime_project_id() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let session = SessionId::new();
        let original_root = {
            let service = service(repository.path(), data.path());
            service.open_session(ProjectId::new(), session).unwrap()
        };

        let reopened = service(repository.path(), data.path());
        let project = ProjectId::new();
        let reopened_root = reopened.open_session(project, session).unwrap();
        assert_eq!(reopened_root, original_root);
        assert_eq!(
            fs::read(reopened_root.join("tracked.txt")).unwrap(),
            b"initial"
        );
        reopened
            .discard(&command_context(project, session, &reopened_root))
            .unwrap();
    }

    #[test]
    fn no_session_layer_is_a_noop() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let host: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(repository.path()).unwrap());
        let layered = service.layer(
            &WorkdirLayerContext {
                project_id: ProjectId::new(),
                project_name: "test".into(),
                session_id: None,
            },
            host.clone(),
        );
        assert!(Arc::ptr_eq(&host, &layered));
    }

    #[tokio::test]
    async fn plugin_registers_commands_not_tools_and_can_be_omitted() {
        let plain = Core::new().build().await.unwrap();
        assert!(plain.workdir_layers().ids().is_empty());
        assert!(plain.commands().ids().is_empty());

        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let core = Core::new()
            .with_plugin(git_worktree_plugin(GitWorktreeConfig::new(
                repository.path(),
                data.path(),
            )))
            .build()
            .await
            .unwrap();
        assert_eq!(
            core.workdir_layers().ids(),
            vec![WorkdirLayerId::new(LAYER_ID)]
        );
        assert!(core.tools().ids().is_empty());
        assert_eq!(
            core.commands()
                .descriptors()
                .into_iter()
                .map(|descriptor| descriptor.name)
                .collect::<Vec<_>>(),
            vec!["apply-worktree", "discard-worktree", "revert-worktree"]
        );
    }

    #[test]
    fn every_completed_tool_call_creates_one_commit_including_noop() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let project = ProjectId::new();
        let session = SessionId::new();
        let root = service.open_session(project, session).unwrap();
        let before = run(&root, &["rev-list", "--count", "HEAD"])
            .parse::<usize>()
            .unwrap();

        let first = tool_context(project, session, &root, "call-1");
        service.before_tool(&first).unwrap();
        service.after_tool(&first).unwrap();
        let second = tool_context(project, session, &root, "call-2");
        service.before_tool(&second).unwrap();
        fs::write(root.join("tracked.txt"), b"changed").unwrap();
        fs::write(root.join("ignored.log"), b"ignored output").unwrap();
        service.after_tool(&second).unwrap();

        assert_eq!(
            run(&root, &["rev-list", "--count", "HEAD"])
                .parse::<usize>()
                .unwrap(),
            before + 2
        );
        assert!(run(&root, &["show", "-s", "--format=%B", "HEAD"])
            .contains("Airicode-Tool-Call: call-2"));
        assert!(run(&root, &["ls-files", "ignored.log"]).is_empty());
        service
            .discard(&command_context(project, session, &root))
            .unwrap();
    }

    #[test]
    fn apply_squashes_session_and_fast_forwards_clean_source() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let project = ProjectId::new();
        let session = SessionId::new();
        let root = service.open_session(project, session).unwrap();
        let base = run(repository.path(), &["rev-parse", "HEAD"]);

        for (call, contents) in [
            ("call-1", b"first".as_slice()),
            ("call-2", b"final".as_slice()),
        ] {
            let context = tool_context(project, session, &root, call);
            service.before_tool(&context).unwrap();
            fs::write(root.join("tracked.txt"), contents).unwrap();
            service.after_tool(&context).unwrap();
        }
        assert_eq!(
            fs::read(repository.path().join("tracked.txt")).unwrap(),
            b"initial"
        );

        service
            .apply(&command_context(project, session, &root))
            .unwrap();
        assert_eq!(
            fs::read(repository.path().join("tracked.txt")).unwrap(),
            b"final"
        );
        assert_eq!(run(repository.path(), &["rev-parse", "HEAD^"]), base);
        assert_eq!(
            run(
                repository.path(),
                &["rev-list", "--count", "HEAD", "^HEAD^"]
            ),
            "1"
        );
        assert!(!service.session_path(session).exists());
        assert!(!service.state_file(session).exists());
    }

    #[test]
    fn apply_refuses_dirty_source_and_discard_remains_explicit() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let project = ProjectId::new();
        let session = SessionId::new();
        let root = service.open_session(project, session).unwrap();
        fs::write(repository.path().join("unrelated.txt"), b"source data").unwrap();

        let error = service
            .apply(&command_context(project, session, &root))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("source worktree or index is dirty"));
        assert!(root.exists());
        assert_eq!(
            fs::read(repository.path().join("unrelated.txt")).unwrap(),
            b"source data"
        );

        service
            .discard(&command_context(project, session, &root))
            .unwrap();
        assert!(!root.exists());
        assert!(repository.path().join("unrelated.txt").exists());
    }

    #[test]
    fn interrupted_or_out_of_band_dirty_operation_fails_closed() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = service(repository.path(), data.path());
        let project = ProjectId::new();
        let session = SessionId::new();
        let root = service.open_session(project, session).unwrap();
        let context = tool_context(project, session, &root, "cancelled-call");
        service.before_tool(&context).unwrap();

        let next = tool_context(project, session, &root, "next-call");
        assert!(service
            .before_tool(&next)
            .unwrap_err()
            .to_string()
            .contains("unresolved tool call"));
        service
            .discard(&command_context(project, session, &root))
            .unwrap();

        let dirty_session = SessionId::new();
        let dirty_root = service.open_session(project, dirty_session).unwrap();
        fs::write(dirty_root.join("tracked.txt"), b"out of band").unwrap();
        assert!(service
            .before_tool(&tool_context(
                project,
                dirty_session,
                &dirty_root,
                "dirty-call"
            ))
            .unwrap_err()
            .to_string()
            .contains("dirty outside"));
        service
            .discard(&command_context(project, dirty_session, &dirty_root))
            .unwrap();
    }

    #[tokio::test]
    async fn checkpoint_revert_restores_tree_and_truncates_later_history() {
        let repository = repository();
        let data = tempfile::tempdir().unwrap();
        let service = Arc::new(service(repository.path(), data.path()));
        let core = Core::new()
            .with_plugin(stub_provider_plugin(StubProvider::responding(
                "stub", "response",
            )))
            .with_plugin(Arc::new(TestWorktreePlugin(service.clone())))
            .build()
            .await
            .unwrap();
        let project = core.open_project(
            "project",
            Arc::new(NativeWorkdir::new(repository.path()).unwrap()),
        );
        let session_id = SessionId::new();
        let session = project
            .open_session(OpenSession {
                id: Some(session_id),
                provider: ProviderId::new("stub"),
                model: "test".into(),
            })
            .await
            .unwrap();
        let mut snapshots = session.subscribe();
        let first = Message::text(Role::User, "first");
        session.send_message(first).await.unwrap();
        wait_for_messages(&mut snapshots, 2).await;

        let root = service.session_path(session_id);
        let first_tool = tool_context(project.id(), session_id, &root, "first-change");
        service.before_tool(&first_tool).unwrap();
        fs::write(root.join("tracked.txt"), b"first change").unwrap();
        service.after_tool(&first_tool).unwrap();

        let second = Message::text(Role::User, "second");
        let second_id = second.id;
        session.send_message(second).await.unwrap();
        wait_for_messages(&mut snapshots, 4).await;
        let second_tool = tool_context(project.id(), session_id, &root, "second-change");
        service.before_tool(&second_tool).unwrap();
        fs::write(root.join("tracked.txt"), b"second change").unwrap();
        service.after_tool(&second_tool).unwrap();

        session
            .dispatch_command(CommandInvocation {
                name: "revert-worktree".into(),
                arguments: second_id.to_string(),
            })
            .await
            .unwrap();
        assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"first change");
        assert_eq!(session.snapshot().messages.len(), 2);
        assert_eq!(
            run(&root, &["show", "-s", "--format=%s", "HEAD"]),
            format!("Revert Airicode worktree to message {second_id}")
        );
        service
            .discard(&command_context(project.id(), session_id, &root))
            .unwrap();
    }

    struct TestWorktreePlugin(Arc<WorktreeService>);

    #[async_trait]
    impl Plugin for TestWorktreePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("test.git-worktree")
        }

        async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
            registrar.register_workdir_layer(100, self.0.clone())?;
            registrar.register_before_user_message(USER_HOOK_ID, 100, self.0.clone())?;
            registrar.register_before_tool_execution(BEFORE_TOOL_HOOK_ID, 100, self.0.clone())?;
            registrar.register_after_tool_execution(AFTER_TOOL_HOOK_ID, -100, self.0.clone())?;
            registrar.register_command(
                0,
                Arc::new(WorktreeCommand::new(CommandKind::Revert, self.0.clone())),
            )
        }
    }

    async fn wait_for_messages(
        snapshots: &mut tokio::sync::watch::Receiver<crate::core::SessionSnapshot>,
        count: usize,
    ) {
        while snapshots.borrow().messages.len() < count || snapshots.borrow().active_turn.is_some()
        {
            snapshots.changed().await.unwrap();
        }
    }

    fn command_context(
        project_id: ProjectId,
        session_id: SessionId,
        root: &Path,
    ) -> CommandContext {
        CommandContext {
            hook_context: HookContext {
                project_id,
                session_id,
            },
            workdir: Arc::new(NativeWorkdir::new(root).unwrap()),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn tool_context(
        project_id: ProjectId,
        session_id: SessionId,
        root: &Path,
        call_id: &str,
    ) -> ToolExecutionContext {
        ToolExecutionContext {
            hook_context: HookContext {
                project_id,
                session_id,
            },
            turn_id: TurnId::new(),
            assistant_message_id: MessageId::new(),
            workdir: Arc::new(NativeWorkdir::new(root).unwrap()),
            call: ToolCallDraft {
                id: ToolCallId::new(call_id),
                name: "test_tool".into(),
                arguments: serde_json::json!({}),
            },
        }
    }
}
