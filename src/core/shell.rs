use super::{
    error::{Error, Result},
    models::{ShellActionContext, ShellActionDefinition, ShellActionInvocation},
    registry::Registry,
};

/// Dispatches `airicode <action> [arguments...]` invocations.
///
/// The action is cloned from the Registry before its handler runs. This keeps
/// Registry locks out of plugin code and allows actions to be removed while a
/// previously resolved invocation is still running.
#[derive(Clone)]
pub struct ShellActionHandler {
    registry: Registry,
}

impl ShellActionHandler {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }

    pub fn schemes(&self) -> Vec<ShellActionDefinition> {
        self.registry
            .shell_actions()
            .into_iter()
            .map(|action| action.definition())
            .collect()
    }

    pub async fn handle(
        &self,
        invocation: ShellActionInvocation,
        context: ShellActionContext,
    ) -> Result<String> {
        let action = self
            .registry
            .shell_action_by_name(&invocation.name)
            .ok_or_else(|| Error::Command(format!("unknown shell action: {}", invocation.name)))?;
        action.execute(invocation.into_input(), context).await
    }

    pub async fn handle_args<I, S>(
        &self,
        arguments: I,
        context: ShellActionContext,
    ) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut arguments = arguments.into_iter().map(Into::into);
        let name = arguments
            .next()
            .ok_or_else(|| Error::Command("expected a shell action".into()))?;
        self.handle(ShellActionInvocation::new(name, arguments), context)
            .await
    }
}
