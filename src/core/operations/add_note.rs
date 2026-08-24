use crate::core::error::Result;
use crate::core::models::{Note, NoteContent, NoteId, SessionMutation, TimeSeq};

use super::Operations;

impl Operations {
    pub async fn add_note(
        &self,
        content: NoteContent,
        metadata: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<NoteId> {
        let note = Note {
            id: NoteId::new(),
            content,
            created_at: TimeSeq::new(),
            metadata: metadata.into_iter().collect(),
        };
        let id = note.id;
        self.commit(vec![SessionMutation::NoteAdded { note }])
            .await?;
        Ok(id)
    }
}
