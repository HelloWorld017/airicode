use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;

use crate::core::{
    Command, CommandContext, CommandDescriptor, CommandId, CommandInvocation, CommandResult,
    Context, Error, FinishReason, Message, Plugin, PluginId, PluginRegistrar, ProviderEvent,
    ProviderMode, ProviderRequest, Result, Role, Usage,
};

const PLUGIN_ID: &str = "builtin.sidequery";
const COMMAND_ID: &str = "builtin.sidequery.query";

#[derive(Clone, Debug)]
pub struct SideQueryConfig {
    /// Uses the current turn's model when unset.
    pub model: Option<String>,
    pub system: Option<String>,
    pub max_output_bytes: usize,
    pub timeout: Duration,
}

impl SideQueryConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            system: None,
            max_output_bytes: 128 * 1024,
            timeout: Duration::from_secs(60),
        }
    }
}

struct SideQueryPlugin {
    config: SideQueryConfig,
}

pub fn sidequery_plugin(config: SideQueryConfig) -> Arc<dyn Plugin> {
    Arc::new(SideQueryPlugin { config })
}

#[async_trait]
impl Plugin for SideQueryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        validate_config(&self.config)?;
        registrar.register_command(
            0,
            Arc::new(SideQueryCommand {
                config: self.config.clone(),
            }),
        )
    }
}

struct SideQueryCommand {
    config: SideQueryConfig,
}

#[derive(Serialize)]
struct SideQueryOutput {
    text: String,
    truncated: bool,
    finish_reason: Option<FinishReason>,
    usage: Option<Usage>,
}

#[async_trait]
impl Command for SideQueryCommand {
    fn id(&self) -> CommandId {
        CommandId::new(COMMAND_ID)
    }

    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "sidequery".into(),
            description: "Run an isolated, tool-free provider query. The query cannot modify the current conversation or workdir.".into(),
            usage: "/sidequery <prompt>".into(),
        }
    }

    async fn execute(
        &self,
        invocation: CommandInvocation,
        context: CommandContext,
    ) -> Result<CommandResult> {
        if invocation.arguments.trim().is_empty() {
            return Err(Error::Plugin("usage: /sidequery <prompt>".into()));
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let provider_id = context.hook_context.provider_id();
        let provider = context
            .hook_context
            .providers()
            .get(&provider_id)
            .ok_or_else(|| {
                Error::Provider(format!("current provider {provider_id} is not registered"))
            })?;
        let model = self
            .config
            .model
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| context.hook_context.model());
        if model.trim().is_empty() {
            return Err(Error::Provider("sidequery model may not be empty".into()));
        }
        let cancellation = context.cancellation.child_token();
        let mut messages = Vec::new();
        if let Some(system) = self
            .config
            .system
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            messages.push(Message::text(Role::System, system));
        }
        messages.push(Message::text(Role::User, invocation.arguments));

        let request = ProviderRequest {
            mode: ProviderMode::SideQuery,
            model: model.clone(),
            messages,
            tools: Vec::new(),
            context: Context::default(),
            cancellation: cancellation.clone(),
        };
        let operation = async {
            let mut stream = provider.request(request).await?;
            let mut output = SideQueryOutput {
                text: String::new(),
                truncated: false,
                finish_reason: None,
                usage: None,
            };
            while let Some(event) = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                event = stream.next() => event,
            } {
                match event? {
                    ProviderEvent::TextDelta { text } => append_bounded(
                        &mut output.text,
                        &text,
                        self.config.max_output_bytes,
                        &mut output.truncated,
                    ),
                    ProviderEvent::Usage { usage } => output.usage = Some(usage),
                    ProviderEvent::Finished { reason } => output.finish_reason = Some(reason),
                    ProviderEvent::ReasoningDelta { .. } | ProviderEvent::ToolCallDelta { .. } => {}
                }
            }
            Ok::<_, Error>(output)
        };
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(self.config.timeout, operation) => match result {
                Ok(result) => result,
                Err(_) => {
                    cancellation.cancel();
                    Err(Error::Provider("sidequery timed out".into()))
                }
            },
        };
        let output = result?;
        let content = serde_json::to_string(&output).map_err(|error| {
            Error::Plugin(format!("could not encode sidequery output: {error}"))
        })?;
        Ok(CommandResult { content })
    }
}

fn validate_config(config: &SideQueryConfig) -> Result<()> {
    if config
        .model
        .as_ref()
        .is_some_and(|model| model.trim().is_empty())
        || config.max_output_bytes == 0
        || config.timeout.is_zero()
    {
        return Err(Error::Plugin("invalid sidequery configuration".into()));
    }
    Ok(())
}

fn append_bounded(target: &mut String, value: &str, limit: usize, truncated: &mut bool) {
    let remaining = limit.saturating_sub(target.len());
    if value.len() <= remaining {
        target.push_str(value);
        return;
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= remaining)
        .last()
        .unwrap_or(0);
    target.push_str(&value[..boundary]);
    *truncated = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Core;

    #[test]
    fn bounds_utf8_output() {
        let mut output = String::new();
        let mut truncated = false;
        append_bounded(&mut output, "ab\u{e9}cd", 3, &mut truncated);
        assert_eq!(output, "ab");
        assert!(truncated);
    }

    #[tokio::test]
    async fn plugin_registers_only_its_isolated_command() {
        let core = Core::new()
            .with_plugin(sidequery_plugin(SideQueryConfig::new("test")))
            .build()
            .await
            .unwrap();
        assert_eq!(core.commands().ids(), vec![CommandId::new(COMMAND_ID)]);
        assert!(core.commands().descriptors()[0]
            .description
            .contains("tool-free"));
        assert!(core.tools().ids().is_empty());

        let omitted = Core::new().build().await.unwrap();
        assert!(omitted.commands().ids().is_empty());
    }
}
