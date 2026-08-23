use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{
    BeforeHookResult, BeforeProviderRequestHook, Context, ContextPriority, HookContext, Message,
    MessagePart, Plugin, PluginId, PluginRegistrar, ProviderRequest, Result, Role,
};

const PLUGIN_ID: &str = "builtin.compaction";
const HOOK_ID: &str = "builtin.compaction.before-provider-request";
const MESSAGE_OVERHEAD_TOKENS: usize = 4;
const CONTEXT_OVERHEAD_TOKENS: usize = 2;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactionPluginConfig {
    pub max_input_tokens: usize,
    pub reserved_output_tokens: usize,
    pub preserve_recent_messages: usize,
    pub max_summary_tokens: usize,
}

impl Default for CompactionPluginConfig {
    fn default() -> Self {
        Self {
            max_input_tokens: 128_000,
            reserved_output_tokens: 8_192,
            preserve_recent_messages: 8,
            max_summary_tokens: 1_024,
        }
    }
}

struct CompactionPlugin {
    config: CompactionPluginConfig,
}

struct CompactionHook {
    config: CompactionPluginConfig,
}

pub fn compaction_plugin(config: CompactionPluginConfig) -> Arc<dyn Plugin> {
    Arc::new(CompactionPlugin { config })
}

#[async_trait]
impl Plugin for CompactionPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        if self.config.max_input_tokens == 0
            || self.config.reserved_output_tokens >= self.config.max_input_tokens
        {
            return Err(crate::core::Error::Plugin(
                "compaction requires a non-zero input budget".into(),
            ));
        }
        registrar.register_before_provider_request(
            HOOK_ID,
            0,
            Arc::new(CompactionHook {
                config: self.config.clone(),
            }),
        )
    }
}

#[async_trait]
impl BeforeProviderRequestHook for CompactionHook {
    async fn before_provider_request(
        &self,
        _context: &HookContext,
        request: &mut ProviderRequest,
    ) -> Result<BeforeHookResult> {
        compact_request(request, &self.config);
        Ok(BeforeHookResult::Continue)
    }
}

fn estimate_tokens(text: &str) -> usize {
    text.len().saturating_add(3) / 4
}

fn estimate_message(message: &Message) -> usize {
    MESSAGE_OVERHEAD_TOKENS
        + message
            .content
            .iter()
            .map(|part| match part {
                MessagePart::Text { text } | MessagePart::Reasoning { text } => {
                    estimate_tokens(text)
                }
                MessagePart::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    estimate_tokens(id.as_str())
                        + estimate_tokens(name)
                        + estimate_tokens(&arguments.to_string())
                }
                MessagePart::ToolResult {
                    call_id, content, ..
                } => estimate_tokens(call_id.as_str()) + estimate_tokens(content),
            })
            .sum::<usize>()
}

fn compact_request(request: &mut ProviderRequest, config: &CompactionPluginConfig) {
    let budget = config
        .max_input_tokens
        .saturating_sub(config.reserved_output_tokens);
    let recent_start = request
        .messages
        .len()
        .saturating_sub(config.preserve_recent_messages);
    let mut retained_context = Vec::new();
    let mut used = 0usize;

    for part in request.context.parts() {
        if part.priority != ContextPriority::Low {
            used = used.saturating_add(CONTEXT_OVERHEAD_TOKENS + estimate_tokens(&part.content));
            retained_context.push(part.clone());
        }
    }

    let mut retained = vec![false; request.messages.len()];
    for (index, message) in request.messages.iter().enumerate() {
        if index >= recent_start || message.role == Role::System {
            retained[index] = true;
            used = used.saturating_add(estimate_message(message));
        }
    }

    for index in (0..recent_start).rev() {
        if retained[index] {
            continue;
        }
        let tokens = estimate_message(&request.messages[index]);
        if used.saturating_add(tokens) <= budget {
            retained[index] = true;
            used += tokens;
        }
    }

    for part in request.context.parts() {
        if part.priority == ContextPriority::Low {
            let tokens = CONTEXT_OVERHEAD_TOKENS + estimate_tokens(&part.content);
            if used.saturating_add(tokens) <= budget {
                retained_context.push(part.clone());
                used += tokens;
            }
        }
    }

    let omitted: Vec<&Message> = request
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (!retained[index]).then_some(message))
        .collect();
    let summary = make_summary(
        &omitted,
        budget.saturating_sub(used).min(config.max_summary_tokens),
    );

    let mut messages = Vec::with_capacity(retained.iter().filter(|keep| **keep).count() + 1);
    let summary_position = retained.iter().position(|keep| !*keep).unwrap_or(0);
    for (index, message) in request.messages.iter().enumerate() {
        if index == summary_position {
            if let Some(summary) = summary.clone() {
                messages.push(summary);
            }
        }
        if retained[index] {
            messages.push(message.clone());
        }
    }
    if request.messages.is_empty() {
        if let Some(summary) = summary {
            messages.push(summary);
        }
    }

    let mut context = Context::default();
    for part in retained_context {
        context.push(part);
    }
    request.context = context;
    request.messages = messages;
}

fn make_summary(messages: &[&Message], token_budget: usize) -> Option<Message> {
    if messages.is_empty() || token_budget <= MESSAGE_OVERHEAD_TOKENS {
        return None;
    }
    let mut text = format!("Summary of {} earlier message(s):", messages.len());
    for message in messages {
        text.push('\n');
        text.push_str(match message.role {
            Role::System => "system: ",
            Role::User => "user: ",
            Role::Assistant => "assistant: ",
            Role::Tool => "tool: ",
        });
        for part in &message.content {
            match part {
                MessagePart::Text { text: value } | MessagePart::Reasoning { text: value } => {
                    text.push_str(value)
                }
                MessagePart::ToolCall { name, .. } => text.push_str(&format!("called {name}")),
                MessagePart::ToolResult { content, .. } => text.push_str(content),
            }
            text.push(' ');
        }
    }
    while MESSAGE_OVERHEAD_TOKENS + estimate_tokens(&text) > token_budget {
        text.pop()?;
    }
    Some(Message::text(Role::System, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ContextPart, ContextSource, Core, ProjectId, SessionId};
    use tokio_util::sync::CancellationToken;

    fn request() -> ProviderRequest {
        let mut context = Context::default();
        context.push(ContextPart {
            priority: ContextPriority::Persistent,
            source: ContextSource::Core,
            content: "required system context".into(),
        });
        context.push(ContextPart {
            priority: ContextPriority::Low,
            source: ContextSource::Core,
            content: "optional context that should be omitted".repeat(20),
        });
        ProviderRequest {
            model: "test".into(),
            messages: vec![
                Message::text(Role::System, "original system message"),
                Message::text(Role::User, "old message".repeat(100)),
                Message::text(Role::Assistant, "recent answer"),
                Message::text(Role::User, "recent question"),
            ],
            tools: Vec::new(),
            context,
            cancellation: CancellationToken::new(),
        }
    }

    fn hook_context() -> HookContext {
        HookContext {
            project_id: ProjectId::new(),
            session_id: SessionId::new(),
        }
    }

    #[tokio::test]
    async fn plugin_compacts_deterministically_and_preserves_required_input() {
        let config = CompactionPluginConfig {
            max_input_tokens: 80,
            reserved_output_tokens: 10,
            preserve_recent_messages: 2,
            max_summary_tokens: 20,
        };
        let core = Core::new()
            .with_plugin(compaction_plugin(config))
            .build()
            .await
            .unwrap();
        let mut first = request();
        let mut second = first.clone();
        core.hooks()
            .before_provider_request(&hook_context(), &mut first)
            .await
            .unwrap();
        core.hooks()
            .before_provider_request(&hook_context(), &mut second)
            .await
            .unwrap();

        assert_eq!(first.context, second.context);
        assert_eq!(first.messages.len(), second.messages.len());
        assert!(first
            .context
            .parts()
            .iter()
            .any(|part| part.content == "required system context"));
        assert!(first
            .messages
            .iter()
            .any(|message| message.role == Role::System
                && message.content == request().messages[0].content));
        assert_eq!(
            first.messages[first.messages.len() - 2].content,
            request().messages[2].content
        );
        assert_eq!(
            first.messages[first.messages.len() - 1].content,
            request().messages[3].content
        );
    }

    #[tokio::test]
    async fn omitting_plugin_omits_compaction_hook() {
        let core = Core::new().build().await.unwrap();
        let mut request = request();
        let original = request.clone();
        core.hooks()
            .before_provider_request(&hook_context(), &mut request)
            .await
            .unwrap();
        assert_eq!(request.messages, original.messages);
        assert_eq!(request.context, original.context);
    }
}
