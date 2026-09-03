use super::Operations;
use crate::core::{
    error::Result,
    models::{ProjectId, SessionGroupId, SessionId},
};

impl Operations {
    pub fn project_id(&self) -> Result<ProjectId> {
        Ok(self.project()?.id)
    }

    pub fn session_id(&self) -> Result<SessionId> {
        Ok(self.host()?.session_id())
    }

    pub fn group_id(&self) -> Result<SessionGroupId> {
        Ok(self.host()?.group_id())
    }
}
