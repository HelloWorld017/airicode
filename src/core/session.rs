use std::{collections::BTreeMap, path::Path, sync::Arc};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    BeforeHookResult, CommandOutput, CommandSpec, Context, Core, Error, EventSink, HookContext,
    HookRegistry, Message, PluginId, ProjectId, Provider, ProviderEvent, ProviderId,
    ProviderRegistry, ProviderRequest, Result, Role, RuntimeEvent, SessionId, SessionStore,
    ToolCallDraft, ToolCallId, ToolContext, ToolExecutionContext, ToolRegistry, ToolServices,
    TurnId, Workdir,
};

const MAX_PROVIDER_ROUNDS: usize = 16;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SessionSnapshot {
    pub id: SessionId,
    pub messages: Vec<Message>,
    pub active_turn: Option<TurnId>,
    pub last_error: Option<String>,
}

enum Command {
    Send {
        message: Message,
        reply: oneshot::Sender<Result<TurnId>>,
    },
    Cancel {
        reply: oneshot::Sender<bool>,
    },
    Close,
}

struct TurnFinished {
    id: TurnId,
    result: Result<()>,
}

struct TurnUpdate {
    id: TurnId,
    kind: TurnUpdateKind,
}

enum TurnUpdateKind {
    Provider(ProviderEvent),
    Message {
        message: Message,
        reply: oneshot::Sender<Result<()>>,
    },
}

struct ActiveTurn {
    id: TurnId,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct Session {
    id: SessionId,
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<SessionSnapshot>,
    cancellation: CancellationToken,
}

pub(crate) struct SessionSpawn {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub provider: Arc<dyn Provider>,
    pub provider_id: ProviderId,
    pub model: String,
    pub hooks: HookRegistry,
    pub tools: ToolRegistry,
    pub providers: ProviderRegistry,
    pub workdir: Arc<dyn Workdir>,
    pub core: Core,
    pub store: Option<Arc<dyn SessionStore>>,
    pub cancellation: CancellationToken,
}

impl Session {
    pub(crate) async fn spawn(input: SessionSpawn) -> Result<Self> {
        let SessionSpawn {
            id,
            project_id,
            provider,
            provider_id,
            model,
            hooks,
            tools,
            providers,
            workdir,
            core,
            store,
            cancellation,
        } = input;
        let messages = match &store {
            Some(store) => store.load_messages(id).await?,
            None => Vec::new(),
        };
        let initial = SessionSnapshot {
            id,
            messages,
            active_turn: None,
            last_error: None,
        };
        let (snapshot_tx, snapshot) = watch::channel(initial);
        let (commands, command_rx) = mpsc::channel(32);
        let actor_cancellation = cancellation.clone();
        tokio::spawn(run_actor(Actor {
            id,
            project_id,
            provider,
            provider_id,
            model,
            hooks,
            tools,
            providers,
            workdir,
            core,
            store,
            cancellation: actor_cancellation,
            command_rx,
            snapshot_tx,
            active: None,
        }));
        Ok(Self {
            id,
            commands,
            snapshot,
            cancellation,
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }
    pub fn snapshot(&self) -> SessionSnapshot {
        self.snapshot.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<SessionSnapshot> {
        self.snapshot.clone()
    }
    pub fn get_message(&self, id: super::MessageId) -> Option<Message> {
        self.snapshot()
            .messages
            .into_iter()
            .find(|message| message.id == id)
    }

    pub async fn send_message(&self, message: Message) -> Result<TurnId> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Send { message, reply })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)?
    }

    pub async fn send_text(&self, text: impl Into<String>) -> Result<TurnId> {
        self.send_message(Message::text(Role::User, text)).await
    }

    pub async fn cancel_turn(&self) -> Result<bool> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Cancel { reply })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)
    }

    pub async fn close(&self) -> Result<()> {
        self.commands
            .send(Command::Close)
            .await
            .map_err(|_| Error::SessionClosed)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

struct Actor {
    id: SessionId,
    project_id: ProjectId,
    provider: Arc<dyn Provider>,
    provider_id: ProviderId,
    model: String,
    hooks: HookRegistry,
    tools: ToolRegistry,
    providers: ProviderRegistry,
    workdir: Arc<dyn Workdir>,
    core: Core,
    store: Option<Arc<dyn SessionStore>>,
    cancellation: CancellationToken,
    command_rx: mpsc::Receiver<Command>,
    snapshot_tx: watch::Sender<SessionSnapshot>,
    active: Option<ActiveTurn>,
}

async fn run_actor(mut actor: Actor) {
    let (finished_tx, mut finished_rx) = mpsc::channel::<TurnFinished>(1);
    let (update_tx, mut update_rx) = mpsc::channel::<TurnUpdate>(32);
    loop {
        tokio::select! {
            _ = actor.cancellation.cancelled() => break,
            Some(finished) = finished_rx.recv() => actor.finish_turn(finished).await,
            Some(update) = update_rx.recv() => actor.apply_turn_update(update).await,
            command = actor.command_rx.recv() => match command {
                Some(Command::Send { message, reply }) => {
                    let result = actor.start_turn(message, finished_tx.clone(), update_tx.clone()).await;
                    let _ = reply.send(result);
                }
                Some(Command::Cancel { reply }) => {
                    let cancelled = actor.active.as_ref().map(|active| { active.cancellation.cancel(); true }).unwrap_or(false);
                    let _ = reply.send(cancelled);
                }
                Some(Command::Close) | None => break,
            }
        }
    }
    if let Some(active) = actor.active.take() {
        active.cancellation.cancel();
        active.task.abort();
        actor.update_snapshot(|snapshot| snapshot.active_turn = None);
        actor
            .notify(RuntimeEvent::TurnCancelled {
                session_id: actor.id,
                turn_id: active.id,
            })
            .await;
    }
    actor.cancellation.cancel();
}

impl Actor {
    async fn start_turn(
        &mut self,
        mut message: Message,
        finished_tx: mpsc::Sender<TurnFinished>,
        update_tx: mpsc::Sender<TurnUpdate>,
    ) -> Result<TurnId> {
        if self.active.is_some() {
            return Err(Error::SessionBusy);
        }
        if self.cancellation.is_cancelled() {
            return Err(Error::SessionClosed);
        }
        let hook_context = HookContext {
            project_id: self.project_id,
            session_id: self.id,
        };
        if let BeforeHookResult::Cancel { reason } = self
            .hooks
            .before_user_message(&hook_context, &mut message)
            .await?
        {
            return Err(Error::HookCancelled(reason));
        }
        self.persist_and_add(message.clone()).await?;
        let id = TurnId::new();
        let cancellation = self.cancellation.child_token();
        let provider = self.provider.clone();
        let provider_id = self.provider_id.clone();
        let model = self.model.clone();
        let messages = self.snapshot_tx.borrow().messages.clone();
        let tools = self.tools.clone();
        let providers = self.providers.clone();
        let hooks = self.hooks.clone();
        let workdir = self.workdir.clone();
        let core = self.core.clone();
        let project_id = self.project_id;
        let session_id = self.id;
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                _ = task_cancellation.cancelled() => Err(Error::Cancelled),
                result = run_turn(
                    id,
                    provider,
                    provider_id,
                    model,
                    messages,
                    project_id,
                    session_id,
                    hooks,
                    tools,
                    providers,
                    workdir,
                    core,
                    task_cancellation.clone(),
                    update_tx,
                ) => result,
            };
            let _ = finished_tx.send(TurnFinished { id, result }).await;
        });
        self.active = Some(ActiveTurn {
            id,
            cancellation,
            task,
        });
        self.update_snapshot(|snapshot| {
            snapshot.active_turn = Some(id);
            snapshot.last_error = None;
        });
        self.notify(RuntimeEvent::TurnStarted {
            session_id: self.id,
            turn_id: id,
        })
        .await;
        Ok(id)
    }

    async fn finish_turn(&mut self, finished: TurnFinished) {
        if self.active.as_ref().map(|active| active.id) != Some(finished.id) {
            return;
        }
        self.active.take();
        match finished.result {
            Ok(()) => {
                self.update_snapshot(|snapshot| snapshot.active_turn = None);
                self.notify(RuntimeEvent::TurnCompleted {
                    session_id: self.id,
                    turn_id: finished.id,
                })
                .await;
            }
            Err(Error::Cancelled) => {
                self.update_snapshot(|snapshot| snapshot.active_turn = None);
                self.notify(RuntimeEvent::TurnCancelled {
                    session_id: self.id,
                    turn_id: finished.id,
                })
                .await;
            }
            Err(error) => self.fail(finished.id, error).await,
        }
    }

    async fn apply_turn_update(&self, update: TurnUpdate) {
        if self.active.as_ref().map(|active| active.id) != Some(update.id) {
            return;
        }
        match update.kind {
            TurnUpdateKind::Provider(event) => {
                self.notify(RuntimeEvent::ProviderEvent {
                    session_id: self.id,
                    turn_id: update.id,
                    event,
                })
                .await;
            }
            TurnUpdateKind::Message { message, reply } => {
                let _ = reply.send(self.persist_and_add(message).await);
            }
        }
    }

    async fn fail(&self, turn_id: TurnId, error: Error) {
        let text = error.to_string();
        self.update_snapshot(|snapshot| {
            snapshot.active_turn = None;
            snapshot.last_error = Some(text.clone());
        });
        self.notify(RuntimeEvent::TurnFailed {
            session_id: self.id,
            turn_id,
            error: text,
        })
        .await;
    }

    async fn persist_and_add(&self, message: Message) -> Result<()> {
        if let Some(store) = &self.store {
            store.append_message(self.id, &message).await?;
        }
        self.update_snapshot(|snapshot| snapshot.messages.push(message.clone()));
        self.notify(RuntimeEvent::MessageAdded {
            session_id: self.id,
            message,
        })
        .await;
        Ok(())
    }

    fn update_snapshot(&self, update: impl FnOnce(&mut SessionSnapshot)) {
        self.snapshot_tx.send_modify(update);
    }

    async fn notify(&self, event: RuntimeEvent) {
        self.core.emit(event).await;
    }
}

#[derive(Default)]
struct ToolCallBuffer {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

type CompletedToolCall = (ToolCallId, String, serde_json::Value);

#[derive(Default)]
struct AssistantBuffer {
    text: String,
    reasoning: String,
    tool_calls: BTreeMap<u32, ToolCallBuffer>,
}

impl AssistantBuffer {
    fn push(&mut self, event: &ProviderEvent) {
        match event {
            ProviderEvent::TextDelta { text } => self.text.push_str(text),
            ProviderEvent::ReasoningDelta { text } => self.reasoning.push_str(text),
            ProviderEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                let call = self.tool_calls.entry(*index).or_default();
                if let Some(id) = id {
                    call.id = Some(id.clone());
                }
                if let Some(name) = name {
                    call.name = Some(name.clone());
                }
                call.arguments.push_str(arguments);
            }
            ProviderEvent::Usage { .. } | ProviderEvent::Finished { .. } => {}
        }
    }

    fn finish(self) -> Result<(Message, Vec<CompletedToolCall>)> {
        let mut parts = Vec::new();
        if !self.reasoning.is_empty() {
            parts.push(super::MessagePart::Reasoning {
                text: self.reasoning,
            });
        }
        if !self.text.is_empty() {
            parts.push(super::MessagePart::Text { text: self.text });
        }
        let mut calls = Vec::new();
        for (_, call) in self.tool_calls {
            let id = ToolCallId::new(
                call.id
                    .ok_or_else(|| Error::Provider("tool call is missing an id".into()))?,
            );
            let name = call
                .name
                .ok_or_else(|| Error::Provider("tool call is missing a name".into()))?;
            let arguments: serde_json::Value = serde_json::from_str(if call.arguments.is_empty() {
                "{}"
            } else {
                &call.arguments
            })
            .map_err(|error| {
                Error::Provider(format!("invalid tool arguments for {name}: {error}"))
            })?;
            parts.push(super::MessagePart::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            });
            calls.push((id, name, arguments));
        }
        Ok((Message::new(Role::Assistant, parts), calls))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    turn_id: TurnId,
    provider: Arc<dyn Provider>,
    provider_id: ProviderId,
    model: String,
    mut messages: Vec<Message>,
    project_id: ProjectId,
    session_id: SessionId,
    hooks: HookRegistry,
    tools: ToolRegistry,
    providers: ProviderRegistry,
    workdir: Arc<dyn Workdir>,
    core: Core,
    cancellation: CancellationToken,
    updates: mpsc::Sender<TurnUpdate>,
) -> Result<()> {
    let available_tools: BTreeMap<_, _> = tools
        .all()
        .into_iter()
        .map(|tool| {
            let owner = tools
                .owner(&tool.id())
                .expect("registered tool has an owner");
            (tool.definition().name, (tool, owner))
        })
        .collect();
    let definitions: Vec<_> = available_tools
        .values()
        .map(|(tool, _)| tool.definition())
        .collect();
    for _ in 0..MAX_PROVIDER_ROUNDS {
        let hook_context = HookContext {
            project_id,
            session_id,
        };
        let mut context = Context::default();
        hooks
            .contribute_context(&hook_context, workdir.clone(), &mut context)
            .await?;
        let mut request = ProviderRequest {
            model: model.clone(),
            messages: messages.clone(),
            tools: definitions.clone(),
            context,
            cancellation: cancellation.clone(),
        };
        if let BeforeHookResult::Cancel { reason } = hooks
            .before_provider_request(&hook_context, &mut request)
            .await?
        {
            return Err(Error::HookCancelled(reason));
        }
        let request_model = request.model.clone();
        let mut stream = tokio::select! {
            _ = cancellation.cancelled() => return Err(Error::Cancelled),
            stream = provider.request(request) => stream?,
        };
        let mut assistant = AssistantBuffer::default();
        loop {
            let event = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                event = stream.next() => event,
            };
            let Some(event) = event else { break };
            let event = event?;
            assistant.push(&event);
            updates
                .send(TurnUpdate {
                    id: turn_id,
                    kind: TurnUpdateKind::Provider(event),
                })
                .await
                .map_err(|_| Error::ChannelClosed)?;
        }

        let (assistant_message, calls) = assistant.finish()?;
        persist_generated(turn_id, assistant_message.clone(), &updates).await?;
        messages.push(assistant_message);
        if calls.is_empty() {
            return Ok(());
        }

        for (call_id, name, arguments) in calls {
            let mut execution_context = ToolExecutionContext {
                hook_context: hook_context.clone(),
                turn_id,
                workdir: workdir.clone(),
                call: ToolCallDraft {
                    id: call_id,
                    name,
                    arguments,
                },
            };
            if let BeforeHookResult::Cancel { reason } =
                hooks.before_tool_execution(&mut execution_context).await?
            {
                return Err(Error::HookCancelled(reason));
            }
            let call = &execution_context.call;
            let mut output = match available_tools.get(&call.name) {
                Some((tool, owner)) => {
                    let execution_workdir: Arc<dyn Workdir> = Arc::new(ExecutionWorkdir {
                        inner: workdir.clone(),
                        services: ToolServices {
                            provider_id: provider_id.clone(),
                            model: request_model.clone(),
                            providers: providers.clone(),
                            messages: messages.clone().into(),
                            events: Arc::new(ScopedEventSink {
                                core: core.clone(),
                                plugin_id: owner.clone(),
                            }),
                        },
                    });
                    let execution = tool.execute(
                        call.arguments.clone(),
                        ToolContext {
                            project_id,
                            session_id,
                            turn_id,
                            workdir: execution_workdir,
                            cancellation: cancellation.clone(),
                        },
                    );
                    match tokio::select! {
                        _ = cancellation.cancelled() => return Err(Error::Cancelled),
                        output = execution => output,
                    } {
                        Ok(output) => output,
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(error) => super::ToolOutput {
                            content: error.to_string(),
                            is_error: true,
                        },
                    }
                }
                None => super::ToolOutput {
                    content: format!("tool {} is not registered", call.name),
                    is_error: true,
                },
            };
            hooks
                .after_tool_execution(&execution_context, &mut output)
                .await?;
            let result = Message::new(
                Role::Tool,
                vec![super::MessagePart::ToolResult {
                    call_id: call.id.clone(),
                    content: output.content,
                    is_error: output.is_error,
                }],
            );
            persist_generated(turn_id, result.clone(), &updates).await?;
            messages.push(result);
        }
    }
    Err(Error::Provider(format!(
        "tool-call loop exceeded {MAX_PROVIDER_ROUNDS} provider rounds"
    )))
}

struct ScopedEventSink {
    core: Core,
    plugin_id: PluginId,
}

#[async_trait]
impl EventSink for ScopedEventSink {
    async fn emit(&self, name: String, payload: serde_json::Value) -> Result<()> {
        self.core
            .emit(RuntimeEvent::FeatureEvent {
                plugin_id: self.plugin_id.clone(),
                name,
                payload,
            })
            .await;
        Ok(())
    }
}

struct ExecutionWorkdir {
    inner: Arc<dyn Workdir>,
    services: ToolServices,
}

#[async_trait]
impl Workdir for ExecutionWorkdir {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.inner.read(path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.inner.write(path, data).await
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        self.inner.remove(path).await
    }

    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput> {
        self.inner.execute(command, cancellation).await
    }

    fn tool_services(&self) -> Option<&ToolServices> {
        Some(&self.services)
    }
}

async fn persist_generated(
    turn_id: TurnId,
    message: Message,
    updates: &mpsc::Sender<TurnUpdate>,
) -> Result<()> {
    let (reply, response) = oneshot::channel();
    updates
        .send(TurnUpdate {
            id: turn_id,
            kind: TurnUpdateKind::Message { message, reply },
        })
        .await
        .map_err(|_| Error::ChannelClosed)?;
    response.await.map_err(|_| Error::ChannelClosed)?
}
