use std::sync::Arc;

use async_trait::async_trait;

use crate::core::{
    Context, ContextContributionHook, ContextPart, ContextPriority, ContextSource, HookContext,
    Plugin, PluginId, PluginRegistrar, Result, Workdir,
};

const PLUGIN_ID: &str = "builtin.instructions.base";
const HOOK_ID: &str = "builtin.instructions.base.context";

struct BaseInstructionsHook {
    instructions: Vec<String>,
}

impl BaseInstructionsHook {
    fn new(instructions: Vec<String>) -> Self {
        Self { instructions }
    }
}

#[async_trait]
impl ContextContributionHook for BaseInstructionsHook {
    async fn contribute_context(
        &self,
        _hook_context: &HookContext,
        _workdir: Arc<dyn Workdir>,
        context: &mut Context,
    ) -> Result<()> {
        for instruction in &self.instructions {
            if !instruction.trim().is_empty() {
                context.push(ContextPart {
                    priority: ContextPriority::Persistent,
                    source: ContextSource::Plugin(PLUGIN_ID.into()),
                    content: instruction.clone(),
                });
            }
        }
        Ok(())
    }
}

struct BaseInstructionsPlugin {
    hook: Arc<BaseInstructionsHook>,
}

#[async_trait]
impl Plugin for BaseInstructionsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_context_contribution(HOOK_ID, 0, self.hook.clone())
    }
}

pub fn base_instructions_plugin(instructions: Vec<String>) -> Arc<dyn Plugin> {
    Arc::new(BaseInstructionsPlugin {
        hook: Arc::new(BaseInstructionsHook::new(instructions)),
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
    async fn base_hook_contributes_only_configured_base_instructions() {
        let hook = BaseInstructionsHook::new(vec!["first".into(), "second".into()]);
        let workdir = Arc::new(StubWorkdir::new("unused"));
        let mut context = Context::default();

        hook.contribute_context(&hook_context(), workdir, &mut context)
            .await
            .unwrap();

        let contents: Vec<_> = context
            .parts()
            .iter()
            .map(|part| part.content.as_str())
            .collect();
        assert_eq!(contents, ["first", "second"]);
        assert!(context.parts().iter().all(|part| {
            part.source == ContextSource::Plugin(PLUGIN_ID.into())
                && part.priority == ContextPriority::Persistent
        }));
    }

    #[tokio::test]
    async fn omitted_and_empty_base_instructions_add_no_context() {
        let hook = BaseInstructionsHook::new(vec![String::new(), "  ".into()]);
        let workdir = Arc::new(StubWorkdir::new("unused"));
        let mut context = Context::default();

        hook.contribute_context(&hook_context(), workdir, &mut context)
            .await
            .unwrap();

        assert!(context.parts().is_empty());
    }
}
