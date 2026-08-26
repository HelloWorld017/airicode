use serde::{Deserialize, Serialize};

use super::id::NoteId;
use super::message::Metadata;
use crate::utils::TimeSeq;

pub type NoteMetadata = Metadata;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NoteContent {
    Info { content: String },
    Alert { content: String },
    Subtle { content: String },
    Diff { file: String, content: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    pub content: NoteContent,
    pub created_at: TimeSeq,
    pub metadata: Metadata,
}
