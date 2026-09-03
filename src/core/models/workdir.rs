use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::super::error::Result;
use super::id::{ProjectId, SessionGroupId};

#[derive(Clone, Debug)]
pub struct WorkdirLayerContext {
    pub project_id: ProjectId,
    pub project_name: String,
    pub session_group_id: SessionGroupId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkdirEntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkdirEntry {
    pub path: PathBuf,
    pub kind: WorkdirEntryKind,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum WorkdirLayerPhase {
    Provision,
    Isolation,
    Observe,
}

#[async_trait]
pub trait Workdir: Send + Sync {
    fn root(&self) -> PathBuf;
    async fn exists(&self, path: &Path) -> Result<bool>;
    async fn list(&self, path: &Path) -> Result<Vec<WorkdirEntry>>;
    async fn read(&self, path: &Path) -> Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> Result<()>;
    async fn rename(&self, from: &Path, to: &Path) -> Result<()>;
    async fn remove(&self, path: &Path) -> Result<()>;
    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandResult>;
}

pub trait WorkdirLayer: Send + Sync {
    fn id(&self) -> super::id::WorkdirLayerId;
    fn phase(&self) -> WorkdirLayerPhase {
        WorkdirLayerPhase::Observe
    }
    fn layer(&self, context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir>;
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub max_output_bytes: usize,
}

impl CommandSpec {
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            env: BTreeMap::new(),
            max_output_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}
