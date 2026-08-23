use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;

use crate::core::{
    Context, ContextContributionHook, ContextPart, ContextPriority, ContextSource, Error,
    HookContext, Plugin, PluginId, PluginRegistrar, Result, Workdir,
};

const PLUGIN_ID: &str = "builtin.instructions.agents";
const HOOK_ID: &str = "builtin.instructions.agents.context";
const DEFAULT_MAX_INSTRUCTION_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_TOTAL_INSTRUCTION_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentsInstructionsConfig {
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for AgentsInstructionsConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_INSTRUCTION_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_INSTRUCTION_BYTES,
        }
    }
}

struct AgentsInstruction {
    path: PathBuf,
    content: String,
}

struct AgentsInstructionsHook {
    config: AgentsInstructionsConfig,
}

impl AgentsInstructionsHook {
    fn load(&self, root: &Path) -> Result<Option<AgentsInstruction>> {
        if self.config.max_file_bytes == 0 || self.config.max_total_bytes == 0 {
            return Err(Error::Plugin(
                "instruction size limits must be non-zero".into(),
            ));
        }

        let root = fs::canonicalize(root).map_err(|error| {
            Error::Plugin(format!(
                "could not resolve workdir root {}: {error}",
                root.display()
            ))
        })?;
        let path = root.join("AGENTS.md");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(Error::Plugin(format!(
                    "could not inspect {}: {error}",
                    path.display()
                )))
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Plugin(format!(
                "instruction file must be a regular non-symlink file: {}",
                path.display()
            )));
        }

        let limit = self.config.max_file_bytes.min(self.config.max_total_bytes);
        if metadata.len() > limit as u64 {
            return Err(Error::Plugin(format!(
                "instruction size limit exceeded by {}",
                path.display()
            )));
        }

        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        fs::File::open(&path)
            .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
            .map_err(|error| {
                Error::Plugin(format!("could not read {}: {error}", path.display()))
            })?;
        if bytes.len() > limit {
            return Err(Error::Plugin(format!(
                "instruction size limit exceeded by {}",
                path.display()
            )));
        }
        let content = String::from_utf8(bytes).map_err(|error| {
            Error::Plugin(format!(
                "instruction file {} is not UTF-8: {error}",
                path.display()
            ))
        })?;

        Ok(Some(AgentsInstruction {
            path: PathBuf::from("AGENTS.md"),
            content,
        }))
    }
}

#[async_trait]
impl ContextContributionHook for AgentsInstructionsHook {
    async fn contribute_context(
        &self,
        _hook_context: &HookContext,
        workdir: Arc<dyn Workdir>,
        context: &mut Context,
    ) -> Result<()> {
        let Some(instruction) = self.load(workdir.root())? else {
            return Ok(());
        };
        if !instruction.content.trim().is_empty() {
            context.push(ContextPart {
                priority: ContextPriority::Persistent,
                source: ContextSource::Plugin(PLUGIN_ID.into()),
                content: format!(
                    "Instructions from {}:\n{}",
                    instruction.path.display(),
                    instruction.content
                ),
            });
        }
        Ok(())
    }
}

struct AgentsInstructionsPlugin {
    hook: Arc<AgentsInstructionsHook>,
}

#[async_trait]
impl Plugin for AgentsInstructionsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_context_contribution(HOOK_ID, 0, self.hook.clone())
    }
}

pub fn agents_instructions_plugin(config: AgentsInstructionsConfig) -> Arc<dyn Plugin> {
    Arc::new(AgentsInstructionsPlugin {
        hook: Arc::new(AgentsInstructionsHook { config }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{ProjectId, SessionId},
        testkit::StubWorkdir,
    };

    fn hook_context() -> HookContext {
        HookContext {
            project_id: ProjectId::new(),
            session_id: SessionId::new(),
        }
    }

    #[tokio::test]
    async fn agents_hook_contributes_only_project_root_agents_file() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("AGENTS.md"), "root agents").unwrap();
        fs::write(temp.path().join("nested/AGENTS.md"), "nested agents").unwrap();
        let hook = AgentsInstructionsHook {
            config: AgentsInstructionsConfig::default(),
        };
        let workdir = Arc::new(StubWorkdir::new(temp.path()));
        let mut context = Context::default();

        hook.contribute_context(&hook_context(), workdir, &mut context)
            .await
            .unwrap();

        assert_eq!(context.parts().len(), 1);
        assert_eq!(
            context.parts()[0].content,
            "Instructions from AGENTS.md:\nroot agents"
        );
        assert_eq!(
            context.parts()[0].source,
            ContextSource::Plugin(PLUGIN_ID.into())
        );
        assert!(!context.parts()[0].content.contains("nested agents"));
    }

    #[tokio::test]
    async fn missing_agents_file_adds_no_context() {
        let temp = tempfile::tempdir().unwrap();
        let hook = AgentsInstructionsHook {
            config: AgentsInstructionsConfig::default(),
        };
        let workdir = Arc::new(StubWorkdir::new(temp.path()));
        let mut context = Context::default();

        hook.contribute_context(&hook_context(), workdir, &mut context)
            .await
            .unwrap();

        assert!(context.parts().is_empty());
    }

    #[tokio::test]
    async fn agents_hook_enforces_context_bound() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("AGENTS.md"), "12345").unwrap();
        let hook = AgentsInstructionsHook {
            config: AgentsInstructionsConfig {
                max_file_bytes: 4,
                max_total_bytes: 20,
            },
        };
        let workdir = Arc::new(StubWorkdir::new(temp.path()));
        let mut context = Context::default();

        let error = hook
            .contribute_context(&hook_context(), workdir, &mut context)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("size limit"));
        assert!(context.parts().is_empty());
    }
}
