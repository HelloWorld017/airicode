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
    BeforeHookResult, CommandCompletion, CommandContext, CommandId, CommandInvocation,
    CommandOutput, CommandRegistry, CommandResult, CommandSpec, Context, Core, Error, EventSink,
    HookContext, HookRegistry, Message, MessageId, PluginId, ProjectId, Provider, ProviderEvent,
    ProviderId, ProviderRegistry, ProviderRequest, Result, Role, RuntimeEvent, SessionId,
    SessionStore, ToolCallDraft, ToolCallId, ToolContext, ToolExecutionContext, ToolRegistry,
    ToolServices, TurnId, Workdir,
};

const MAX_PROVIDER_ROUNDS: usize = 16;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SessionSnapshot {
    pub id: SessionId,
    pub revision: u64,
    pub messages: Vec<Message>,
    pub active_turn: Option<TurnId>,
    pub active_command: Option<CommandId>,
    pub last_error: Option<String>,
}

enum SessionRequest {
    Send {
        message: Message,
        reply: oneshot::Sender<Result<TurnId>>,
    },
    Cancel {
        reply: oneshot::Sender<bool>,
    },
    DispatchCommand {
        invocation: CommandInvocation,
        reply: oneshot::Sender<Result<CommandResult>>,
    },
    HistorySnapshot {
        reply: oneshot::Sender<HistorySnapshot>,
    },
    ReplaceHistory {
        operation: HistoryOperation,
        expected_revision: u64,
        reply: oneshot::Sender<Result<HistorySnapshot>>,
    },
    Close,
}

struct TurnFinished {
    id: TurnId,
    result: Result<()>,
}

struct PreparationFinished {
    result: Result<Message>,
}

struct CommandFinished {
    id: CommandId,
    result: Result<CommandResult>,
    reply: oneshot::Sender<Result<CommandResult>>,
}

struct TurnUpdate {
    id: TurnId,
    kind: TurnUpdateKind,
}

enum TurnUpdateKind {
    Provider(ProviderEvent),
    Message {
        message: Message,
        reply: oneshot::Sender<Result<HistorySnapshot>>,
    },
}

struct ActiveTurn {
    id: TurnId,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

struct ActivePreparation {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
    reply: oneshot::Sender<Result<TurnId>>,
}

struct ActiveCommand {
    id: CommandId,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct HistorySnapshot {
    pub revision: u64,
    pub messages: Vec<Message>,
}

enum HistoryOperation {
    ReplaceRange {
        first: MessageId,
        last: MessageId,
        replacement: Vec<Message>,
    },
    TruncateBefore(MessageId),
    TruncateAfter(MessageId),
}

#[derive(Clone)]
pub struct SessionHistory {
    session_id: SessionId,
    requests: mpsc::Sender<SessionRequest>,
}

impl SessionHistory {
    pub(crate) fn closed(session_id: SessionId) -> Self {
        let (requests, request_rx) = mpsc::channel(1);
        drop(request_rx);
        Self {
            session_id,
            requests,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub async fn snapshot(&self) -> Result<HistorySnapshot> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(SessionRequest::HistorySnapshot { reply })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)
    }

    pub async fn replace_range(
        &self,
        expected_revision: u64,
        first: MessageId,
        last: MessageId,
        replacement: Vec<Message>,
    ) -> Result<HistorySnapshot> {
        self.replace(
            expected_revision,
            HistoryOperation::ReplaceRange {
                first,
                last,
                replacement,
            },
        )
        .await
    }

    pub async fn replace_messages(
        &self,
        expected_revision: u64,
        first: MessageId,
        last: MessageId,
        replacement: Vec<Message>,
    ) -> Result<HistorySnapshot> {
        self.replace_range(expected_revision, first, last, replacement)
            .await
    }

    pub async fn truncate_before(
        &self,
        expected_revision: u64,
        anchor: MessageId,
    ) -> Result<HistorySnapshot> {
        self.replace(expected_revision, HistoryOperation::TruncateBefore(anchor))
            .await
    }

    pub async fn truncate_after(
        &self,
        expected_revision: u64,
        anchor: MessageId,
    ) -> Result<HistorySnapshot> {
        self.replace(expected_revision, HistoryOperation::TruncateAfter(anchor))
            .await
    }

    async fn replace(
        &self,
        expected_revision: u64,
        operation: HistoryOperation,
    ) -> Result<HistorySnapshot> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(SessionRequest::ReplaceHistory {
                operation,
                expected_revision,
                reply,
            })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)?
    }
}

#[derive(Clone)]
pub struct Session {
    id: SessionId,
    requests: mpsc::Sender<SessionRequest>,
    command_registry: CommandRegistry,
    hook_context: HookContext,
    workdir: Arc<dyn Workdir>,
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
    pub commands: CommandRegistry,
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
            commands: command_registry,
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
            revision: 0,
            messages,
            active_turn: None,
            active_command: None,
            last_error: None,
        };
        let (snapshot_tx, snapshot) = watch::channel(initial);
        let (requests, request_rx) = mpsc::channel(32);
        let history = SessionHistory {
            session_id: id,
            requests: requests.clone(),
        };
        let hook_services = super::install_hook_services(
            id,
            history,
            providers.clone(),
            provider_id.clone(),
            model.clone(),
            cancellation.clone(),
        );
        let hook_context = HookContext {
            project_id,
            session_id: id,
        };
        let actor_cancellation = cancellation.clone();
        let session_workdir = workdir.clone();
        tokio::spawn(run_actor(Actor {
            id,
            project_id,
            provider,
            provider_id,
            model,
            hooks,
            tools,
            commands: command_registry.clone(),
            providers,
            workdir,
            core,
            store,
            cancellation: actor_cancellation,
            request_rx,
            snapshot_tx,
            active: None,
            active_preparation: None,
            active_command: None,
            hook_services,
        }));
        Ok(Self {
            id,
            requests,
            command_registry,
            hook_context,
            workdir: session_workdir,
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
        self.requests
            .send(SessionRequest::Send { message, reply })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)?
    }

    pub async fn send_text(&self, text: impl Into<String>) -> Result<TurnId> {
        self.send_message(Message::text(Role::User, text)).await
    }

    pub async fn cancel_turn(&self) -> Result<bool> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(SessionRequest::Cancel { reply })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)
    }

    pub async fn cancel_command(&self) -> Result<bool> {
        self.cancel_turn().await
    }

    pub async fn cancel_active(&self) -> Result<bool> {
        self.cancel_turn().await
    }

    pub fn command_descriptors(&self) -> Vec<super::CommandDescriptor> {
        self.command_registry.descriptors()
    }

    pub fn commands(&self) -> Vec<super::CommandDescriptor> {
        self.command_descriptors()
    }

    pub fn history(&self) -> SessionHistory {
        SessionHistory {
            session_id: self.id,
            requests: self.requests.clone(),
        }
    }

    pub async fn complete_command(
        &self,
        invocation: CommandInvocation,
    ) -> Result<Vec<CommandCompletion>> {
        let command = self
            .command_registry
            .get_by_name(&invocation.name)
            .ok_or_else(|| Error::CommandNotFound(invocation.name.clone()))?;
        command
            .complete(
                &CommandContext {
                    hook_context: self.hook_context.clone(),
                    workdir: self.workdir.clone(),
                    cancellation: self.cancellation.child_token(),
                },
                &invocation,
            )
            .await
    }

    pub async fn dispatch_command(&self, invocation: CommandInvocation) -> Result<CommandResult> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(SessionRequest::DispatchCommand { invocation, reply })
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::ChannelClosed)?
    }

    pub async fn close(&self) -> Result<()> {
        self.requests
            .send(SessionRequest::Close)
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
    commands: CommandRegistry,
    providers: ProviderRegistry,
    workdir: Arc<dyn Workdir>,
    core: Core,
    store: Option<Arc<dyn SessionStore>>,
    cancellation: CancellationToken,
    request_rx: mpsc::Receiver<SessionRequest>,
    snapshot_tx: watch::Sender<SessionSnapshot>,
    active: Option<ActiveTurn>,
    active_preparation: Option<ActivePreparation>,
    active_command: Option<ActiveCommand>,
    hook_services: super::HookServicesRegistration,
}

async fn run_actor(mut actor: Actor) {
    let (preparation_finished_tx, mut preparation_finished_rx) =
        mpsc::channel::<PreparationFinished>(1);
    let (finished_tx, mut finished_rx) = mpsc::channel::<TurnFinished>(1);
    let (command_finished_tx, mut command_finished_rx) = mpsc::channel::<CommandFinished>(1);
    let (update_tx, mut update_rx) = mpsc::channel::<TurnUpdate>(32);
    loop {
        tokio::select! {
            _ = actor.cancellation.cancelled() => break,
            Some(finished) = preparation_finished_rx.recv() => {
                actor.finish_preparation(finished, finished_tx.clone(), update_tx.clone()).await;
            }
            Some(finished) = finished_rx.recv() => actor.finish_turn(finished).await,
            Some(finished) = command_finished_rx.recv() => actor.finish_command(finished).await,
            Some(update) = update_rx.recv() => actor.apply_turn_update(update).await,
            request = actor.request_rx.recv() => match request {
                Some(SessionRequest::Send { message, reply }) => {
                    actor.start_preparation(message, reply, preparation_finished_tx.clone());
                }
                Some(SessionRequest::Cancel { reply }) => {
                    let cancelled = if let Some(active) = actor.active_preparation.as_ref() {
                        active.cancellation.cancel();
                        true
                    } else if let Some(active) = actor.active.as_ref() {
                        active.cancellation.cancel();
                        true
                    } else if let Some(active) = actor.active_command.as_ref() {
                        active.cancellation.cancel();
                        true
                    } else {
                        false
                    };
                    let _ = reply.send(cancelled);
                }
                Some(SessionRequest::DispatchCommand { invocation, reply }) => {
                    actor.start_command(invocation, reply, command_finished_tx.clone()).await;
                }
                Some(SessionRequest::HistorySnapshot { reply }) => {
                    let _ = reply.send(actor.history_snapshot());
                }
                Some(SessionRequest::ReplaceHistory { operation, expected_revision, reply }) => {
                    let _ = reply.send(actor.replace_history(expected_revision, operation).await);
                }
                Some(SessionRequest::Close) | None => break,
            }
        }
    }
    if let Some(active) = actor.active_preparation.take() {
        active.cancellation.cancel();
        active.task.abort();
        let _ = active.reply.send(Err(Error::SessionClosed));
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
    if let Some(active) = actor.active_command.take() {
        active.cancellation.cancel();
        active.task.abort();
        actor.update_snapshot(|snapshot| snapshot.active_command = None);
        actor
            .notify(RuntimeEvent::CommandCancelled {
                session_id: actor.id,
                command_id: active.id,
            })
            .await;
    }
    actor.cancellation.cancel();
    super::uninstall_hook_services(actor.id, &actor.hook_services);
}

impl Actor {
    fn start_preparation(
        &mut self,
        mut message: Message,
        reply: oneshot::Sender<Result<TurnId>>,
        finished_tx: mpsc::Sender<PreparationFinished>,
    ) {
        if self.active_preparation.is_some()
            || self.active.is_some()
            || self.active_command.is_some()
        {
            let _ = reply.send(Err(Error::SessionBusy));
            return;
        }
        if self.cancellation.is_cancelled() {
            let _ = reply.send(Err(Error::SessionClosed));
            return;
        }
        let cancellation = self.cancellation.child_token();
        super::set_hook_cancellation(self.id, cancellation.clone());
        let task_cancellation = cancellation.clone();
        let hooks = self.hooks.clone();
        let hook_context = HookContext {
            project_id: self.project_id,
            session_id: self.id,
        };
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                _ = task_cancellation.cancelled() => Err(Error::Cancelled),
                result = hooks.before_user_message(&hook_context, &mut message) => match result {
                    Ok(BeforeHookResult::Continue) => Ok(message),
                    Ok(BeforeHookResult::Cancel { reason }) => Err(Error::HookCancelled(reason)),
                    Err(error) => Err(error),
                },
            };
            let _ = finished_tx.send(PreparationFinished { result }).await;
        });
        self.active_preparation = Some(ActivePreparation {
            cancellation,
            task,
            reply,
        });
    }

    async fn finish_preparation(
        &mut self,
        finished: PreparationFinished,
        finished_tx: mpsc::Sender<TurnFinished>,
        update_tx: mpsc::Sender<TurnUpdate>,
    ) {
        let Some(active) = self.active_preparation.take() else {
            return;
        };
        super::set_hook_cancellation(self.id, self.cancellation.child_token());
        let result = match finished.result {
            Ok(message) => self.start_turn(message, finished_tx, update_tx).await,
            Err(error) => Err(error),
        };
        let _ = active.reply.send(result);
    }

    async fn start_turn(
        &mut self,
        message: Message,
        finished_tx: mpsc::Sender<TurnFinished>,
        update_tx: mpsc::Sender<TurnUpdate>,
    ) -> Result<TurnId> {
        if self.cancellation.is_cancelled() {
            return Err(Error::SessionClosed);
        }
        let cancellation = self.cancellation.child_token();
        super::set_hook_cancellation(self.id, cancellation.clone());
        self.persist_and_add(message.clone()).await?;
        let id = TurnId::new();
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

    async fn start_command(
        &mut self,
        invocation: CommandInvocation,
        reply: oneshot::Sender<Result<CommandResult>>,
        finished_tx: mpsc::Sender<CommandFinished>,
    ) {
        if self.active_preparation.is_some()
            || self.active.is_some()
            || self.active_command.is_some()
        {
            let _ = reply.send(Err(Error::SessionBusy));
            return;
        }
        if self.cancellation.is_cancelled() {
            let _ = reply.send(Err(Error::SessionClosed));
            return;
        }
        let Some(id) = self.commands.id_by_name(&invocation.name) else {
            let _ = reply.send(Err(Error::CommandNotFound(invocation.name)));
            return;
        };
        let command = self
            .commands
            .get(&id)
            .expect("command id came from registry");
        let cancellation = self.cancellation.child_token();
        super::set_hook_cancellation(self.id, cancellation.clone());
        let task_cancellation = cancellation.clone();
        let context = CommandContext {
            hook_context: HookContext {
                project_id: self.project_id,
                session_id: self.id,
            },
            workdir: self.workdir.clone(),
            cancellation: task_cancellation.clone(),
        };
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                _ = task_cancellation.cancelled() => Err(Error::Cancelled),
                result = command.execute(invocation, context) => result,
            };
            let finished = CommandFinished {
                id: task_id,
                result,
                reply,
            };
            if let Err(error) = finished_tx.send(finished).await {
                let finished = error.0;
                let _ = finished.reply.send(finished.result);
            }
        });
        self.active_command = Some(ActiveCommand {
            id: id.clone(),
            cancellation,
            task,
        });
        self.update_snapshot(|snapshot| {
            snapshot.active_command = Some(id.clone());
            snapshot.last_error = None;
        });
        self.notify(RuntimeEvent::CommandStarted {
            session_id: self.id,
            command_id: id,
        })
        .await;
    }

    async fn finish_command(&mut self, finished: CommandFinished) {
        if self.active_command.as_ref().map(|active| &active.id) != Some(&finished.id) {
            return;
        }
        self.active_command.take();
        super::set_hook_cancellation(self.id, self.cancellation.child_token());
        self.update_snapshot(|snapshot| snapshot.active_command = None);
        let event = match &finished.result {
            Ok(_) => RuntimeEvent::CommandCompleted {
                session_id: self.id,
                command_id: finished.id.clone(),
            },
            Err(Error::Cancelled) => RuntimeEvent::CommandCancelled {
                session_id: self.id,
                command_id: finished.id.clone(),
            },
            Err(error) => {
                let text = error.to_string();
                self.update_snapshot(|snapshot| snapshot.last_error = Some(text.clone()));
                RuntimeEvent::CommandFailed {
                    session_id: self.id,
                    command_id: finished.id.clone(),
                    error: text,
                }
            }
        };
        self.notify(event).await;
        let _ = finished.reply.send(finished.result);
    }

    async fn finish_turn(&mut self, finished: TurnFinished) {
        if self.active.as_ref().map(|active| active.id) != Some(finished.id) {
            return;
        }
        self.active.take();
        super::set_hook_cancellation(self.id, self.cancellation.child_token());
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

    async fn persist_and_add(&self, message: Message) -> Result<HistorySnapshot> {
        if let Some(store) = &self.store {
            store.append_message(self.id, &message).await?;
        }
        self.update_snapshot(|snapshot| {
            snapshot.messages.push(message.clone());
            snapshot.revision += 1;
        });
        self.notify(RuntimeEvent::MessageAdded {
            session_id: self.id,
            message,
        })
        .await;
        Ok(self.history_snapshot())
    }

    fn history_snapshot(&self) -> HistorySnapshot {
        let snapshot = self.snapshot_tx.borrow();
        HistorySnapshot {
            revision: snapshot.revision,
            messages: snapshot.messages.clone(),
        }
    }

    async fn replace_history(
        &self,
        expected_revision: u64,
        operation: HistoryOperation,
    ) -> Result<HistorySnapshot> {
        let current = self.history_snapshot();
        if current.revision != expected_revision {
            return Err(Error::HistoryRevisionMismatch {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        let mut messages = current.messages;
        match operation {
            HistoryOperation::ReplaceRange {
                first,
                last,
                replacement,
            } => {
                let start = messages
                    .iter()
                    .position(|message| message.id == first)
                    .ok_or(Error::MessageNotFound(first))?;
                let end = messages
                    .iter()
                    .position(|message| message.id == last)
                    .ok_or(Error::MessageNotFound(last))?;
                if start > end {
                    return Err(Error::InvalidMessageRange);
                }
                messages.splice(start..=end, replacement);
            }
            HistoryOperation::TruncateBefore(anchor) => {
                let index = messages
                    .iter()
                    .position(|message| message.id == anchor)
                    .ok_or(Error::MessageNotFound(anchor))?;
                messages.drain(..index);
            }
            HistoryOperation::TruncateAfter(anchor) => {
                let index = messages
                    .iter()
                    .position(|message| message.id == anchor)
                    .ok_or(Error::MessageNotFound(anchor))?;
                messages.truncate(index + 1);
            }
        }
        if let Some(store) = &self.store {
            store.replace_messages(self.id, &messages).await?;
        }
        let revision = current.revision + 1;
        self.update_snapshot(|snapshot| {
            snapshot.messages = messages.clone();
            snapshot.revision = revision;
        });
        self.notify(RuntimeEvent::HistoryReplaced {
            session_id: self.id,
            revision,
            messages: messages.clone(),
        })
        .await;
        Ok(HistorySnapshot { revision, messages })
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
            mode: super::ProviderMode::Normal,
            model: model.clone(),
            messages: messages.clone(),
            tools: definitions.clone(),
            context,
            cancellation: cancellation.clone(),
        };
        let history_before_hook = hook_context.history().snapshot().await?.revision;
        if let BeforeHookResult::Cancel { reason } = hooks
            .before_provider_request(&hook_context, &mut request)
            .await?
        {
            return Err(Error::HookCancelled(reason));
        }
        let canonical = hook_context.history().snapshot().await?;
        if canonical.revision != history_before_hook {
            messages = canonical.messages;
            request.messages = messages.clone();
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
        let assistant_message_id = assistant_message.id;
        messages = persist_generated(turn_id, assistant_message, &updates)
            .await?
            .messages;
        if calls.is_empty() {
            return Ok(());
        }

        for (call_id, name, arguments) in calls {
            let mut execution_context = ToolExecutionContext {
                hook_context: hook_context.clone(),
                turn_id,
                assistant_message_id,
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
            messages = persist_generated(turn_id, result, &updates).await?.messages;
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
) -> Result<HistorySnapshot> {
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
