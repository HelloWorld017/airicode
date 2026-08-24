use crate::core::error::Result;
use crate::core::models::{MessageId, SessionCommit, SessionMutation};

use super::Operations;

impl Operations {
    pub async fn invalidate_message(&self, id: MessageId) -> Result<SessionCommit> {
        self.commit(vec![SessionMutation::MessageInvalidated { message_id: id }])
            .await
    }
}
