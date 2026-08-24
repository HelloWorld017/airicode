use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use airicode::{
    core::*,
    testkit::{stub_provider_plugin, StubProvider, StubWorkdir},
    Core,
};
use async_trait::async_trait;
use futures_util::stream;
use tokio::{
    sync::Notify,
    time::{timeout, Duration},
};

async fn session_with(provider: StubProvider) -> Session {
    let core = Core::new()
        .with_plugin(stub_provider_plugin(provider))
        .build()
        .await
        .unwrap();
    core.open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_turn_adds_user_and_assistant_messages() {
    let session = session_with(StubProvider::responding("stub", "answer")).await;
    session.send_text("question").await.unwrap();
    let mut snapshots = session.subscribe();
    timeout(Duration::from_secs(1), async {
        loop {
            if snapshots.borrow().messages.len() == 2 {
                break;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.messages[0].role, Role::User);
    assert_eq!(snapshot.messages[1].role, Role::Assistant);
    assert_eq!(snapshot.active_turn, None);
}

#[tokio::test]
async fn rejects_a_second_active_turn_and_cancels_the_first() {
    let gate = Arc::new(Notify::new());
    let session = session_with(StubProvider::blocked("stub", gate)).await;
    session.send_text("first").await.unwrap();
    assert!(matches!(
        session.send_text("second").await,
        Err(Error::SessionBusy)
    ));
    assert!(session.cancel_turn().await.unwrap());
    let mut snapshots = session.subscribe();
    timeout(Duration::from_secs(1), async {
        while snapshots.borrow().active_turn.is_some() {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(session.snapshot().messages.len(), 1);
}

#[tokio::test]
async fn core_shutdown_cancels_descendant_sessions() {
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::blocked(
            "stub",
            Arc::new(Notify::new()),
        )))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap();
    session.send_text("first").await.unwrap();
    core.shutdown();
    timeout(Duration::from_secs(1), async {
        while !session.is_cancelled() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn context_is_ordered_and_contains_content() {
    let mut context = Context::default();
    context.push(ContextPart {
        priority: ContextPriority::Low,
        source: ContextSource::User,
        content: "low".into(),
    });
    context.push(ContextPart {
        priority: ContextPriority::Persistent,
        source: ContextSource::Core,
        content: "persistent".into(),
    });
    assert_eq!(context.parts()[0].content, "persistent");
}

#[tokio::test]
async fn registry_ids_are_deterministic() {
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding("z", "")))
        .with_plugin(stub_provider_plugin(StubProvider::responding("a", "")))
        .build()
        .await
        .unwrap();
    assert_eq!(
        core.providers().ids(),
        vec![ProviderId::from("a"), ProviderId::from("z")]
    );
}

struct RecordingPlugin {
    events: Arc<Mutex<Vec<RuntimeEvent>>>,
}

#[async_trait]
impl Plugin for RecordingPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("recording")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_before_user_message("recording-message", 0, self.clone())?;
        registrar.register_runtime_event("recording-events", 0, self)
    }
}

#[async_trait]
impl BeforeUserMessageHook for RecordingPlugin {
    async fn before_user_message(
        &self,
        _context: &HookContext,
        message: &mut Message,
    ) -> Result<BeforeHookResult> {
        message.metadata.insert("hooked".into(), true.into());
        Ok(BeforeHookResult::Continue)
    }
}

#[async_trait]
impl RuntimeEventHook for RecordingPlugin {
    async fn on_event(&self, event: &RuntimeEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn before_hooks_mutate_input_and_notifications_observe_events() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding(
            "stub", "answer",
        )))
        .with_plugin(Arc::new(RecordingPlugin {
            events: events.clone(),
        }))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap();

    session.send_text("question").await.unwrap();
    assert_eq!(session.snapshot().messages[0].metadata["hooked"], true);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(event, RuntimeEvent::TurnStarted { .. })));
}

struct ReplacingUserMessagePlugin;

#[async_trait]
impl Plugin for ReplacingUserMessagePlugin {
    fn id(&self) -> PluginId {
        PluginId::from("replacing-user-message")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_before_user_message("replace-history", 0, self)
    }
}

#[async_trait]
impl BeforeUserMessageHook for ReplacingUserMessagePlugin {
    async fn before_user_message(
        &self,
        context: &HookContext,
        _message: &mut Message,
    ) -> Result<BeforeHookResult> {
        let snapshot = context.history().snapshot().await?;
        if let Some(first) = snapshot.messages.first() {
            context
                .history()
                .replace_range(
                    snapshot.revision,
                    first.id,
                    first.id,
                    vec![Message::text(Role::System, "hook replacement")],
                )
                .await?;
        }
        Ok(BeforeHookResult::Continue)
    }
}

#[tokio::test]
async fn before_user_hook_can_replace_history_without_deadlocking_actor() {
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding(
            "stub", "answer",
        )))
        .with_plugin(Arc::new(ReplacingUserMessagePlugin))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap();

    timeout(Duration::from_secs(1), session.send_text("first"))
        .await
        .unwrap()
        .unwrap();
    wait_for_turn(&session).await;
    timeout(Duration::from_secs(1), session.send_text("second"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(session.snapshot().messages[0].role, Role::System);
    assert!(matches!(
        &session.snapshot().messages[0].content[0],
        MessagePart::Text { text } if text == "hook replacement"
    ));
}

#[tokio::test]
async fn closed_session_hook_context_returns_session_closed_for_history() {
    let session = session_with(StubProvider::responding("stub", "answer")).await;
    let context = HookContext {
        project_id: ProjectId::new(),
        session_id: session.id(),
    };

    session.close().await.unwrap();
    timeout(Duration::from_secs(1), async {
        loop {
            match context.history().snapshot().await {
                Err(Error::SessionClosed) => break,
                Err(Error::ChannelClosed) | Ok(_) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected history error: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

struct ScriptedProvider {
    requests: Arc<Mutex<Vec<ProviderRequest>>>,
    responses: Mutex<VecDeque<Vec<ProviderEvent>>>,
}

struct ServicesPlugin {
    id: PluginId,
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
}

impl ServicesPlugin {
    fn new(id: &str, provider: Arc<dyn Provider>, tools: Vec<Arc<dyn Tool>>) -> Arc<Self> {
        Arc::new(Self {
            id: PluginId::from(id),
            provider,
            tools,
        })
    }
}

#[async_trait]
impl Plugin for ServicesPlugin {
    fn id(&self) -> PluginId {
        self.id.clone()
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_provider(0, self.provider.clone())?;
        for tool in &self.tools {
            registrar.register_tool(0, tool.clone())?;
        }
        Ok(())
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from("scripted")
    }

    async fn get_models(&self) -> Result<Vec<Model>> {
        Ok(Vec::new())
    }

    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::Provider("script exhausted".into()))?;
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

struct RecordingTool {
    executions: Arc<Mutex<Vec<(serde_json::Value, PathBuf)>>>,
}

struct ContextPlugin;

#[async_trait]
impl Plugin for ContextPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("context")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_context_contribution("test-context", 0, self)
    }
}

#[async_trait]
impl ContextContributionHook for ContextPlugin {
    async fn contribute_context(
        &self,
        hook_context: &HookContext,
        workdir: Arc<dyn Workdir>,
        context: &mut Context,
    ) -> Result<()> {
        context.push(ContextPart {
            priority: ContextPriority::Persistent,
            source: ContextSource::Plugin(self.id().to_string()),
            content: format!(
                "project={} session={} root={}",
                hook_context.project_id,
                hook_context.session_id,
                workdir.root().display()
            ),
        });
        Ok(())
    }
}

struct CancellingToolPlugin;

#[async_trait]
impl Plugin for CancellingToolPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("cancel-tool")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_before_tool_execution("cancel-tool-call", 0, self)
    }
}

#[async_trait]
impl BeforeToolExecutionHook for CancellingToolPlugin {
    async fn before_tool_execution(
        &self,
        context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult> {
        context.call.arguments["hooked"] = true.into();
        Ok(BeforeHookResult::Cancel {
            reason: "tool blocked".into(),
        })
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn id(&self) -> ToolId {
        ToolId::from("inspect")
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "inspect".into(),
            description: "Inspect a path".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        self.executions
            .lock()
            .unwrap()
            .push((input, context.workdir.root().to_path_buf()));
        Ok(ToolOutput {
            content: "contents".into(),
            is_error: false,
        })
    }
}

#[tokio::test]
async fn plugin_context_reaches_scripted_provider() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from([vec![
            ProviderEvent::TextDelta {
                text: "answer".into(),
            },
            ProviderEvent::Finished {
                reason: FinishReason::Stop,
            },
        ]])),
    };
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "scripted-services",
            Arc::new(provider),
            Vec::new(),
        ))
        .with_plugin(Arc::new(ContextPlugin))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/context-project")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("scripted"),
            model: "test".into(),
        })
        .await
        .unwrap();

    session.send_text("question").await.unwrap();
    let mut snapshots = session.subscribe();
    timeout(Duration::from_secs(1), async {
        while snapshots.borrow().active_turn.is_some() {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].context.parts().len(), 1);
    assert!(requests[0].context.parts()[0]
        .content
        .contains("root=/context-project"));
    assert!(requests[0].context.parts()[0]
        .content
        .contains(&session.id().to_string()));
}

#[tokio::test]
async fn tool_hook_cancellation_prevents_execution() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        responses: Mutex::new(VecDeque::from([vec![
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("blocked-call".into()),
                name: Some("inspect".into()),
                arguments: "{\"path\":\"secret\"}".into(),
            },
            ProviderEvent::Finished {
                reason: FinishReason::ToolCalls,
            },
        ]])),
    };
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "scripted-services",
            Arc::new(provider),
            vec![Arc::new(RecordingTool {
                executions: executions.clone(),
            })],
        ))
        .with_plugin(Arc::new(CancellingToolPlugin))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/project")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("scripted"),
            model: "test".into(),
        })
        .await
        .unwrap();

    session.send_text("question").await.unwrap();
    let mut snapshots = session.subscribe();
    timeout(Duration::from_secs(1), async {
        while snapshots.borrow().active_turn.is_some() {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    assert!(executions.lock().unwrap().is_empty());
    assert_eq!(
        session.snapshot().last_error.as_deref(),
        Some("hook cancelled the operation: tool blocked")
    );
}

#[tokio::test]
async fn agent_streams_and_repeats_after_executing_tools_in_project_workdir() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executions = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from([
            vec![
                ProviderEvent::ReasoningDelta {
                    text: "need file".into(),
                },
                ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_external_1".into()),
                    name: Some("inspect".into()),
                    arguments: "{\"path\":\"src/lib.rs\"}".into(),
                },
                ProviderEvent::Finished {
                    reason: FinishReason::ToolCalls,
                },
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "final answer".into(),
                },
                ProviderEvent::Finished {
                    reason: FinishReason::Stop,
                },
            ],
        ])),
    };
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "scripted-services",
            Arc::new(provider),
            vec![Arc::new(RecordingTool {
                executions: executions.clone(),
            })],
        ))
        .build()
        .await
        .unwrap();
    let mut events = core.subscribe();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/project")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("scripted"),
            model: "test".into(),
        })
        .await
        .unwrap();

    session.send_text("question").await.unwrap();
    let mut snapshots = session.subscribe();
    timeout(Duration::from_secs(1), async {
        while snapshots.borrow().active_turn.is_some() {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let snapshot = session.snapshot();
    assert_eq!(snapshot.messages.len(), 4);
    assert!(matches!(
        &snapshot.messages[1].content[1],
        MessagePart::ToolCall { id, .. } if id.as_str() == "call_external_1"
    ));
    assert!(matches!(
        &snapshot.messages[2].content[0],
        MessagePart::ToolResult { call_id, content, .. }
            if call_id.as_str() == "call_external_1" && content == "contents"
    ));
    assert_eq!(executions.lock().unwrap()[0].1, PathBuf::from("/project"));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools[0].name, "inspect");
    assert_eq!(requests[1].messages.len(), 3);
    drop(requests);

    let mut saw_text_delta = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event,
            RuntimeEvent::ProviderEvent {
                event: ProviderEvent::TextDelta { ref text },
                ..
            } if text == "final answer"
        ) {
            saw_text_delta = true;
        }
    }
    assert!(saw_text_delta);
}

struct MemoryStore {
    messages: Mutex<BTreeMap<SessionId, Vec<Message>>>,
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load_messages(&self, session_id: SessionId) -> Result<Vec<Message>> {
        Ok(self
            .messages
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()> {
        self.messages
            .lock()
            .unwrap()
            .entry(session_id)
            .or_default()
            .push(message.clone());
        Ok(())
    }

    async fn replace_messages(&self, session_id: SessionId, messages: &[Message]) -> Result<()> {
        self.messages
            .lock()
            .unwrap()
            .insert(session_id, messages.to_vec());
        Ok(())
    }
}

struct StorePlugin {
    store: Arc<MemoryStore>,
}

#[async_trait]
impl Plugin for StorePlugin {
    fn id(&self) -> PluginId {
        PluginId::from("memory-persistence")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_session_store_factory(0, self)
    }
}

#[async_trait]
impl SessionStoreFactory for StorePlugin {
    fn id(&self) -> SessionStoreFactoryId {
        SessionStoreFactoryId::from("memory")
    }

    async fn open(&self, _context: &SessionStoreContext) -> Result<Option<Arc<dyn SessionStore>>> {
        Ok(Some(self.store.clone()))
    }
}

#[tokio::test]
async fn session_store_is_selected_by_a_plugin_factory() {
    let id = SessionId::new();
    let existing = Message::text(Role::Assistant, "restored");
    let store = Arc::new(MemoryStore {
        messages: Mutex::new(BTreeMap::from([(id, vec![existing.clone()])])),
    });
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding(
            "stub", "answer",
        )))
        .with_plugin(Arc::new(StorePlugin {
            store: store.clone(),
        }))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: Some(id),
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap();

    assert_eq!(session.snapshot().messages, vec![existing]);
    session.send_text("question").await.unwrap();
    assert_eq!(store.messages.lock().unwrap()[&id][1].role, Role::User);
}

struct FailingRuntimePlugin;

#[async_trait]
impl Plugin for FailingRuntimePlugin {
    fn id(&self) -> PluginId {
        PluginId::from("failing-runtime")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_runtime_event("failing-runtime", 0, self)
    }
}

#[async_trait]
impl RuntimeEventHook for FailingRuntimePlugin {
    async fn on_event(&self, _event: &RuntimeEvent) -> Result<()> {
        Err(Error::Plugin("expected runtime hook failure".into()))
    }
}

#[tokio::test]
async fn runtime_hook_failures_are_reported_without_failing_operations() {
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding(
            "stub", "answer",
        )))
        .with_plugin(Arc::new(FailingRuntimePlugin))
        .build()
        .await
        .unwrap();
    let mut events = core.subscribe();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap();

    session.send_text("question").await.unwrap();
    let mut reported = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event,
            RuntimeEvent::HookFailed { ref plugin_id, .. }
                if plugin_id == &PluginId::from("failing-runtime")
        ) {
            reported = true;
        }
    }
    assert!(reported);
}

#[tokio::test]
async fn duplicate_owned_services_abort_the_build() {
    let result = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding("same", "")))
        .with_plugin(Arc::new(ServicesPlugin {
            id: PluginId::from("another-owner"),
            provider: Arc::new(StubProvider::responding("same", "")),
            tools: Vec::new(),
        }))
        .build()
        .await;

    assert!(matches!(result, Err(Error::DuplicateProvider(id)) if id == ProviderId::from("same")));
}

struct ProviderRequestPlugin;

#[async_trait]
impl Plugin for ProviderRequestPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("provider-request-hook")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_before_provider_request("provider-model", 10, self)
    }
}

#[async_trait]
impl BeforeProviderRequestHook for ProviderRequestPlugin {
    async fn before_provider_request(
        &self,
        _context: &HookContext,
        request: &mut ProviderRequest,
    ) -> Result<BeforeHookResult> {
        assert_eq!(request.context.parts().len(), 1);
        request.model = "hook-model".into();
        request.messages[0]
            .metadata
            .insert("provider-hook".into(), true.into());
        Ok(BeforeHookResult::Continue)
    }
}

#[tokio::test]
async fn before_provider_hook_runs_after_context_and_mutates_request() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from([vec![
            ProviderEvent::TextDelta { text: "ok".into() },
            ProviderEvent::Finished {
                reason: FinishReason::Stop,
            },
        ]])),
    };
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "services",
            Arc::new(provider),
            Vec::new(),
        ))
        .with_plugin(Arc::new(ContextPlugin))
        .with_plugin(Arc::new(ProviderRequestPlugin))
        .build()
        .await
        .unwrap();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/provider-hook")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("scripted"),
            model: "original".into(),
        })
        .await
        .unwrap();

    session.send_text("question").await.unwrap();
    wait_for_turn(&session).await;
    let requests = requests.lock().unwrap();
    assert_eq!(requests[0].model, "hook-model");
    assert_eq!(requests[0].messages[0].metadata["provider-hook"], true);
}

struct OrderedToolHook {
    label: &'static str,
    cancel: bool,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl BeforeToolExecutionHook for OrderedToolHook {
    async fn before_tool_execution(
        &self,
        context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult> {
        self.calls.lock().unwrap().push(self.label);
        if self.label == "first" {
            context.call.arguments["first"] = true.into();
        } else {
            assert_eq!(context.call.arguments["first"], true);
        }
        if self.cancel {
            Ok(BeforeHookResult::Cancel {
                reason: "ordered cancellation".into(),
            })
        } else {
            Ok(BeforeHookResult::Continue)
        }
    }
}

struct OrderedToolPlugin {
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Plugin for OrderedToolPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("ordered-tool-hooks")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        for (id, priority, cancel) in [
            ("first", 20, false),
            ("cancel", 10, true),
            ("last", 0, false),
        ] {
            registrar.register_before_tool_execution(
                id,
                priority,
                Arc::new(OrderedToolHook {
                    label: id,
                    cancel,
                    calls: self.calls.clone(),
                }),
            )?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn before_tool_hooks_mutate_cancel_and_stop_in_priority_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let executions = Arc::new(Mutex::new(Vec::new()));
    let provider = one_tool_call_provider();
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "services",
            Arc::new(provider),
            vec![Arc::new(RecordingTool {
                executions: executions.clone(),
            })],
        ))
        .with_plugin(Arc::new(OrderedToolPlugin {
            calls: calls.clone(),
        }))
        .build()
        .await
        .unwrap();
    let session = open_scripted_session(&core, "/ordered-hooks").await;

    session.send_text("question").await.unwrap();
    wait_for_turn(&session).await;
    assert_eq!(*calls.lock().unwrap(), vec!["first", "cancel"]);
    assert!(executions.lock().unwrap().is_empty());
    assert_eq!(
        session.snapshot().last_error.as_deref(),
        Some("hook cancelled the operation: ordered cancellation")
    );
}

struct TransformOutputPlugin;

#[async_trait]
impl Plugin for TransformOutputPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("transform-output")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_after_tool_execution("transform-output", 0, self)
    }
}

#[async_trait]
impl AfterToolExecutionHook for TransformOutputPlugin {
    async fn after_tool_execution(
        &self,
        context: &ToolExecutionContext,
        output: &mut ToolOutput,
    ) -> Result<()> {
        assert_eq!(context.call.name, "inspect");
        output.content.push_str("-transformed");
        Ok(())
    }
}

#[tokio::test]
async fn after_tool_hook_transforms_output_before_message_persistence() {
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "services",
            Arc::new(one_tool_call_provider_with_final()),
            vec![Arc::new(RecordingTool {
                executions: Arc::new(Mutex::new(Vec::new())),
            })],
        ))
        .with_plugin(Arc::new(TransformOutputPlugin))
        .build()
        .await
        .unwrap();
    let session = open_scripted_session(&core, "/after-hook").await;

    session.send_text("question").await.unwrap();
    wait_for_turn(&session).await;
    assert!(matches!(
        &session.snapshot().messages[2].content[0],
        MessagePart::ToolResult { content, .. } if content == "contents-transformed"
    ));
}

struct RootLayer;

impl WorkdirLayer for RootLayer {
    fn id(&self) -> WorkdirLayerId {
        WorkdirLayerId::from("root-marker")
    }

    fn layer(&self, _context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        Arc::new(StubWorkdir::new(inner.root().join("layered")))
    }
}

struct LayerPlugin;

#[async_trait]
impl Plugin for LayerPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("workdir-layer")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_workdir_layer(0, Arc::new(RootLayer))
    }
}

#[tokio::test]
async fn workdir_layer_is_applied_only_when_its_plugin_is_present() {
    let plain = Core::new().build().await.unwrap();
    let layered = Core::new()
        .with_plugin(Arc::new(LayerPlugin))
        .build()
        .await
        .unwrap();

    assert_eq!(
        plain
            .open_project("plain", Arc::new(StubWorkdir::new("/host")))
            .get_workdir()
            .root(),
        PathBuf::from("/host")
    );
    assert_eq!(
        layered
            .open_project("layered", Arc::new(StubWorkdir::new("/host")))
            .get_workdir()
            .root(),
        PathBuf::from("/host/layered")
    );
}

struct FeatureTool;

#[async_trait]
impl Tool for FeatureTool {
    fn id(&self) -> ToolId {
        ToolId::from("feature")
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "feature".into(),
            description: "emit a feature event".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn execute(&self, _input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        assert_eq!(context.provider_id(), &ProviderId::from("scripted"));
        assert_eq!(context.model(), "test");
        assert!(context.providers().get(context.provider_id()).is_some());
        assert_eq!(context.messages().len(), 2);
        context
            .emit_feature(
                "started",
                serde_json::json!({"history": context.messages().len()}),
            )
            .await?;
        Ok(ToolOutput {
            content: "emitted".into(),
            is_error: false,
        })
    }
}

#[tokio::test]
async fn tool_context_emits_owner_namespaced_feature_events() {
    let provider = ScriptedProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        responses: Mutex::new(VecDeque::from([
            vec![
                ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some("feature-call".into()),
                    name: Some("feature".into()),
                    arguments: "{}".into(),
                },
                ProviderEvent::Finished {
                    reason: FinishReason::ToolCalls,
                },
            ],
            vec![ProviderEvent::Finished {
                reason: FinishReason::Stop,
            }],
        ])),
    };
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "feature-owner",
            Arc::new(provider),
            vec![Arc::new(FeatureTool)],
        ))
        .build()
        .await
        .unwrap();
    let mut events = core.subscribe();
    let session = open_scripted_session(&core, "/feature").await;

    session.send_text("question").await.unwrap();
    wait_for_turn(&session).await;
    let mut feature = None;
    while let Ok(event) = events.try_recv() {
        if let RuntimeEvent::FeatureEvent {
            plugin_id,
            name,
            payload,
        } = event
        {
            feature = Some((plugin_id, name, payload));
        }
    }
    let (plugin_id, name, payload) = feature.expect("feature event was emitted");
    assert_eq!(plugin_id, PluginId::from("feature-owner"));
    assert_eq!(name, "started");
    assert_eq!(payload["history"], 2);
}

async fn wait_for_turn(session: &Session) {
    let mut snapshots = session.subscribe();
    timeout(Duration::from_secs(1), async {
        while snapshots.borrow().active_turn.is_some() {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn open_scripted_session(core: &Core, root: &str) -> Session {
    core.open_project("test", Arc::new(StubWorkdir::new(root)))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("scripted"),
            model: "test".into(),
        })
        .await
        .unwrap()
}

struct HistoryCommand;

#[async_trait]
impl Command for HistoryCommand {
    fn id(&self) -> CommandId {
        CommandId::from("test.history")
    }

    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "history".into(),
            description: "Replace the first message".into(),
            usage: "/history".into(),
        }
    }

    async fn execute(
        &self,
        _invocation: CommandInvocation,
        context: CommandContext,
    ) -> Result<CommandResult> {
        let snapshot = context.history().snapshot().await?;
        let first = snapshot.messages[0].id;
        context
            .history()
            .replace_range(
                snapshot.revision,
                first,
                first,
                vec![Message::text(Role::System, "command replacement")],
            )
            .await?;
        Ok(CommandResult {
            content: "replaced".into(),
        })
    }
}

struct CommandPlugin;

#[async_trait]
impl Plugin for CommandPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("commands")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_command(0, Arc::new(HistoryCommand))
    }
}

#[tokio::test]
async fn commands_can_replace_history_without_deadlocking_the_actor() {
    let core = Core::new()
        .with_plugin(stub_provider_plugin(StubProvider::responding(
            "stub", "answer",
        )))
        .with_plugin(Arc::new(CommandPlugin))
        .build()
        .await
        .unwrap();
    let mut events = core.subscribe();
    let session = core
        .open_project("test", Arc::new(StubWorkdir::new("/tmp")))
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from("stub"),
            model: "test".into(),
        })
        .await
        .unwrap();
    session.send_text("question").await.unwrap();
    wait_for_turn(&session).await;

    assert_eq!(session.commands()[0].name, "history");
    let output = timeout(
        Duration::from_secs(1),
        session.dispatch_command(parse_command_invocation("/history").unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.content, "replaced");
    assert_eq!(session.snapshot().messages[0].role, Role::System);
    assert_eq!(session.snapshot().revision, 3);
    let mut saw_history = false;
    let mut saw_command = false;
    while let Ok(event) = events.try_recv() {
        saw_history |= matches!(event, RuntimeEvent::HistoryReplaced { .. });
        saw_command |= matches!(event, RuntimeEvent::CommandCompleted { .. });
    }
    assert!(saw_history && saw_command);
}

struct ReplacingProviderHook;

#[async_trait]
impl BeforeProviderRequestHook for ReplacingProviderHook {
    async fn before_provider_request(
        &self,
        context: &HookContext,
        _request: &mut ProviderRequest,
    ) -> Result<BeforeHookResult> {
        let snapshot = context.history().snapshot().await?;
        let first = snapshot.messages[0].id;
        context
            .history()
            .replace_range(
                snapshot.revision,
                first,
                first,
                vec![Message::text(Role::User, "provider replacement")],
            )
            .await?;
        Ok(BeforeHookResult::Continue)
    }
}

struct ReplacingProviderPlugin;

#[async_trait]
impl Plugin for ReplacingProviderPlugin {
    fn id(&self) -> PluginId {
        PluginId::from("replace-before-provider")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_before_provider_request(
            "replace-history",
            0,
            Arc::new(ReplacingProviderHook),
        )
    }
}

#[tokio::test]
async fn before_provider_history_replacement_resynchronizes_the_request() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from([vec![ProviderEvent::Finished {
            reason: FinishReason::Stop,
        }]])),
    };
    let core = Core::new()
        .with_plugin(ServicesPlugin::new(
            "services",
            Arc::new(provider),
            Vec::new(),
        ))
        .with_plugin(Arc::new(ReplacingProviderPlugin))
        .build()
        .await
        .unwrap();
    let session = open_scripted_session(&core, "/provider-history").await;

    session.send_text("original").await.unwrap();
    wait_for_turn(&session).await;
    assert!(matches!(
        &requests.lock().unwrap()[0].messages[0].content[0],
        MessagePart::Text { text } if text == "provider replacement"
    ));
}

fn one_tool_call_provider() -> ScriptedProvider {
    ScriptedProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        responses: Mutex::new(VecDeque::from([vec![
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("inspect-call".into()),
                name: Some("inspect".into()),
                arguments: "{}".into(),
            },
            ProviderEvent::Finished {
                reason: FinishReason::ToolCalls,
            },
        ]])),
    }
}

fn one_tool_call_provider_with_final() -> ScriptedProvider {
    let provider = one_tool_call_provider();
    provider.responses.lock().unwrap().push_back(vec![
        ProviderEvent::TextDelta {
            text: "done".into(),
        },
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
        },
    ]);
    provider
}
