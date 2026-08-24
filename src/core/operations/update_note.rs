use crate::core::error::Result;
use crate::core::models::{NoteContent, NoteId, SessionCommit, SessionMutation};

use super::Operations;

impl Operations {
    pub async fn update_note(
        &self,
        id: NoteId,
        content: NoteContent,
        metadata: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<SessionCommit> {
        self.commit(vec![SessionMutation::NoteUpdated {
            note_id: id,
            content,
            metadata: metadata.into_iter().collect(),
        }])
        .await
    }
}
