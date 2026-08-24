use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::{
    BeforeHookResult, BeforeProviderRequestHook, Context, Error, FinishReason, HookContext,
    Message, MessagePart, Plugin, PluginId, PluginRegistrar, ProviderEvent, ProviderMode,
    ProviderRequest, Result, Role,
};

const PLUGIN_ID: &str = "builtin.compaction";
const HOOK_ID: &str = "builtin.compaction.before-provider-request";
const MESSAGE_OVERHEAD_TOKENS: usize = 4;
const CONTEXT_OVERHEAD_TOKENS: usize = 2;
const SUMMARY_PROMPT: &str = "Summarize the messages above following the instruction";

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactionPluginConfig {
    pub max_input_tokens: usize,
    pub reserved_output_tokens: usize,
    pub preserve_recent_messages: usize,
    pub max_summary_tokens: usize,
    pub instruction: String,
}

impl Default for CompactionPluginConfig {
    fn default() -> Self {
        Self {
            max_input_tokens: 128_000,
            reserved_output_tokens: 8_192,
            preserve_recent_messages: 8,
            max_summary_tokens: 1_024,
            instruction: "Preserve decisions, requirements, unresolved questions, and details needed to continue the work. Omit repetition and incidental conversation.".into(),
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
            || self.config.max_summary_tokens == 0
            || self.config.instruction.trim().is_empty()
        {
            return Err(Error::Plugin(
                "compaction requires non-zero input and summary budgets and an instruction".into(),
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
        context: &HookContext,
        request: &mut ProviderRequest,
    ) -> Result<BeforeHookResult> {
        if request.mode != ProviderMode::Normal || estimate_request(request) <= self.input_budget()
        {
            return Ok(BeforeHookResult::Continue);
        }

        let history = context.history();
        let snapshot = history.snapshot().await?;
        let Some(end) = compaction_end(&snapshot.messages, self.config.preserve_recent_messages)
        else {
            return Ok(BeforeHookResult::Continue);
        };
        let covered = &snapshot.messages[..=end];
        let summary_text = self.summarize(context, covered).await?;
        let covered_ids: Vec<_> = covered
            .iter()
            .map(|message| message.id.to_string())
            .collect();
        let mut summary = Message::text(Role::System, summary_text);
        summary.metadata.insert("compaction".into(), json!(true));
        summary
            .metadata
            .insert("covered_message_ids".into(), json!(covered_ids));

        match history
            .replace_range(
                snapshot.revision,
                covered[0].id,
                covered.last().expect("covered prefix is non-empty").id,
                vec![summary],
            )
            .await
        {
            Ok(updated) => request.messages = updated.messages,
            Err(Error::HistoryRevisionMismatch { .. }) => {}
            Err(error) => return Err(error),
        }
        Ok(BeforeHookResult::Continue)
    }
}

impl CompactionHook {
    fn input_budget(&self) -> usize {
        self.config
            .max_input_tokens
            .saturating_sub(self.config.reserved_output_tokens)
    }

    async fn summarize(&self, context: &HookContext, covered: &[Message]) -> Result<String> {
        let provider_id = context.provider_id();
        let provider = context.providers().get(&provider_id).ok_or_else(|| {
            Error::Provider(format!("current provider {provider_id} is not registered"))
        })?;
        let cancellation = context.cancellation();
        let mut messages = Vec::with_capacity(covered.len() + 2);
        messages.push(Message::text(Role::System, self.config.instruction.clone()));
        messages.extend_from_slice(covered);
        messages.push(Message::text(Role::User, SUMMARY_PROMPT));
        let internal_request = ProviderRequest {
            mode: ProviderMode::Compaction,
            model: context.model(),
            messages,
            tools: Vec::new(),
            context: Context::default(),
            cancellation: cancellation.clone(),
        };
        let mut stream = tokio::select! {
            _ = cancellation.cancelled() => return Err(Error::Cancelled),
            stream = provider.request(internal_request) => stream?,
        };
        let max_bytes = self.config.max_summary_tokens.saturating_mul(4);
        let mut summary = String::new();
        let mut rejected_tool_call = false;
        loop {
            let event = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                event = stream.next() => event,
            };
            let Some(event) = event else { break };
            match event? {
                ProviderEvent::TextDelta { text } => append_bounded(&mut summary, &text, max_bytes),
                ProviderEvent::ToolCallDelta { .. }
                | ProviderEvent::Finished {
                    reason: FinishReason::ToolCalls,
                } => rejected_tool_call = true,
                ProviderEvent::ReasoningDelta { .. }
                | ProviderEvent::Usage { .. }
                | ProviderEvent::Finished { .. } => {}
            }
        }
        if rejected_tool_call {
            return Err(Error::Provider(
                "compaction provider returned a tool call".into(),
            ));
        }
        if summary.trim().is_empty() {
            return Err(Error::Provider(
                "compaction provider returned an empty summary".into(),
            ));
        }
        Ok(summary)
    }
}

fn append_bounded(output: &mut String, text: &str, max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(output.len());
    let mut end = remaining.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    output.push_str(&text[..end]);
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

fn estimate_request(request: &ProviderRequest) -> usize {
    let messages = request.messages.iter().map(estimate_message).sum::<usize>();
    let context = request
        .context
        .parts()
        .iter()
        .map(|part| CONTEXT_OVERHEAD_TOKENS + estimate_tokens(&part.content))
        .sum::<usize>();
    let tools = request
        .tools
        .iter()
        .map(|tool| estimate_tokens(&serde_json::to_string(tool).unwrap_or_default()))
        .sum::<usize>();
    messages.saturating_add(context).saturating_add(tools)
}

fn compaction_end(messages: &[Message], preserve_recent_messages: usize) -> Option<usize> {
    if messages.len() <= 1 {
        return None;
    }
    let current_user = messages
        .iter()
        .rposition(|message| message.role == Role::User)
        .unwrap_or(messages.len() - 1);
    let preserve_from = messages
        .len()
        .saturating_sub(preserve_recent_messages)
        .min(current_user);
    if preserve_from == 0 {
        return None;
    }

    let mut end = preserve_from - 1;
    if messages[preserve_from].role == Role::Tool {
        while end > 0 && messages[end].role == Role::Tool {
            end -= 1;
        }
        if messages[end].role == Role::Assistant
            && messages[end]
                .content
                .iter()
                .any(|part| matches!(part, MessagePart::ToolCall { .. }))
        {
            if end == 0 {
                return None;
            }
            end -= 1;
        }
    } else if messages[end].role == Role::Assistant
        && messages[end]
            .content
            .iter()
            .any(|part| matches!(part, MessagePart::ToolCall { .. }))
    {
        if end == 0 {
            return None;
        }
        end -= 1;
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_util::stream;

    use super::*;
    use crate::{
        core::{Model, OpenSession, Provider, ProviderId, ProviderStream},
        testkit::StubWorkdir,
        Core,
    };
    use tokio::sync::Notify;

    struct RecordingProvider {
        requests: Arc<Mutex<Vec<ProviderRequest>>>,
        compaction_started: Option<Arc<Notify>>,
        compaction_release: Option<Arc<Notify>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from("recording")
        }

        async fn get_models(&self) -> Result<Vec<Model>> {
            Ok(vec![Model {
                id: "current-model".into(),
                display_name: "Current Model".into(),
            }])
        }

        async fn request(&self, request: ProviderRequest) -> Result<ProviderStream> {
            let mode = request.mode;
            self.requests.lock().unwrap().push(request);
            if mode == ProviderMode::Compaction {
                if let Some(started) = &self.compaction_started {
                    started.notify_one();
                }
                if let Some(release) = &self.compaction_release {
                    release.notified().await;
                }
            }
            let text = match mode {
                ProviderMode::Compaction => "provider summary",
                ProviderMode::Normal => "normal answer",
                _ => panic!("unexpected provider mode"),
            };
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta { text: text.into() }),
                Ok(ProviderEvent::Finished {
                    reason: FinishReason::Stop,
                }),
            ])))
        }
    }

    struct RecordingProviderPlugin {
        provider: Arc<RecordingProvider>,
    }

    #[async_trait]
    impl Plugin for RecordingProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("test.recording-provider")
        }

        async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
            registrar.register_provider(0, self.provider.clone())
        }
    }

    async fn wait_for_messages(session: &crate::core::Session, count: usize) {
        let mut snapshots = session.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if snapshots.borrow().messages.len() >= count
                    && snapshots.borrow().active_turn.is_none()
                {
                    break;
                }
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn compacts_once_through_provider_and_replaces_canonical_history() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            requests: requests.clone(),
            compaction_started: None,
            compaction_release: None,
        });
        let instruction = "Keep every durable implementation decision";
        let core = Core::new()
            .with_plugin(Arc::new(RecordingProviderPlugin { provider }))
            .with_plugin(compaction_plugin(CompactionPluginConfig {
                max_input_tokens: 85,
                reserved_output_tokens: 10,
                preserve_recent_messages: 1,
                max_summary_tokens: 20,
                instruction: instruction.into(),
            }))
            .build()
            .await
            .unwrap();
        let session = core
            .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
            .open_session(OpenSession {
                id: None,
                provider: ProviderId::from("recording"),
                model: "current-model".into(),
            })
            .await
            .unwrap();

        session.send_text("x".repeat(260)).await.unwrap();
        wait_for_messages(&session, 2).await;
        assert_eq!(requests.lock().unwrap().len(), 1, "under budget");
        let covered_ids: Vec<_> = session
            .snapshot()
            .messages
            .iter()
            .map(|message| message.id.to_string())
            .collect();

        session.send_text("current question").await.unwrap();
        wait_for_messages(&session, 3).await;

        let recorded = requests.lock().unwrap();
        assert_eq!(
            recorded
                .iter()
                .filter(|request| request.mode == ProviderMode::Compaction)
                .count(),
            1
        );
        let compaction = recorded
            .iter()
            .find(|request| request.mode == ProviderMode::Compaction)
            .unwrap();
        assert_eq!(compaction.model, "current-model");
        assert!(compaction.tools.is_empty());
        assert_eq!(compaction.messages[0].role, Role::System);
        assert_eq!(
            compaction.messages[0].content,
            Message::text(Role::System, instruction).content
        );
        assert_eq!(
            compaction.messages.last().unwrap().content,
            Message::text(Role::User, SUMMARY_PROMPT).content
        );

        let history = session.snapshot().messages;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].role, Role::System);
        assert_eq!(
            history[0].content,
            Message::text(Role::System, "provider summary").content
        );
        assert_eq!(history[0].metadata.get("compaction"), Some(&json!(true)));
        assert_eq!(
            history[0].metadata.get("covered_message_ids"),
            Some(&json!(covered_ids.clone()))
        );
        assert!(history
            .iter()
            .all(|message| !covered_ids.contains(&message.id.to_string())));
        assert_eq!(
            history[1].content,
            Message::text(Role::User, "current question").content
        );
        assert_eq!(
            recorded.last().unwrap().messages,
            history[..2],
            "outer request uses replaced canonical history"
        );
    }

    #[tokio::test]
    async fn revision_conflict_skips_stale_replacement_without_failing_turn() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let provider = Arc::new(RecordingProvider {
            requests: requests.clone(),
            compaction_started: Some(started.clone()),
            compaction_release: Some(release.clone()),
        });
        let core = Core::new()
            .with_plugin(Arc::new(RecordingProviderPlugin { provider }))
            .with_plugin(compaction_plugin(CompactionPluginConfig {
                max_input_tokens: 85,
                reserved_output_tokens: 10,
                preserve_recent_messages: 1,
                max_summary_tokens: 20,
                instruction: "Keep decisions".into(),
            }))
            .build()
            .await
            .unwrap();
        let session = core
            .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
            .open_session(OpenSession {
                id: None,
                provider: ProviderId::from("recording"),
                model: "current-model".into(),
            })
            .await
            .unwrap();

        session.send_text("x".repeat(260)).await.unwrap();
        wait_for_messages(&session, 2).await;
        session.send_text("current question").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .unwrap();

        let snapshot = session.history().snapshot().await.unwrap();
        let first = snapshot.messages[0].clone();
        session
            .history()
            .replace_range(snapshot.revision, first.id, first.id, vec![first])
            .await
            .unwrap();
        release.notify_one();
        wait_for_messages(&session, 4).await;

        let history = session.snapshot().messages;
        assert!(history
            .iter()
            .all(|message| !message.metadata.contains_key("compaction")));
        let recorded = requests.lock().unwrap();
        assert_eq!(
            recorded
                .iter()
                .filter(|request| request.mode == ProviderMode::Compaction)
                .count(),
            1
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|request| request.mode == ProviderMode::Normal)
                .count(),
            2
        );
    }

    #[test]
    fn tool_call_group_is_not_split() {
        let call = crate::core::ToolCallId::new("call");
        let messages = vec![
            Message::text(Role::User, "old"),
            Message::new(
                Role::Assistant,
                vec![MessagePart::ToolCall {
                    id: call.clone(),
                    name: "read".into(),
                    arguments: json!({}),
                }],
            ),
            Message::new(
                Role::Tool,
                vec![MessagePart::ToolResult {
                    call_id: call,
                    content: "result".into(),
                    is_error: false,
                }],
            ),
            Message::text(Role::User, "current"),
        ];

        assert_eq!(compaction_end(&messages, 2), Some(0));
        assert_eq!(compaction_end(&messages, 1), Some(2));
    }

    #[test]
    fn bounded_append_preserves_utf8_boundaries() {
        let mut output = String::new();
        append_bounded(&mut output, "abcé", 4);
        assert_eq!(output, "abc");
    }
}
