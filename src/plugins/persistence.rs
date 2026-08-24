use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::{
    Error, Message, Plugin, PluginId, PluginRegistrar, Result, SessionId, SessionStore,
    SessionStoreContext, SessionStoreFactory, SessionStoreFactoryId,
};

const PLUGIN_ID: &str = "builtin.persistence.jsonl";
const FACTORY_ID: &str = "builtin.persistence.jsonl";
const SCHEMA_VERSION: u32 = 1;
const FACTORY_PRIORITY: i32 = 0;

const MESSAGE_KIND: &str = "message";

#[derive(Clone, Debug)]
pub struct JsonlPersistenceConfig {
    pub data_dir: PathBuf,
    pub fsync: bool,
}

impl JsonlPersistenceConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            fsync: false,
        }
    }

    pub fn with_fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }
}

pub fn jsonl_persistence_plugin(config: JsonlPersistenceConfig) -> Arc<dyn Plugin> {
    Arc::new(JsonlPersistencePlugin { config })
}

struct JsonlPersistencePlugin {
    config: JsonlPersistenceConfig,
}

#[async_trait]
impl Plugin for JsonlPersistencePlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_session_store_factory(
            FACTORY_PRIORITY,
            Arc::new(JsonlSessionStoreFactory {
                config: self.config.clone(),
            }),
        )
    }
}

struct JsonlSessionStoreFactory {
    config: JsonlPersistenceConfig,
}

#[async_trait]
impl SessionStoreFactory for JsonlSessionStoreFactory {
    fn id(&self) -> SessionStoreFactoryId {
        SessionStoreFactoryId::new(FACTORY_ID)
    }

    async fn open(&self, context: &SessionStoreContext) -> Result<Option<Arc<dyn SessionStore>>> {
        Ok(Some(Arc::new(
            JsonlSessionStore::new(&self.config.data_dir, &context.project_name)
                .with_fsync(self.config.fsync),
        )))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct JsonlRecord {
    schema_version: u32,
    sequence: u64,
    event_id: Uuid,
    recorded_at: u64,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Debug)]
struct ScanResult {
    messages: Vec<Message>,
    next_sequence: u64,
    truncate_to: Option<u64>,
    needs_separator: bool,
}

/// Stores each session as an append-only, versioned JSONL event stream.
#[derive(Debug)]
struct JsonlSessionStore {
    base_dir: PathBuf,
    project_identity: String,
    fsync: bool,
    append_lock: Mutex<()>,
}

impl JsonlSessionStore {
    fn new(base_dir: impl Into<PathBuf>, project_identity: impl fmt::Display) -> Self {
        Self {
            base_dir: base_dir.into(),
            project_identity: project_identity.to_string(),
            fsync: false,
            append_lock: Mutex::new(()),
        }
    }

    fn with_fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }

    fn path_for_session(&self, session_id: SessionId) -> PathBuf {
        session_path(&self.base_dir, &self.project_identity, session_id)
    }

    fn load(&self, session_id: SessionId) -> Result<Vec<Message>> {
        let path = self.path_for_session(session_id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(store_error(&path, "read", error)),
        };

        Ok(scan_records(&path, &bytes)?.messages)
    }

    fn append(&self, session_id: SessionId, message: &Message) -> Result<()> {
        let _guard = self
            .append_lock
            .lock()
            .map_err(|_| Error::Store("JSONL append lock is poisoned".into()))?;
        let path = self.path_for_session(session_id);
        let parent = path
            .parent()
            .ok_or_else(|| Error::Store(format!("invalid session path {}", path.display())))?;
        fs::create_dir_all(parent).map_err(|error| store_error(parent, "create", error))?;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| store_error(&path, "open", error))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| store_error(&path, "read", error))?;
        let scan = scan_records(&path, &bytes)?;

        if let Some(length) = scan.truncate_to {
            file.set_len(length)
                .map_err(|error| store_error(&path, "repair", error))?;
        }
        file.seek(SeekFrom::End(0))
            .map_err(|error| store_error(&path, "seek", error))?;
        if scan.needs_separator && scan.truncate_to.is_none() {
            file.write_all(b"\n")
                .map_err(|error| store_error(&path, "append", error))?;
        }

        let record = JsonlRecord {
            schema_version: SCHEMA_VERSION,
            sequence: scan.next_sequence,
            event_id: Uuid::new_v4(),
            recorded_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            kind: MESSAGE_KIND.into(),
            payload: serde_json::to_value(message)
                .map_err(|error| Error::Store(format!("serialize message: {error}")))?,
        };
        let encoded = serde_json::to_vec(&record)
            .map_err(|error| Error::Store(format!("serialize JSONL record: {error}")))?;
        file.write_all(&encoded)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|error| store_error(&path, "append", error))?;
        file.flush()
            .map_err(|error| store_error(&path, "flush", error))?;
        if self.fsync {
            file.sync_data()
                .map_err(|error| store_error(&path, "fsync", error))?;
        }
        Ok(())
    }

    fn replace(&self, session_id: SessionId, messages: &[Message]) -> Result<()> {
        let _guard = self
            .append_lock
            .lock()
            .map_err(|_| Error::Store("JSONL append lock is poisoned".into()))?;
        let path = self.path_for_session(session_id);
        let parent = path
            .parent()
            .ok_or_else(|| Error::Store(format!("invalid session path {}", path.display())))?;
        fs::create_dir_all(parent).map_err(|error| store_error(parent, "create", error))?;
        let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| store_error(&temporary, "create", error))?;
            for (index, message) in messages.iter().enumerate() {
                let record = JsonlRecord {
                    schema_version: SCHEMA_VERSION,
                    sequence: index as u64 + 1,
                    event_id: Uuid::new_v4(),
                    recorded_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    kind: MESSAGE_KIND.into(),
                    payload: serde_json::to_value(message).map_err(|error| {
                        Error::Store(format!("serialize replacement message: {error}"))
                    })?,
                };
                let encoded = serde_json::to_vec(&record).map_err(|error| {
                    Error::Store(format!("serialize replacement JSONL record: {error}"))
                })?;
                file.write_all(&encoded)
                    .and_then(|_| file.write_all(b"\n"))
                    .map_err(|error| store_error(&temporary, "write", error))?;
            }
            file.flush()
                .map_err(|error| store_error(&temporary, "flush", error))?;
            if self.fsync {
                file.sync_data()
                    .map_err(|error| store_error(&temporary, "fsync", error))?;
            }
            fs::rename(&temporary, &path).map_err(|error| store_error(&path, "replace", error))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn load_messages(&self, session_id: SessionId) -> Result<Vec<Message>> {
        self.load(session_id)
    }

    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()> {
        self.append(session_id, message)
    }

    async fn replace_messages(&self, session_id: SessionId, messages: &[Message]) -> Result<()> {
        self.replace(session_id, messages)
    }
}

/// Returns a filesystem-safe, non-empty project directory name.
fn sanitize_project_identity(identity: &str) -> String {
    let mut sanitized = String::with_capacity(identity.len());
    let mut previous_was_separator = false;
    for character in identity.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            sanitized.push(character);
            previous_was_separator = false;
        } else if !previous_was_separator {
            sanitized.push('_');
            previous_was_separator = true;
        }
    }

    let sanitized = sanitized.trim_matches(['.', '_', '-']);
    if sanitized.is_empty() {
        "project".into()
    } else {
        sanitized.to_owned()
    }
}

fn session_path(
    base_dir: impl AsRef<Path>,
    project_identity: impl fmt::Display,
    session_id: SessionId,
) -> PathBuf {
    base_dir
        .as_ref()
        .join("projects")
        .join(sanitize_project_identity(&project_identity.to_string()))
        .join("sessions")
        .join(format!("{session_id}.jsonl"))
}

fn scan_records(path: &Path, bytes: &[u8]) -> Result<ScanResult> {
    let mut messages = Vec::new();
    let mut expected_sequence = 1_u64;
    let mut start = 0;
    let mut truncate_to = None;

    while start < bytes.len() {
        let relative_end = bytes[start..].iter().position(|byte| *byte == b'\n');
        let end = relative_end.map_or(bytes.len(), |offset| start + offset);
        let next = if relative_end.is_some() { end + 1 } else { end };
        let is_final = next >= bytes.len();
        let line = &bytes[start..end];

        let record: JsonlRecord = match serde_json::from_slice(line) {
            Ok(record) => record,
            Err(_) if is_final => {
                truncate_to = Some(start as u64);
                break;
            }
            Err(error) => {
                return Err(Error::Store(format!(
                    "corrupt JSONL record in {} at byte {start}: {error}",
                    path.display()
                )))
            }
        };
        if record.schema_version != SCHEMA_VERSION {
            return Err(Error::Store(format!(
                "unsupported schema version {} in {} at sequence {}",
                record.schema_version,
                path.display(),
                record.sequence
            )));
        }
        if record.sequence != expected_sequence {
            return Err(Error::Store(format!(
                "unexpected sequence {} in {} (expected {})",
                record.sequence,
                path.display(),
                expected_sequence
            )));
        }
        if record.kind != MESSAGE_KIND {
            return Err(Error::Store(format!(
                "unsupported record kind {:?} in {} at sequence {}",
                record.kind,
                path.display(),
                record.sequence
            )));
        }
        let message = serde_json::from_value(record.payload).map_err(|error| {
            Error::Store(format!(
                "invalid message payload in {} at sequence {}: {error}",
                path.display(),
                record.sequence
            ))
        })?;
        messages.push(message);
        expected_sequence += 1;
        start = next;
    }

    Ok(ScanResult {
        messages,
        next_sequence: expected_sequence,
        truncate_to,
        needs_separator: !bytes.is_empty() && !bytes.ends_with(b"\n"),
    })
}

fn store_error(path: &Path, operation: &str, error: std::io::Error) -> Error {
    Error::Store(format!(
        "could not {operation} session store {}: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, sync::Arc};

    use tempfile::tempdir;

    use super::*;
    use crate::{
        core::{NativeWorkdir, OpenSession, ProviderId, Role},
        Core,
    };

    fn factory(data_dir: impl Into<PathBuf>, fsync: bool) -> JsonlSessionStoreFactory {
        JsonlSessionStoreFactory {
            config: JsonlPersistenceConfig::new(data_dir).with_fsync(fsync),
        }
    }

    async fn open_store(
        factory: &JsonlSessionStoreFactory,
        project_name: &str,
        workdir: Arc<NativeWorkdir>,
    ) -> Arc<dyn SessionStore> {
        factory
            .open(&SessionStoreContext {
                project_id: Default::default(),
                project_name: project_name.into(),
                workdir,
                session: OpenSession {
                    id: Some(SessionId::new()),
                    provider: ProviderId::new("test"),
                    model: "test".into(),
                },
            })
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn plugin_registers_exactly_one_session_store_factory() {
        let plugin = jsonl_persistence_plugin(JsonlPersistenceConfig::new("state"));
        let registrar = PluginRegistrar::new(plugin.id());

        plugin.init(registrar.clone()).await.unwrap();

        let registrations = registrar.take();
        assert!(registrations.providers.is_empty());
        assert!(registrations.tools.is_empty());
        assert!(registrations.hooks.is_empty());
        assert_eq!(registrations.store_factories.len(), 1);
        assert_eq!(
            registrations.store_factories[0].factory.id(),
            SessionStoreFactoryId::new(FACTORY_ID)
        );
    }

    #[tokio::test]
    async fn core_without_persistence_plugin_opens_no_store() {
        let directory = tempdir().unwrap();
        let workdir = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let core = Core::new().build().await.unwrap();
        let context = SessionStoreContext {
            project_id: Default::default(),
            project_name: "project".into(),
            workdir,
            session: OpenSession {
                id: Some(SessionId::new()),
                provider: ProviderId::new("test"),
                model: "test".into(),
            },
        };

        assert!(core
            .hooks()
            .open_session_store(&context)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn appends_one_line_records_and_loads_messages() {
        let directory = tempdir().unwrap();
        let workdir = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let store = open_store(&factory(directory.path(), true), "org/example", workdir).await;
        let session_id = SessionId::new();
        let first = Message::text(Role::User, "hello\nworld");
        let second = Message::text(Role::Assistant, "response");

        store.append_message(session_id, &first).await.unwrap();
        store.append_message(session_id, &second).await.unwrap();

        assert_eq!(
            store.load_messages(session_id).await.unwrap(),
            vec![first, second]
        );
        let contents =
            fs::read_to_string(session_path(directory.path(), "org/example", session_id)).unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for (index, line) in lines.iter().enumerate() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["schema_version"], SCHEMA_VERSION);
            assert_eq!(record["sequence"], (index + 1) as u64);
            assert_eq!(record["kind"], MESSAGE_KIND);
            assert!(record["event_id"].is_string());
            assert!(record["recorded_at"].is_number());
            assert!(record["payload"].is_object());
        }
    }

    #[tokio::test]
    async fn ignores_and_repairs_a_malformed_final_line() {
        let directory = tempdir().unwrap();
        let workdir = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let store = open_store(&factory(directory.path(), false), "project", workdir).await;
        let session_id = SessionId::new();
        let first = Message::text(Role::User, "first");
        let second = Message::text(Role::Assistant, "second");
        store.append_message(session_id, &first).await.unwrap();

        let path = session_path(directory.path(), "project", session_id);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"schema_version\":1")
            .unwrap();
        assert_eq!(
            store.load_messages(session_id).await.unwrap(),
            vec![first.clone()]
        );

        store.append_message(session_id, &second).await.unwrap();
        assert_eq!(
            store.load_messages(session_id).await.unwrap(),
            vec![first, second]
        );
    }

    #[tokio::test]
    async fn rejects_corruption_before_the_final_line() {
        let directory = tempdir().unwrap();
        let workdir = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let store = open_store(&factory(directory.path(), false), "project", workdir).await;
        let session_id = SessionId::new();
        let path = session_path(directory.path(), "project", session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not-json\n{\"also\":\"bad\"}").unwrap();

        let error = store.load_messages(session_id).await.unwrap_err();
        assert!(error.to_string().contains("corrupt JSONL record"));
    }

    #[tokio::test]
    async fn rejects_unsupported_schema_versions() {
        let directory = tempdir().unwrap();
        let workdir = Arc::new(NativeWorkdir::new(directory.path()).unwrap());
        let store = open_store(&factory(directory.path(), false), "project", workdir).await;
        let session_id = SessionId::new();
        let path = session_path(directory.path(), "project", session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "{{\"schema_version\":{},\"sequence\":1,\"event_id\":\"{}\",\"recorded_at\":0,\"kind\":\"message\",\"payload\":{{}}}}\n",
                SCHEMA_VERSION + 1,
                Uuid::new_v4()
            ),
        )
        .unwrap();

        let error = store.load_messages(session_id).await.unwrap_err();
        assert!(error.to_string().contains("unsupported schema version"));
    }

    #[test]
    fn session_paths_cannot_escape_the_base_directory() {
        let session_id = SessionId::new();
        let path = session_path("/state", "../../ unsafe/project ", session_id);

        assert_eq!(
            path,
            Path::new("/state")
                .join("projects")
                .join("unsafe_project")
                .join("sessions")
                .join(format!("{session_id}.jsonl"))
        );
    }
}
