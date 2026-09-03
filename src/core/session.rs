use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};

use super::{
    error::{Error, Result},
    models::{
        ContextSource, Project, ProjectId, RuntimeEvent, SessionCommit, SessionGroupId, SessionId,
        SessionMutation, SessionState, WorkdirLayerContext,
    },
    operations::{Operations, SessionRequest, run_actor},
    persistence::SessionStore,
    registry::Registry,
    workdir::{NativeWorkdir, Workdir},
};

pub struct SessionRuntimeDeps {
    pub registry: Registry,
    pub project: Project,
    pub store: Option<Arc<dyn SessionStore>>,
}

impl SessionRuntimeDeps {
    pub fn new(registry: Registry, project: Project) -> Self {
        Self {
            registry,
            project,
            store: None,
        }
    }
}

pub struct SessionRuntime {
    project_id: ProjectId,
    session_id: SessionId,
    group_id: SessionGroupId,
    registry: Registry,
    workdir: Arc<dyn Workdir>,
    store: Option<Arc<dyn SessionStore>>,
    sender: mpsc::Sender<SessionRequest>,
    snapshot: watch::Receiver<SessionState>,
    events: broadcast::Sender<RuntimeEvent>,
}

impl SessionRuntime {
    pub fn spawn(state: SessionState, deps: SessionRuntimeDeps) -> Result<Arc<Self>> {
        let base: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(&deps.project.root)?);
        let workdir = deps.registry.layer_workdir(
            &WorkdirLayerContext {
                project_id: deps.project.id,
                project_name: deps.project.name.clone(),
                session_group_id: state.group_id,
            },
            base,
        );
        let (sender, receiver) = mpsc::channel(64);
        let (snapshot_tx, snapshot) = watch::channel(state.clone());
        let (events, _) = broadcast::channel(256);
        let runtime = Arc::new(Self {
            project_id: deps.project.id,
            session_id: state.session_id,
            group_id: state.group_id,
            registry: deps.registry,
            workdir,
            store: deps.store,
            sender,
            snapshot,
            events,
        });
        tokio::spawn(run_actor(
            state,
            receiver,
            snapshot_tx,
            runtime.events(),
            runtime.store(),
        ));
        Ok(runtime)
    }

    pub fn operations(self: &Arc<Self>) -> Operations {
        Operations::from_runtime(Arc::downgrade(self))
    }

    pub fn turn_engine(self: &Arc<Self>) -> super::runtime::TurnEngine {
        super::runtime::TurnEngine::from_runtime(self.clone())
    }

    pub(crate) fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn group_id(&self) -> SessionGroupId {
        self.group_id
    }

    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    pub(crate) fn workdir(&self) -> Arc<dyn Workdir> {
        self.workdir.clone()
    }

    pub(crate) fn store(&self) -> Option<Arc<dyn SessionStore>> {
        self.store.clone()
    }

    pub(crate) fn sender(&self) -> mpsc::Sender<SessionRequest> {
        self.sender.clone()
    }

    pub(crate) fn snapshot(&self) -> &watch::Receiver<SessionState> {
        &self.snapshot
    }

    pub(crate) fn events(&self) -> broadcast::Sender<RuntimeEvent> {
        self.events.clone()
    }
}

impl SessionState {
    pub fn new(
        session_id: super::models::SessionId,
        group_id: super::models::SessionGroupId,
    ) -> Self {
        Self {
            session_id,
            group_id,
            messages: Default::default(),
            invalidated_messages: Default::default(),
            context: Default::default(),
            notes: Default::default(),
            ui: Default::default(),
            last_sequence: 0,
        }
    }

    pub fn active_context(&self) -> Vec<super::models::ContextPart> {
        let mut context = self
            .context
            .values()
            .filter(|part| !part.invalidated)
            .cloned()
            .collect::<Vec<_>>();
        context.sort_by_key(|part| part.created_at);
        context
    }

    pub fn replay(
        session_id: super::models::SessionId,
        group_id: super::models::SessionGroupId,
        commits: Vec<SessionCommit>,
    ) -> Result<Self> {
        let mut state = Self::new(session_id, group_id);
        for commit in commits {
            state.apply(&commit)?;
        }
        Ok(state)
    }

    pub fn visible_messages(&self) -> Vec<&super::models::Message> {
        let mut messages = self
            .messages
            .iter()
            .filter(|(id, _)| !self.invalidated_messages.contains(id))
            .map(|(_, message)| message)
            .collect::<Vec<_>>();
        messages.sort_by_key(|message| message.created_at);
        messages
    }

    pub fn apply(&mut self, commit: &SessionCommit) -> Result<()> {
        if commit.sequence != self.last_sequence + 1 {
            return Err(Error::InvalidState(format!(
                "expected commit sequence {}, got {}",
                self.last_sequence + 1,
                commit.sequence
            )));
        }
        let mut next = self.clone();
        for mutation in &commit.mutations {
            next.apply_mutation(mutation)?;
        }
        next.last_sequence = commit.sequence;
        *self = next;
        Ok(())
    }

    fn apply_mutation(&mut self, mutation: &SessionMutation) -> Result<()> {
        match mutation {
            SessionMutation::SessionCreated {
                session_id,
                group_id,
            } => {
                if *session_id != self.session_id || *group_id != self.group_id {
                    return Err(Error::InvalidState("session identity mismatch".into()));
                }
            }
            SessionMutation::MessageAdded { message } => {
                if message.content.iter().any(|part| !part.is_valid()) {
                    return Err(Error::InvalidState("message contains an empty part".into()));
                }
                if self.messages.contains_key(&message.id) {
                    return Err(Error::InvalidState(format!(
                        "duplicate message {}",
                        message.id
                    )));
                }
                self.messages.insert(message.id, message.clone());
            }
            SessionMutation::MessageInvalidated { message_id } => {
                if !self.messages.contains_key(message_id) {
                    return Err(Error::InvalidState(format!(
                        "unknown message {}",
                        message_id
                    )));
                }
                self.invalidated_messages.insert(*message_id);
            }
            SessionMutation::ContextPartAdded { part } => {
                if self.context.contains_key(&part.id) {
                    return Err(Error::InvalidState(format!(
                        "duplicate context part {}",
                        part.id
                    )));
                }
                if let ContextSource::Message(id) = part.source {
                    if !self.messages.contains_key(&id) {
                        return Err(Error::InvalidState(
                            "context references unknown message".into(),
                        ));
                    }
                }
                self.context.insert(part.id, part.clone());
            }
            SessionMutation::ContextPartInvalidated { context_part_id } => {
                let part = self.context.get_mut(context_part_id).ok_or_else(|| {
                    Error::InvalidState(format!("unknown context part {}", context_part_id))
                })?;
                part.invalidated = true;
            }
            SessionMutation::NoteAdded { note } => {
                if self.notes.contains_key(&note.id) {
                    return Err(Error::InvalidState(format!("duplicate note {}", note.id)));
                }
                self.notes.insert(note.id, note.clone());
            }
            SessionMutation::NoteUpdated {
                note_id,
                content,
                metadata,
            } => {
                let note = self
                    .notes
                    .get_mut(note_id)
                    .ok_or_else(|| Error::InvalidState(format!("unknown note {}", note_id)))?;
                note.content = content.clone();
                note.metadata = metadata.clone();
            }
            SessionMutation::DurableUIStateUpdated { state } => self.ui.durable = state.clone(),
        }
        Ok(())
    }
}
