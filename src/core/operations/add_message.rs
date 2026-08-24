use crate::core::error::Result;
use crate::core::models::{
    ContextPart, ContextPartId, ContextPriority, ContextSource, Message, MessageId, SessionMutation,
};
use crate::utils::TimeSeq;

use super::Operations;

impl Operations {
    pub async fn add_message(
        &self,
        message: Message,
    ) -> Result<crate::core::models::SessionCommit> {
        self.commit(vec![SessionMutation::MessageAdded { message }])
            .await
    }

    pub async fn add_conversation_message(
        &self,
        message: Message,
        priority: ContextPriority,
    ) -> Result<(MessageId, ContextPartId)> {
        let message_id = message.id;
        let part = ContextPart {
            id: ContextPartId::new(),
            priority,
            source: ContextSource::Message(message_id),
            created_at: TimeSeq::new(),
            metadata: Default::default(),
            invalidated: false,
        };
        let part_id = part.id;
        self.commit(vec![
            SessionMutation::MessageAdded { message },
            SessionMutation::ContextPartAdded { part },
        ])
        .await?;
        Ok((message_id, part_id))
    }
}
