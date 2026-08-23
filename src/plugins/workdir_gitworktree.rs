use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::core::{
    CommandOutput, CommandSpec, Error, NativeWorkdir, Plugin, PluginId, PluginRegistrar, ProjectId,
    Result, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput, Workdir, WorkdirLayer,
    WorkdirLayerContext, WorkdirLayerId,
};

const PLUGIN_ID: &str = "builtin.git-worktree";
const LAYER_ID: &str = "builtin.git-worktree";
const CLEANUP_TOOL_ID: &str = "builtin.git-worktree.cleanup";

#[derive(Clone, Debug)]
pub struct GitWorktreeConfig {
    pub repository: PathBuf,
    pub worktree_dir: PathBuf,
    pub revision: String,
}

impl GitWorktreeConfig {
    pub fn new(repository: impl Into<PathBuf>, worktree_dir: impl Into<PathBuf>) -> Self {
        Self {
            repository: repository.into(),
            worktree_dir: worktree_dir.into(),
            revision: "HEAD".into(),
        }
    }

    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = revision.into();
        self
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
        if self.config.revision.is_empty() {
            return Err(Error::Plugin(
                "Git worktree revision must not be empty".into(),
            ));
        }
        let repository = fs::canonicalize(&self.config.repository).map_err(|error| {
            Error::Plugin(format!(
                "could not resolve Git repository {}: {error}",
                self.config.repository.display()
            ))
        })?;
        let worktree_dir = if self.config.worktree_dir.is_absolute() {
            self.config.worktree_dir.clone()
        } else {
            repository.join(&self.config.worktree_dir)
        };
        let state = Arc::new(Mutex::new(BTreeMap::new()));
        let layer = Arc::new(GitWorktreeLayer {
            repository,
            worktree_dir,
            revision: self.config.revision.clone(),
            state: state.clone(),
        });
        registrar.register_workdir_layer(0, layer)?;
        registrar.register_tool(0, Arc::new(CleanupTool { state }))
    }
}

struct GitWorktreeLayer {
    repository: PathBuf,
    worktree_dir: PathBuf,
    revision: String,
    state: Arc<Mutex<BTreeMap<ProjectId, Arc<GitWorktree>>>>,
}

impl WorkdirLayer for GitWorktreeLayer {
    fn id(&self) -> WorkdirLayerId {
        WorkdirLayerId::new(LAYER_ID)
    }

    fn layer(&self, context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        let path = self.worktree_dir.join(context.project_id.to_string());
        match GitWorktree::create(&self.repository, &path, &self.revision) {
            Ok(worktree) => {
                let worktree = Arc::new(worktree);
                self.state
                    .lock()
                    .expect("Git worktree state lock poisoned")
                    .insert(context.project_id, worktree.clone());
                worktree
            }
            Err(error) => Arc::new(FailedWorkdir {
                root: inner.root().to_path_buf(),
                error: error.to_string(),
            }),
        }
    }
}

struct GitWorktree {
    repository: PathBuf,
    path: PathBuf,
    native: NativeWorkdir,
}

impl GitWorktree {
    fn create(repository: &Path, path: &Path, revision: &str) -> Result<Self> {
        if path.exists() {
            return Err(Error::Workdir(format!(
                "Git worktree path already exists: {}",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::Workdir(format!(
                    "could not create Git worktree directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let output = git(
            repository,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--detach"),
                path.as_os_str(),
                OsStr::new(revision),
            ],
        )?;
        ensure_success("create Git worktree", &output)?;

        let native = NativeWorkdir::new(path)?;
        let path = fs::canonicalize(path).map_err(|error| {
            Error::Workdir(format!("could not resolve created Git worktree: {error}"))
        })?;
        Ok(Self {
            repository: repository.to_path_buf(),
            path,
            native,
        })
    }

    /// Removes only a verified-clean worktree. No implicit or forced cleanup is performed.
    fn cleanup(&self) -> Result<()> {
        let status = git(
            &self.path,
            [
                OsStr::new("status"),
                OsStr::new("--porcelain"),
                OsStr::new("--untracked-files=normal"),
                OsStr::new("--ignored"),
            ],
        )?;
        ensure_success("inspect Git worktree", &status)?;
        if !status.stdout.is_empty() {
            return Err(Error::Workdir(format!(
                "refusing to remove dirty Git worktree {}",
                self.path.display()
            )));
        }
        let output = git(
            &self.repository,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                self.path.as_os_str(),
            ],
        )?;
        ensure_success("remove Git worktree", &output)
    }
}

// There is deliberately no Drop cleanup: dropping a project must never discard work.

#[async_trait]
impl Workdir for GitWorktree {
    fn root(&self) -> &Path {
        self.native.root()
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.native.read(path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.native.write(path, data).await
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        self.native.remove(path).await
    }

    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput> {
        self.native.execute(command, cancellation).await
    }
}

struct FailedWorkdir {
    root: PathBuf,
    error: String,
}

impl FailedWorkdir {
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
        _cancellation: CancellationToken,
    ) -> Result<CommandOutput> {
        Err(self.failure())
    }
}

struct CleanupTool {
    state: Arc<Mutex<BTreeMap<ProjectId, Arc<GitWorktree>>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupInput {}

#[async_trait]
impl Tool for CleanupTool {
    fn id(&self) -> ToolId {
        ToolId::new(CLEANUP_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cleanup_git_worktree".into(),
            description: "Explicitly remove this project's detached Git worktree if it is clean."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let _: CleanupInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid Git worktree cleanup input: {error}")))?;
        let worktree = self
            .state
            .lock()
            .expect("Git worktree state lock poisoned")
            .get(&context.project_id)
            .cloned()
            .ok_or_else(|| Error::Tool("this project has no managed Git worktree".into()))?;
        worktree.cleanup()?;
        self.state
            .lock()
            .expect("Git worktree state lock poisoned")
            .remove(&context.project_id);
        Ok(ToolOutput {
            content: serde_json::json!({ "removed": worktree.path }).to_string(),
            is_error: false,
        })
    }
}

fn git<I, S>(cwd: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| Error::Workdir(format!("could not run git: {error}")))
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
    use crate::core::Core;

    fn run(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Airicode Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Airicode Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
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

    #[tokio::test]
    async fn plugin_registers_one_layer_and_cleanup_tool_and_delegates() {
        let repository = repository();
        let trees = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        let core = Core::new()
            .with_plugin(git_worktree_plugin(GitWorktreeConfig::new(
                repository.path(),
                trees.path(),
            )))
            .build()
            .await
            .unwrap();

        assert_eq!(
            core.workdir_layers().ids(),
            vec![WorkdirLayerId::new(LAYER_ID)]
        );
        assert_eq!(core.tools().ids(), vec![ToolId::new(CLEANUP_TOOL_ID)]);
        let project = core.open_project(
            "project",
            Arc::new(NativeWorkdir::new(host.path()).unwrap()),
        );
        assert_eq!(
            project
                .get_workdir()
                .read(Path::new("tracked.txt"))
                .await
                .unwrap(),
            b"initial"
        );
        assert!(project.get_workdir().root().starts_with(trees.path()));
    }

    #[test]
    fn cleanup_preserves_dirty_tree_and_drop_does_nothing() {
        let repository = repository();
        let trees = tempfile::tempdir().unwrap();
        let path = trees.path().join("dirty");
        let worktree = GitWorktree::create(repository.path(), &path, "HEAD").unwrap();
        fs::write(worktree.path.join("ignored.log"), b"user data").unwrap();

        assert!(worktree.cleanup().is_err());
        assert!(path.exists());
        drop(worktree);
        assert!(path.exists());

        run(
            repository.path(),
            &["worktree", "remove", "--force", path.to_str().unwrap()],
        );
    }

    #[test]
    fn cleanup_removes_a_clean_tree() {
        let repository = repository();
        let trees = tempfile::tempdir().unwrap();
        let path = trees.path().join("clean");
        let worktree = GitWorktree::create(repository.path(), &path, "HEAD").unwrap();
        worktree.cleanup().unwrap();
        assert!(!path.exists());
    }
}
