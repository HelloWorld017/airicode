use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::core::{
    CommandOutput, CommandSpec, Error, Plugin, PluginId, PluginRegistrar, Result, Workdir,
    WorkdirLayer, WorkdirLayerContext, WorkdirLayerId,
};

const PLUGIN_ID: &str = "builtin.sandbox-policy";
const LAYER_ID: &str = "builtin.sandbox-policy.workdir";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyFileOperation {
    Read,
    Write,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyPathRule {
    pub operation: Option<PolicyFileOperation>,
    pub path: PathBuf,
    pub allow: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyProcessRule {
    pub program: Option<String>,
    pub allow: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PolicySandboxConfig {
    pub path_rules: Vec<PolicyPathRule>,
    pub process_rules: Vec<PolicyProcessRule>,
    pub default_allow: bool,
}

pub fn policy_sandbox_plugin(config: PolicySandboxConfig) -> Arc<dyn Plugin> {
    Arc::new(PolicyPlugin { config })
}

#[derive(Clone)]
struct Policy {
    path_rules: Vec<PolicyPathRule>,
    process_rules: Vec<PolicyProcessRule>,
    default_allow: bool,
}

impl Policy {
    fn new(mut config: PolicySandboxConfig) -> Result<Self> {
        for rule in &mut config.path_rules {
            rule.path = validate_relative(&rule.path, true)?;
        }
        Ok(Self {
            path_rules: config.path_rules,
            process_rules: config.process_rules,
            default_allow: config.default_allow,
        })
    }

    fn check_path(&self, operation: PolicyFileOperation, path: &Path) -> Result<PathBuf> {
        let path = validate_relative(path, false)?;
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
                "policy sandbox denied {operation:?} for {}",
                path.display()
            )))
        }
    }

    fn check_process(&self, program: &str) -> Result<()> {
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
                "policy sandbox denied execution of {program}"
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

struct PolicyPlugin {
    config: PolicySandboxConfig,
}

#[async_trait]
impl Plugin for PolicyPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        let policy = Policy::new(self.config.clone())?;
        registrar.register_workdir_layer(0, Arc::new(PolicyLayer { policy }))
    }
}

struct PolicyLayer {
    policy: Policy,
}

impl WorkdirLayer for PolicyLayer {
    fn id(&self) -> WorkdirLayerId {
        WorkdirLayerId::new(LAYER_ID)
    }

    fn layer(&self, _context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        Arc::new(PolicyWorkdir {
            inner,
            policy: self.policy.clone(),
        })
    }
}

struct PolicyWorkdir {
    inner: Arc<dyn Workdir>,
    policy: Policy,
}

#[async_trait]
impl Workdir for PolicyWorkdir {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let path = self.policy.check_path(PolicyFileOperation::Read, path)?;
        self.inner.read(&path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let path = self.policy.check_path(PolicyFileOperation::Write, path)?;
        self.inner.write(&path, data).await
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        let path = self.policy.check_path(PolicyFileOperation::Remove, path)?;
        self.inner.remove(&path).await
    }

    async fn execute(
        &self,
        command: CommandSpec,
        _cancellation: CancellationToken,
    ) -> Result<CommandOutput> {
        self.policy.check_process(&command.program)?;
        // Delegating to an arbitrary inner workdir cannot enforce this layer's path policy.
        Err(Error::Workdir(
            "policy sandbox cannot safely delegate process execution".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Core, NativeWorkdir};
    use std::collections::BTreeMap;

    fn config() -> PolicySandboxConfig {
        PolicySandboxConfig {
            path_rules: vec![
                PolicyPathRule {
                    operation: None,
                    path: "private".into(),
                    allow: false,
                },
                PolicyPathRule {
                    operation: Some(PolicyFileOperation::Read),
                    path: "".into(),
                    allow: true,
                },
            ],
            process_rules: vec![PolicyProcessRule {
                program: Some("true".into()),
                allow: true,
            }],
            default_allow: false,
        }
    }

    #[tokio::test]
    async fn plugin_registers_only_one_workdir_layer() {
        let omitted = Core::new().build().await.unwrap();
        assert!(omitted.workdir_layers().ids().is_empty());

        let plugin = policy_sandbox_plugin(config());
        let registrar = PluginRegistrar::new(plugin.id());
        plugin.init(registrar.clone()).await.unwrap();
        let staged = registrar.take();

        assert_eq!(staged.workdir_layers.len(), 1);
        assert!(staged.providers.is_empty());
        assert!(staged.tools.is_empty());
        assert!(staged.hooks.is_empty());
        assert!(staged.store_factories.is_empty());
        assert_eq!(staged.workdir_layers[0].id, WorkdirLayerId::new(LAYER_ID));
    }

    #[tokio::test]
    async fn rules_are_ordered_and_execute_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let workdir = PolicyWorkdir {
            inner,
            policy: Policy::new(config()).unwrap(),
        };

        assert!(workdir.read(Path::new("private/file")).await.is_err());
        assert!(workdir.read(Path::new("public/file")).await.is_err());
        assert!(workdir
            .write(Path::new("public/file"), b"no")
            .await
            .is_err());
        assert!(workdir
            .execute(
                CommandSpec {
                    program: "true".into(),
                    args: vec![],
                    cwd: None,
                    env: BTreeMap::new(),
                },
                CancellationToken::new(),
            )
            .await
            .is_err());
    }
}
