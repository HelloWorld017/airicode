use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::core::{
    CommandOutput, CommandSpec, Error, Plugin, PluginId, PluginRegistrar, Result, Workdir,
    WorkdirLayer, WorkdirLayerContext, WorkdirLayerId,
};

const PLUGIN_ID: &str = "builtin.sandbox-bubblewrap";
const LAYER_ID: &str = "builtin.sandbox-bubblewrap.workdir";
const SANDBOX_ROOT: &str = "/workspace";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BubblewrapFileOperation {
    Read,
    Write,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BubblewrapPathRule {
    pub operation: Option<BubblewrapFileOperation>,
    pub path: PathBuf,
    pub allow: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BubblewrapProcessRule {
    pub program: Option<String>,
    pub allow: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BubblewrapSandboxConfig {
    pub path_rules: Vec<BubblewrapPathRule>,
    pub process_rules: Vec<BubblewrapProcessRule>,
    pub writable_paths: Vec<PathBuf>,
    pub default_allow: bool,
    pub allow_network: bool,
}

pub fn bubblewrap_sandbox_plugin(config: BubblewrapSandboxConfig) -> Arc<dyn Plugin> {
    Arc::new(BubblewrapPlugin { config })
}

#[derive(Clone)]
struct BubblewrapPolicy {
    path_rules: Vec<BubblewrapPathRule>,
    process_rules: Vec<BubblewrapProcessRule>,
    writable_paths: Vec<PathBuf>,
    default_allow: bool,
    allow_network: bool,
}

impl BubblewrapPolicy {
    fn new(mut config: BubblewrapSandboxConfig) -> Result<Self> {
        for rule in &mut config.path_rules {
            rule.path = validate_relative(&rule.path, true)?;
        }
        let mut policy = Self {
            path_rules: config.path_rules,
            process_rules: config.process_rules,
            writable_paths: Vec::new(),
            default_allow: config.default_allow,
            allow_network: config.allow_network,
        };
        for path in config.writable_paths {
            let path = validate_relative(&path, true)?;
            policy.require_path_scope(BubblewrapFileOperation::Write, &path)?;
            policy.writable_paths.push(path);
        }
        Ok(policy)
    }

    fn require_path(&self, operation: BubblewrapFileOperation, path: &Path) -> Result<PathBuf> {
        self.require_path_inner(operation, path, false)
    }

    fn require_path_scope(
        &self,
        operation: BubblewrapFileOperation,
        path: &Path,
    ) -> Result<PathBuf> {
        self.require_path_inner(operation, path, true)
    }

    fn require_path_inner(
        &self,
        operation: BubblewrapFileOperation,
        path: &Path,
        allow_root: bool,
    ) -> Result<PathBuf> {
        let path = validate_relative(path, allow_root)?;
        let allowed = self
            .path_rules
            .iter()
            .find(|rule| {
                rule.operation.map_or(true, |value| value == operation)
                    && path.starts_with(&rule.path)
            })
            .map_or(self.default_allow, |rule| rule.allow);
        if allowed {
            Ok(path)
        } else {
            Err(Error::Workdir(format!(
                "bubblewrap sandbox denied {operation:?} for {}",
                path.display()
            )))
        }
    }

    fn require_process(&self, program: &str) -> Result<()> {
        let allowed = self
            .process_rules
            .iter()
            .find(|rule| {
                rule.program
                    .as_deref()
                    .map_or(true, |value| value == program)
            })
            .map_or(self.default_allow, |rule| rule.allow);
        if allowed {
            Ok(())
        } else {
            Err(Error::Workdir(format!(
                "bubblewrap sandbox denied execution of {program}"
            )))
        }
    }
}

fn validate_relative(path: &Path, allow_root: bool) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err(Error::Workdir(format!(
            "sandbox path must be project-relative: {}",
            path.display()
        )));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Workdir(format!(
                    "sandbox path may not escape the project: {}",
                    path.display()
                )));
            }
        }
    }
    if !allow_root && normalized.as_os_str().is_empty() {
        return Err(Error::Workdir("sandbox path must be non-empty".into()));
    }
    Ok(normalized)
}

struct BubblewrapPlugin {
    config: BubblewrapSandboxConfig,
}

#[async_trait]
impl Plugin for BubblewrapPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        let policy = BubblewrapPolicy::new(self.config.clone())?;
        registrar.register_workdir_layer(0, Arc::new(BubblewrapLayer { policy }))
    }
}

struct BubblewrapLayer {
    policy: BubblewrapPolicy,
}

impl WorkdirLayer for BubblewrapLayer {
    fn id(&self) -> WorkdirLayerId {
        WorkdirLayerId::new(LAYER_ID)
    }

    fn layer(&self, _context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        Arc::new(BubblewrapWorkdir {
            inner,
            policy: self.policy.clone(),
        })
    }
}

struct BubblewrapWorkdir {
    inner: Arc<dyn Workdir>,
    policy: BubblewrapPolicy,
}

impl BubblewrapWorkdir {
    fn confinement_args(&self) -> Vec<String> {
        let mut args = vec![
            "--die-with-parent".into(),
            "--new-session".into(),
            "--unshare-user".into(),
            "--unshare-pid".into(),
            "--unshare-uts".into(),
            "--unshare-ipc".into(),
            "--unshare-cgroup-try".into(),
        ];
        if !self.policy.allow_network {
            args.push("--unshare-net".into());
        }
        args
    }

    fn build_command(&self, command: CommandSpec) -> Result<CommandSpec> {
        self.policy.require_process(&command.program)?;
        let cwd = validate_relative(command.cwd.as_deref().unwrap_or(Path::new("")), true)?;
        let mut args = self.confinement_args();
        args.extend([
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--ro-bind".into(),
            self.inner.root().to_string_lossy().into_owned(),
            SANDBOX_ROOT.into(),
        ]);
        for path in &self.policy.writable_paths {
            let host = self.inner.root().join(path);
            let guest = Path::new(SANDBOX_ROOT).join(path);
            args.extend([
                "--bind".into(),
                host.to_string_lossy().into_owned(),
                guest.to_string_lossy().into_owned(),
            ]);
        }
        args.extend([
            "--chdir".into(),
            Path::new(SANDBOX_ROOT)
                .join(cwd)
                .to_string_lossy()
                .into_owned(),
        ]);
        for (key, value) in command.env {
            args.extend(["--setenv".into(), key, value]);
        }
        args.push("--".into());
        args.push(command.program);
        args.extend(command.args);
        Ok(CommandSpec {
            program: "bwrap".into(),
            args,
            cwd: None,
            env: BTreeMap::new(),
        })
    }

    fn availability_probe(&self) -> CommandSpec {
        let mut args = self.confinement_args();
        args.extend([
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--".into(),
            "true".into(),
        ]);
        CommandSpec {
            program: "bwrap".into(),
            args,
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    async fn ensure_available(&self, cancellation: CancellationToken) -> Result<()> {
        let output = self
            .inner
            .execute(self.availability_probe(), cancellation)
            .await
            .map_err(|error| Error::Workdir(format!("bubblewrap sandbox unavailable: {error}")))?;
        if output.status == 0 {
            Ok(())
        } else {
            Err(Error::Workdir(format!(
                "bubblewrap sandbox probe failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }
}

#[async_trait]
impl Workdir for BubblewrapWorkdir {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let path = self
            .policy
            .require_path(BubblewrapFileOperation::Read, path)?;
        self.inner.read(&path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let path = self
            .policy
            .require_path(BubblewrapFileOperation::Write, path)?;
        self.inner.write(&path, data).await
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        let path = self
            .policy
            .require_path(BubblewrapFileOperation::Remove, path)?;
        self.inner.remove(&path).await
    }

    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let sandboxed = self.build_command(command)?;
        self.ensure_available(cancellation.clone()).await?;
        self.inner.execute(sandboxed, cancellation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Core, NativeWorkdir};
    use std::sync::Mutex;

    struct UnavailableWorkdir {
        root: PathBuf,
        commands: Mutex<Vec<CommandSpec>>,
    }

    #[async_trait]
    impl Workdir for UnavailableWorkdir {
        fn root(&self) -> &Path {
            &self.root
        }

        async fn read(&self, _path: &Path) -> Result<Vec<u8>> {
            unreachable!()
        }

        async fn write(&self, _path: &Path, _data: &[u8]) -> Result<()> {
            unreachable!()
        }

        async fn remove(&self, _path: &Path) -> Result<()> {
            unreachable!()
        }

        async fn execute(
            &self,
            command: CommandSpec,
            _cancellation: CancellationToken,
        ) -> Result<CommandOutput> {
            self.commands.lock().unwrap().push(command);
            Err(Error::Workdir("runtime unavailable".into()))
        }
    }

    fn config() -> BubblewrapSandboxConfig {
        BubblewrapSandboxConfig {
            path_rules: vec![BubblewrapPathRule {
                operation: Some(BubblewrapFileOperation::Read),
                path: "".into(),
                allow: true,
            }],
            process_rules: vec![BubblewrapProcessRule {
                program: Some("sh".into()),
                allow: true,
            }],
            writable_paths: vec![],
            default_allow: false,
            allow_network: false,
        }
    }

    #[tokio::test]
    async fn plugin_registers_only_one_workdir_layer() {
        let omitted = Core::new().build().await.unwrap();
        assert!(omitted.workdir_layers().ids().is_empty());

        let plugin = bubblewrap_sandbox_plugin(config());
        let registrar = PluginRegistrar::new(plugin.id());
        plugin.init(registrar.clone()).await.unwrap();
        let staged = registrar.take();

        assert_eq!(staged.workdir_layers.len(), 1);
        assert!(staged.providers.is_empty());
        assert!(staged.tools.is_empty());
        assert!(staged.hooks.is_empty());
        assert!(staged.store_factories.is_empty());
    }

    #[test]
    fn constructs_readonly_networkless_command() {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let workdir = BubblewrapWorkdir {
            inner,
            policy: BubblewrapPolicy::new(config()).unwrap(),
        };
        let built = workdir
            .build_command(CommandSpec {
                program: "sh".into(),
                args: vec!["-c".into(), "pwd".into()],
                cwd: Some("src".into()),
                env: BTreeMap::from([("A".into(), "B".into())]),
            })
            .unwrap();

        assert_eq!(built.program, "bwrap");
        assert!(built.args.iter().any(|arg| arg == "--unshare-net"));
        assert!(built
            .args
            .windows(3)
            .any(|part| part == ["--ro-bind", "/", "/"]));
        assert!(built
            .args
            .windows(2)
            .any(|part| part == ["--chdir", "/workspace/src"]));
        assert_eq!(&built.args[built.args.len() - 3..], ["sh", "-c", "pwd"]);
    }

    #[tokio::test]
    async fn unavailable_runtime_fails_closed_without_native_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(UnavailableWorkdir {
            root: directory.path().into(),
            commands: Mutex::new(vec![]),
        });
        let workdir = BubblewrapWorkdir {
            inner: inner.clone(),
            policy: BubblewrapPolicy::new(config()).unwrap(),
        };

        assert!(workdir
            .execute(
                CommandSpec {
                    program: "sh".into(),
                    args: vec![],
                    cwd: None,
                    env: BTreeMap::new(),
                },
                CancellationToken::new(),
            )
            .await
            .is_err());
        let commands = inner.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "bwrap");
    }
}
