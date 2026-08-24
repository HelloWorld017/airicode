use crate::core::error::Result;
use crate::core::models::{
    ContextPart, ContextPartId, ContextPriority, ContextSource, SessionMutation,
};

use super::Operations;

impl Operations {
    pub async fn add_context_part(
        &self,
        priority: ContextPriority,
        source: ContextSource,
        metadata: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<ContextPartId> {
        let part = ContextPart {
            id: ContextPartId::new(),
            priority,
            source,
            metadata: metadata.into_iter().collect(),
            invalidated: false,
        };
        let id = part.id;
        self.commit(vec![SessionMutation::ContextPartAdded { part }])
            .await?;
        Ok(id)
    }
}
