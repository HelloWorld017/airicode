use serde::{Deserialize, Serialize};

use super::id::{ProjectId, SessionGroupId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionGroup {
    pub id: SessionGroupId,
    pub project_id: ProjectId,
}
