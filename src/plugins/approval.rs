use std::sync::Arc;

use async_trait::async_trait;

use crate::core::{
    BeforeHookResult, BeforeToolExecutionHook, Plugin, PluginId, PluginRegistrar, Result,
    ToolExecutionContext,
};

const APPROVAL_PLUGIN_ID: &str = "builtin.approval";
const APPROVAL_HOOK_ID: &str = "builtin.approval.before-tool-call";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalPolicy {
    Allow,
    Deny,
}

struct ApprovalPlugin {
    policy: ApprovalPolicy,
}

pub fn approval_plugin(policy: ApprovalPolicy) -> Arc<dyn Plugin> {
    Arc::new(ApprovalPlugin { policy })
}

#[async_trait]
impl Plugin for ApprovalPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(APPROVAL_PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_before_tool_execution(APPROVAL_HOOK_ID, 0, self)
    }
}

#[async_trait]
impl BeforeToolExecutionHook for ApprovalPlugin {
    async fn before_tool_execution(
        &self,
        _context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult> {
        Ok(match self.policy {
            ApprovalPolicy::Allow => BeforeHookResult::Continue,
            ApprovalPolicy::Deny => BeforeHookResult::Cancel {
                reason: "tool execution denied by approval policy".into(),
            },
        })
    }
}
