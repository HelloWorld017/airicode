use super::error::{Error, Result};
use super::models::{
    ContextSource, SessionCommit, SessionMutation, SessionState, TimelineEntry, TimelineItem,
};

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
            timeline: Vec::new(),
            last_sequence: 0,
        }
    }

    pub fn active_context(&self) -> Vec<super::models::ContextPart> {
        self.context
            .values()
            .filter(|part| !part.invalidated)
            .cloned()
            .collect()
    }

    pub fn visible_messages(&self) -> Vec<&super::models::Message> {
        self.timeline
            .iter()
            .filter_map(|entry| match entry.item {
                TimelineItem::Message(id) if !self.invalidated_messages.contains(&id) => {
                    self.messages.get(&id)
                }
                _ => None,
            })
            .collect()
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
            next.apply_mutation(commit.sequence, mutation)?;
        }
        next.last_sequence = commit.sequence;
        *self = next;
        Ok(())
    }

    fn apply_mutation(&mut self, sequence: u64, mutation: &SessionMutation) -> Result<()> {
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
                if self.messages.contains_key(&message.id) {
                    return Err(Error::InvalidState(format!(
                        "duplicate message {}",
                        message.id
                    )));
                }
                self.messages.insert(message.id, message.clone());
                self.timeline.push(TimelineEntry {
                    sequence,
                    item: TimelineItem::Message(message.id),
                });
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
                self.timeline.push(TimelineEntry {
                    sequence,
                    item: TimelineItem::Note(note.id),
                });
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
