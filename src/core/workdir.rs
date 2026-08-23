use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{Error, EventSink, Message, ProviderId, ProviderRegistry, Result, WorkdirLayerId};

const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait Workdir: Send + Sync {
    fn root(&self) -> &Path;
    async fn read(&self, path: &Path) -> Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> Result<()>;
    async fn remove(&self, path: &Path) -> Result<()>;
    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput>;

    #[doc(hidden)]
    fn tool_services(&self) -> Option<&ToolServices> {
        None
    }
}

pub struct ToolServices {
    pub(crate) provider_id: ProviderId,
    pub(crate) model: String,
    pub(crate) providers: ProviderRegistry,
    pub(crate) messages: Arc<[Message]>,
    pub(crate) events: Arc<dyn EventSink>,
}

#[derive(Clone, Debug)]
pub struct WorkdirLayerContext {
    pub project_id: super::ProjectId,
    pub project_name: String,
}

pub trait WorkdirLayer: Send + Sync {
    fn id(&self) -> WorkdirLayerId;
    fn layer(&self, context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir>;
}

#[derive(Clone, Debug)]
pub struct NativeWorkdir {
    root: Arc<PathBuf>,
    max_output_bytes: usize,
}

impl NativeWorkdir {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref()).map_err(|error| {
            Error::Workdir(format!(
                "could not canonicalize {}: {error}",
                root.as_ref().display()
            ))
        })?;
        if !root.is_dir() {
            return Err(Error::Workdir(format!(
                "workdir root is not a directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            root: Arc::new(root),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        })
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    fn validate_relative(path: &Path) -> Result<()> {
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(Error::Workdir(format!(
                "path must be non-empty and project-relative: {}",
                path.display()
            )));
        }
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(Error::Workdir(format!(
                "path may not contain parent components: {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn resolve(&self, path: &Path, allow_missing: bool) -> Result<PathBuf> {
        Self::validate_relative(path)?;
        let candidate = self.root.join(path);
        match fs::canonicalize(&candidate) {
            Ok(resolved) => {
                if resolved.starts_with(self.root.as_path()) {
                    Ok(resolved)
                } else {
                    Err(Error::Workdir(format!(
                        "path escapes the workdir through a symlink: {}",
                        path.display()
                    )))
                }
            }
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => {
                let mut ancestor = candidate.as_path();
                while !ancestor.exists() {
                    ancestor = ancestor.parent().ok_or_else(|| {
                        Error::Workdir(format!("invalid path: {}", path.display()))
                    })?;
                }
                let resolved_ancestor = fs::canonicalize(ancestor).map_err(|error| {
                    Error::Workdir(format!("could not resolve {}: {error}", path.display()))
                })?;
                if !resolved_ancestor.starts_with(self.root.as_path()) {
                    return Err(Error::Workdir(format!(
                        "path escapes the workdir through a symlink: {}",
                        path.display()
                    )));
                }
                Ok(candidate)
            }
            Err(error) => Err(Error::Workdir(format!(
                "could not resolve {}: {error}",
                path.display()
            ))),
        }
    }

    fn io_error(action: &str, path: &Path, error: io::Error) -> Error {
        Error::Workdir(format!("could not {action} {}: {error}", path.display()))
    }
}

#[async_trait]
impl Workdir for NativeWorkdir {
    fn root(&self) -> &Path {
        self.root.as_path()
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let resolved = self.resolve(path, false)?;
        let display = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            fs::read(resolved).map_err(|error| Self::io_error("read", &display, error))
        })
        .await
        .map_err(|error| Error::Workdir(format!("read task failed: {error}")))?
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let destination = self.resolve(path, true)?;
        let display = path.to_path_buf();
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            let parent = destination.parent().ok_or_else(|| {
                Error::Workdir(format!("path has no parent: {}", display.display()))
            })?;
            fs::create_dir_all(parent)
                .map_err(|error| Self::io_error("create parent for", &display, error))?;
            let temporary = parent.join(format!(".airicode-{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| {
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                let mut file = options.open(&temporary).map_err(|error| {
                    Self::io_error("create temporary file for", &display, error)
                })?;
                file.write_all(&data)
                    .and_then(|_| file.sync_all())
                    .map_err(|error| Self::io_error("write", &display, error))?;
                if let Ok(metadata) = fs::metadata(&destination) {
                    fs::set_permissions(&temporary, metadata.permissions()).map_err(|error| {
                        Self::io_error("preserve permissions for", &display, error)
                    })?;
                }
                fs::rename(&temporary, &destination)
                    .map_err(|error| Self::io_error("replace", &display, error))
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result
        })
        .await
        .map_err(|error| Error::Workdir(format!("write task failed: {error}")))?
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        let resolved = self.resolve(path, false)?;
        let source = self.root.join(path);
        let display = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let metadata = fs::symlink_metadata(&source)
                .map_err(|error| Self::io_error("inspect", &display, error))?;
            if metadata.file_type().is_symlink() {
                fs::remove_file(source)
            } else if resolved.is_dir() {
                fs::remove_dir_all(source)
            } else {
                fs::remove_file(source)
            }
            .map_err(|error| Self::io_error("remove", &display, error))
        })
        .await
        .map_err(|error| Error::Workdir(format!("remove task failed: {error}")))?
    }

    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let cwd = match command.cwd.as_deref() {
            Some(path) => self.resolve(path, false)?,
            None => self.root.as_ref().clone(),
        };
        if !cwd.is_dir() {
            return Err(Error::Workdir(format!(
                "command cwd is not a directory: {}",
                cwd.display()
            )));
        }
        let max_output_bytes = self.max_output_bytes;
        tokio::task::spawn_blocking(move || {
            let mut child = Command::new(&command.program)
                .args(&command.args)
                .current_dir(cwd)
                .envs(&command.env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    Error::Workdir(format!("could not start {}: {error}", command.program))
                })?;

            fn collect<R: Read + Send + 'static>(
                mut reader: R,
                limit: usize,
            ) -> (thread::JoinHandle<()>, Arc<Mutex<Vec<u8>>>) {
                let output = Arc::new(Mutex::new(Vec::with_capacity(limit.min(8192))));
                let writer = output.clone();
                let handle = thread::spawn(move || {
                    let mut buffer = [0_u8; 8192];
                    while let Ok(count) = reader.read(&mut buffer) {
                        if count == 0 {
                            break;
                        }
                        let mut output = writer.lock().expect("output lock poisoned");
                        let remaining = limit.saturating_sub(output.len());
                        output.extend_from_slice(&buffer[..count.min(remaining)]);
                    }
                });
                (handle, output)
            }

            fn finish_capture(
                handle: thread::JoinHandle<()>,
                output: Arc<Mutex<Vec<u8>>>,
                wait: bool,
            ) -> Vec<u8> {
                if wait {
                    let deadline = Instant::now() + Duration::from_millis(100);
                    while !handle.is_finished() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                if handle.is_finished() {
                    let _ = handle.join();
                }
                output.lock().map(|bytes| bytes.clone()).unwrap_or_default()
            }

            let (stdout_handle, stdout) =
                collect(child.stdout.take().expect("piped stdout"), max_output_bytes);
            let (stderr_handle, stderr) =
                collect(child.stderr.take().expect("piped stderr"), max_output_bytes);
            let status = loop {
                if cancellation.is_cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(finish_capture(stdout_handle, stdout, false));
                    drop(finish_capture(stderr_handle, stderr, false));
                    return Err(Error::Cancelled);
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => thread::sleep(Duration::from_millis(10)),
                    Err(error) => {
                        let _ = child.kill();
                        return Err(Error::Workdir(format!(
                            "could not wait for {}: {error}",
                            command.program
                        )));
                    }
                }
            };
            Ok(CommandOutput {
                status: status.code().unwrap_or(-1),
                stdout: finish_capture(stdout_handle, stdout, true),
                stderr: finish_capture(stderr_handle, stderr, true),
            })
        })
        .await
        .map_err(|error| Error::Workdir(format!("command task failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_parent_paths_and_writes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let workdir = NativeWorkdir::new(directory.path()).unwrap();
        assert!(workdir.read(Path::new("../outside")).await.is_err());

        workdir
            .write(Path::new("nested/file.txt"), b"hello")
            .await
            .unwrap();
        assert_eq!(
            workdir.read(Path::new("nested/file.txt")).await.unwrap(),
            b"hello"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape_and_cancels_commands() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), directory.path().join("escape")).unwrap();
        let workdir = NativeWorkdir::new(directory.path()).unwrap();
        assert!(workdir.read(Path::new("escape/file")).await.is_err());

        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let task = tokio::spawn(async move {
            workdir
                .execute(
                    CommandSpec {
                        program: "sh".into(),
                        args: vec!["-c".into(), "sleep 10".into()],
                        cwd: None,
                        env: Default::default(),
                    },
                    cancellation,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(Error::Cancelled)));
    }
}
