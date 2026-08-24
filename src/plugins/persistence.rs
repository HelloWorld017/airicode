use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;

use crate::core::{
    error::{Error, Result},
    models::{Plugin, PluginId, SessionCommit, SessionId},
    registry::PluginRegistryScope,
};

pub use crate::core::persistence::SessionStore;

pub const SESSION_LOG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionLogRecord {
    pub schema_version: u32,
    pub sequence: u64,
    pub commit_id: crate::core::models::CommitId,
    pub created_at: crate::core::models::TimeSeq,
    pub mutations: Vec<crate::core::models::SessionMutation>,
}

impl From<&SessionCommit> for SessionLogRecord {
    fn from(commit: &SessionCommit) -> Self {
        Self {
            schema_version: SESSION_LOG_SCHEMA_VERSION,
            sequence: commit.sequence,
            commit_id: commit.commit_id,
            created_at: commit.created_at,
            mutations: commit.mutations.clone(),
        }
    }
}

impl TryFrom<SessionLogRecord> for SessionCommit {
    type Error = Error;

    fn try_from(record: SessionLogRecord) -> Result<Self> {
        if record.schema_version != SESSION_LOG_SCHEMA_VERSION {
            return Err(Error::Persistence(format!(
                "unsupported session log schema version {}",
                record.schema_version
            )));
        }
        Ok(Self {
            sequence: record.sequence,
            commit_id: record.commit_id,
            created_at: record.created_at,
            mutations: record.mutations,
        })
    }
}

/// Append-only JSONL storage for one project. A single store can be shared by
/// all sessions in that project; the session id is part of the log filename.
#[derive(Clone)]
pub struct JsonlSessionStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl JsonlSessionStore {
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self> {
        let project_root = fs::canonicalize(project_root.as_ref()).map_err(|error| {
            Error::Persistence(format!("cannot canonicalize project root: {error}"))
        })?;
        let data_root = default_data_root()?.join("airicode");
        let project_name = project_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project");
        let project_dir =
            data_root.join(format!("{}-{}", project_name, project_hash(&project_root)));
        Ok(Self {
            root: Arc::new(project_dir.join("sessions")),
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn new_at(path: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(path.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn root(&self) -> PathBuf {
        (*self.root).clone()
    }

    pub fn path_for(&self, session_id: SessionId) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }

    pub async fn discover(&self) -> Result<Vec<SessionId>> {
        let mut entries = match tokio::fs::read_dir(self.root.as_path()).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut sessions = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let uuid = Uuid::parse_str(stem).map_err(|error| {
                Error::Persistence(format!("invalid session filename {stem}: {error}"))
            })?;
            sessions.push(SessionId::from_uuid(uuid));
        }
        sessions.sort_unstable();
        Ok(sessions)
    }

    async fn load_unlocked(&self, session_id: SessionId) -> Result<Vec<SessionCommit>> {
        let bytes = match tokio::fs::read(self.path_for(session_id)).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let has_final_newline = bytes.last() == Some(&b'\n');
        let mut lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        if has_final_newline {
            lines.pop();
        }
        let mut commits = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            if line.is_empty() {
                return Err(Error::Persistence(format!(
                    "empty record at line {}",
                    index + 1
                )));
            }
            let record = match serde_json::from_slice::<SessionLogRecord>(line) {
                Ok(record) => record,
                Err(error) if !has_final_newline && index + 1 == lines.len() => {
                    // A process can die between write and newline. The last
                    // incomplete record is intentionally discarded.
                    let _ = error;
                    break;
                }
                Err(error) => {
                    return Err(Error::Persistence(format!(
                        "corrupt session log line {}: {error}",
                        index + 1
                    )))
                }
            };
            commits.push(SessionCommit::try_from(record)?);
        }
        for (expected, commit) in (1..).zip(&commits) {
            if commit.sequence != expected {
                return Err(Error::Persistence(format!(
                    "session log sequence mismatch: expected {expected}, got {}",
                    commit.sequence
                )));
            }
        }
        Ok(commits)
    }

    async fn recover_partial_tail(&self, session_id: SessionId) -> Result<()> {
        let path = self.path_for(session_id);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if bytes.last() == Some(&b'\n') {
            return Ok(());
        }
        let Some(start) = bytes.iter().rposition(|byte| *byte == b'\n') else {
            let file = OpenOptions::new().write(true).open(&path).await?;
            file.set_len(0).await?;
            return Ok(());
        };
        let tail = &bytes[start + 1..];
        if serde_json::from_slice::<SessionLogRecord>(tail).is_ok() {
            let mut file = OpenOptions::new().append(true).open(&path).await?;
            file.write_all(b"\n").await?;
            file.flush().await?;
            return Ok(());
        }
        let file = OpenOptions::new().write(true).open(&path).await?;
        file.set_len((start + 1) as u64).await?;
        Ok(())
    }
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn load(&self, session_id: SessionId) -> Result<Vec<SessionCommit>> {
        let _guard = self.lock.lock().await;
        self.load_unlocked(session_id).await
    }

    async fn append(&self, session_id: SessionId, commit: &SessionCommit) -> Result<()> {
        let _guard = self.lock.lock().await;
        let existing = self.load_unlocked(session_id).await?;
        let expected = existing.len() as u64 + 1;
        if commit.sequence != expected {
            return Err(Error::Persistence(format!(
                "cannot append sequence {}, expected {}",
                commit.sequence, expected
            )));
        }
        tokio::fs::create_dir_all(self.root.as_path()).await?;
        self.recover_partial_tail(session_id).await?;
        let record = SessionLogRecord::from(commit);
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path_for(session_id))
            .await?;
        file.write_all(&line).await?;
        file.flush().await?;
        file.sync_data().await?;
        Ok(())
    }
}

pub fn project_hash(project_root: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(project_root.as_os_str().to_string_lossy().as_bytes());
    digest
        .finalize()
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn default_data_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home =
        std::env::var_os("HOME").ok_or_else(|| Error::Persistence("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".local/share"))
}

pub type JsonlStore = JsonlSessionStore;

pub struct PersistencePlugin {
    id: PluginId,
    store: Arc<dyn SessionStore>,
}

impl PersistencePlugin {
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            id: PluginId::new(),
            store,
        }
    }

    pub fn for_project(project_root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::new(Arc::new(JsonlSessionStore::new(project_root)?)))
    }

    pub fn store(&self) -> Arc<dyn SessionStore> {
        self.store.clone()
    }
}

#[async_trait]
impl Plugin for PersistencePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "persistence"
    }

    fn config_schema(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry
            .register_session_store(self.store.clone(), 0)
            .map(|_| ())
    }
}
