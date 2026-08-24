use crate::core::error::Result;
use crate::core::models::{ContextPartId, SessionCommit, SessionMutation};

use super::Operations;

impl Operations {
    pub async fn invalidate_context_part(&self, id: ContextPartId) -> Result<SessionCommit> {
        self.commit(vec![SessionMutation::ContextPartInvalidated {
            context_part_id: id,
        }])
        .await
    }
}
