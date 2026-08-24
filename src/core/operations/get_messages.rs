use crate::core::error::Result;
use crate::core::models::Message;

use super::Operations;

impl Operations {
    pub async fn get_messages(&self) -> Result<Vec<Message>> {
        Ok(self
            .snapshot()
            .await?
            .visible_messages()
            .into_iter()
            .cloned()
            .collect())
    }
}
