use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::context::ContextPart;
use super::id::{CommitId, ContextPartId, MessageId, NoteId, SessionGroupId, SessionId};
use super::message::Message;
use super::note::{Note, NoteContent};
use super::ui_state::UIState;
use crate::utils::TimeSeq;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: SessionId,
    pub group_id: SessionGroupId,
    pub messages: BTreeMap<MessageId, Message>,
    pub invalidated_messages: BTreeSet<MessageId>,
    pub context: BTreeMap<ContextPartId, ContextPart>,
    pub notes: BTreeMap<NoteId, Note>,
    pub ui: UIState,
    pub last_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionMutation {
    SessionCreated {
        session_id: SessionId,
        group_id: SessionGroupId,
    },
    MessageAdded {
        message: Message,
    },
    MessageInvalidated {
        message_id: MessageId,
    },
    ContextPartAdded {
        part: ContextPart,
    },
    ContextPartInvalidated {
        context_part_id: ContextPartId,
    },
    NoteAdded {
        note: Note,
    },
    NoteUpdated {
        note_id: NoteId,
        content: NoteContent,
        metadata: super::message::Metadata,
    },
    DurableUIStateUpdated {
        state: super::ui_state::DurableUIState,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionCommit {
    pub sequence: u64,
    pub commit_id: CommitId,
    pub created_at: TimeSeq,
    pub mutations: Vec<SessionMutation>,
}

impl SessionCommit {
    pub fn new(sequence: u64, mutations: Vec<SessionMutation>) -> Self {
        Self {
            sequence,
            commit_id: CommitId::new(),
            created_at: TimeSeq::new(),
            mutations,
        }
    }
}
