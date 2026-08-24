use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::id::ProjectId;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub root: PathBuf,
}
