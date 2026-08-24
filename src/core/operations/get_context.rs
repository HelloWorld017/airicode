use crate::core::error::Result;
use crate::core::models::ContextPart;

use super::Operations;

impl Operations {
    pub async fn get_context(&self) -> Result<Vec<ContextPart>> {
        Ok(self.snapshot().await?.active_context())
    }
}
