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

const PLUGIN_ID: &str = "builtin.sandbox-container";
const LAYER_ID: &str = "builtin.sandbox-container.workdir";
const SANDBOX_ROOT: &str = "/workspace";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerSandboxRuntime {
    Docker,
    Podman,
}

impl ContainerSandboxRuntime {
    fn executable(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerFileOperation {
    Read,
    Write,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerPathRule {
    pub operation: Option<ContainerFileOperation>,
    pub path: PathBuf,
    pub allow: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerProcessRule {
    pub program: Option<String>,
    pub allow: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerSandboxConfig {
    pub runtime: ContainerSandboxRuntime,
    pub image: String,
    pub path_rules: Vec<ContainerPathRule>,
    pub process_rules: Vec<ContainerProcessRule>,
    pub writable_paths: Vec<PathBuf>,
    pub default_allow: bool,
    pub allow_network: bool,
}

pub fn container_sandbox_plugin(config: ContainerSandboxConfig) -> Arc<dyn Plugin> {
    Arc::new(ContainerPlugin { config })
}

#[derive(Clone)]
struct ContainerPolicy {
    runtime: ContainerSandboxRuntime,
    image: String,
    path_rules: Vec<ContainerPathRule>,
    process_rules: Vec<ContainerProcessRule>,
    writable_paths: Vec<PathBuf>,
    default_allow: bool,
    allow_network: bool,
}

impl ContainerPolicy {
    fn new(mut config: ContainerSandboxConfig) -> Result<Self> {
        if config.image.trim().is_empty() {
            return Err(Error::Workdir(
                "container sandbox image must not be empty".into(),
            ));
        }
        for rule in &mut config.path_rules {
            rule.path = validate_relative(&rule.path, true)?;
        }
        let mut policy = Self {
            runtime: config.runtime,
            image: config.image,
            path_rules: config.path_rules,
            process_rules: config.process_rules,
            writable_paths: Vec::new(),
            default_allow: config.default_allow,
            allow_network: config.allow_network,
        };
        for path in config.writable_paths {
            let path = validate_relative(&path, true)?;
            policy.require_path_scope(ContainerFileOperation::Write, &path)?;
            policy.writable_paths.push(path);
        }
        Ok(policy)
    }

    fn require_path(&self, operation: ContainerFileOperation, path: &Path) -> Result<PathBuf> {
        self.require_path_inner(operation, path, false)
    }

    fn require_path_scope(
        &self,
        operation: ContainerFileOperation,
        path: &Path,
    ) -> Result<PathBuf> {
        self.require_path_inner(operation, path, true)
    }

    fn require_path_inner(
        &self,
        operation: ContainerFileOperation,
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
                "container sandbox denied {operation:?} for {}",
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
                "container sandbox denied execution of {program}"
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

struct ContainerPlugin {
    config: ContainerSandboxConfig,
}

#[async_trait]
impl Plugin for ContainerPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        let policy = ContainerPolicy::new(self.config.clone())?;
        registrar.register_workdir_layer(0, Arc::new(ContainerLayer { policy }))
    }
}

struct ContainerLayer {
    policy: ContainerPolicy,
}

impl WorkdirLayer for ContainerLayer {
    fn id(&self) -> WorkdirLayerId {
        WorkdirLayerId::new(LAYER_ID)
    }

    fn layer(&self, _context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        Arc::new(ContainerWorkdir {
            inner,
            policy: self.policy.clone(),
        })
    }
}

struct ContainerWorkdir {
    inner: Arc<dyn Workdir>,
    policy: ContainerPolicy,
}

impl ContainerWorkdir {
    fn build_command(&self, command: CommandSpec) -> Result<CommandSpec> {
        self.policy.require_process(&command.program)?;
        let cwd = validate_relative(command.cwd.as_deref().unwrap_or(Path::new("")), true)?;
        let root = self.inner.root().to_string_lossy();
        if root.contains(',') {
            return Err(Error::Workdir(
                "container sandbox workdir path may not contain a comma".into(),
            ));
        }
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--pull".into(),
            "never".into(),
            "--read-only".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--pids-limit".into(),
            "256".into(),
            "--user".into(),
            "65534:65534".into(),
        ];
        if !self.policy.allow_network {
            args.extend(["--network".into(), "none".into()]);
        }
        args.extend([
            "--mount".into(),
            format!("type=bind,source={root},target={SANDBOX_ROOT},readonly"),
        ]);
        for path in &self.policy.writable_paths {
            let host = self.inner.root().join(path);
            let guest = Path::new(SANDBOX_ROOT).join(path);
            let host = host.to_string_lossy();
            if host.contains(',') {
                return Err(Error::Workdir(
                    "container sandbox writable path may not contain a comma".into(),
                ));
            }
            args.extend([
                "--mount".into(),
                format!("type=bind,source={host},target={}", guest.display()),
            ]);
        }
        args.extend([
            "--workdir".into(),
            Path::new(SANDBOX_ROOT)
                .join(cwd)
                .to_string_lossy()
                .into_owned(),
        ]);
        for (key, value) in command.env {
            args.extend(["--env".into(), format!("{key}={value}")]);
        }
        args.push(self.policy.image.clone());
        args.push(command.program);
        args.extend(command.args);
        Ok(CommandSpec {
            program: self.policy.runtime.executable().into(),
            args,
            cwd: None,
            env: BTreeMap::new(),
        })
    }

    fn availability_probe(&self) -> CommandSpec {
        CommandSpec {
            program: self.policy.runtime.executable().into(),
            args: vec!["version".into()],
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    async fn ensure_available(&self, cancellation: CancellationToken) -> Result<()> {
        let output = self
            .inner
            .execute(self.availability_probe(), cancellation)
            .await
            .map_err(|error| Error::Workdir(format!("container sandbox unavailable: {error}")))?;
        if output.status == 0 {
            Ok(())
        } else {
            Err(Error::Workdir(format!(
                "container sandbox probe failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }
}

#[async_trait]
impl Workdir for ContainerWorkdir {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let path = self
            .policy
            .require_path(ContainerFileOperation::Read, path)?;
        self.inner.read(&path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let path = self
            .policy
            .require_path(ContainerFileOperation::Write, path)?;
        self.inner.write(&path, data).await
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        let path = self
            .policy
            .require_path(ContainerFileOperation::Remove, path)?;
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

    fn config(runtime: ContainerSandboxRuntime) -> ContainerSandboxConfig {
        ContainerSandboxConfig {
            runtime,
            image: "rust:latest".into(),
            path_rules: vec![],
            process_rules: vec![ContainerProcessRule {
                program: Some("cargo".into()),
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

        let plugin = container_sandbox_plugin(config(ContainerSandboxRuntime::Docker));
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
    fn constructs_locked_down_docker_command() {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let workdir = ContainerWorkdir {
            inner,
            policy: ContainerPolicy::new(config(ContainerSandboxRuntime::Docker)).unwrap(),
        };
        let built = workdir
            .build_command(CommandSpec {
                program: "cargo".into(),
                args: vec!["test".into()],
                cwd: None,
                env: BTreeMap::new(),
            })
            .unwrap();

        assert_eq!(built.program, "docker");
        assert!(built
            .args
            .windows(2)
            .any(|part| part == ["--network", "none"]));
        assert!(built.args.iter().any(|arg| arg == "--read-only"));
        assert!(built
            .args
            .windows(2)
            .any(|part| part == ["--cap-drop", "ALL"]));
        assert!(built
            .args
            .windows(2)
            .any(|part| part[0] == "--mount" && part[1].contains("target=/workspace,readonly")));
        assert_eq!(
            &built.args[built.args.len() - 3..],
            ["rust:latest", "cargo", "test"]
        );
    }

    #[test]
    fn constructs_podman_probe_without_running_it() {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let workdir = ContainerWorkdir {
            inner,
            policy: ContainerPolicy::new(config(ContainerSandboxRuntime::Podman)).unwrap(),
        };
        assert_eq!(
            workdir.availability_probe(),
            CommandSpec {
                program: "podman".into(),
                args: vec!["version".into()],
                cwd: None,
                env: BTreeMap::new(),
            }
        );
    }

    #[tokio::test]
    async fn unavailable_runtime_fails_closed_without_native_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(UnavailableWorkdir {
            root: directory.path().into(),
            commands: Mutex::new(vec![]),
        });
        let workdir = ContainerWorkdir {
            inner: inner.clone(),
            policy: ContainerPolicy::new(config(ContainerSandboxRuntime::Docker)).unwrap(),
        };
        assert!(workdir
            .execute(
                CommandSpec {
                    program: "cargo".into(),
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
        assert_eq!(commands[0].program, "docker");
        assert_eq!(commands[0].args, ["version"]);
    }
}
