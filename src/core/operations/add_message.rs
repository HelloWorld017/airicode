use crate::core::error::Result;
use crate::core::models::{
    ContextPart, ContextPartId, ContextPriority, ContextSource, Message, MessageId, Role,
    SessionCommit, SessionMutation,
};
use crate::utils::TimeSeq;

use super::Operations;

impl Operations {
    pub async fn add_message(&self, mut message: Message) -> Result<SessionCommit> {
        add_mode_metadata(&mut message);
        self.commit(vec![SessionMutation::MessageAdded { message }])
            .await
    }

    pub async fn add_conversation_message(
        &self,
        mut message: Message,
        priority: ContextPriority,
    ) -> Result<(MessageId, ContextPartId)> {
        add_mode_metadata(&mut message);
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

fn add_mode_metadata(message: &mut Message) {
    if message.role == Role::User {
        let mode = message.mode.clone();
        message
            .metadata
            .entry("mode".into())
            .or_insert(serde_json::Value::String(mode));
    }
}
