use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

pub use super::models::workdir::{Workdir, WorkdirEntry, WorkdirEntryKind, WorkdirLayer};
use super::{
    error::{Error, Result},
    models::{CommandResult, CommandSpec},
};

pub fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(Error::Workdir(format!(
            "path must be root-relative: {}",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Workdir(format!(
                    "path escapes workdir: {}",
                    path.display()
                )))
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct NativeWorkdir {
    root: Arc<PathBuf>,
    canonical_root: Arc<PathBuf>,
}

impl NativeWorkdir {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = std::fs::canonicalize(&root)
            .map_err(|error| Error::Workdir(format!("{}: {error}", root.display())))?;
        if !root.is_dir() {
            return Err(Error::Workdir(format!(
                "not a directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            canonical_root: Arc::new(root.clone()),
            root: Arc::new(root),
        })
    }

    fn logical_path(&self, path: &Path) -> Result<PathBuf> {
        validate_relative_path(path)?;
        Ok(self.root.join(path))
    }

    fn ensure_inside(&self, path: &Path) -> Result<()> {
        if path.starts_with(self.canonical_root.as_path()) {
            Ok(())
        } else {
            Err(Error::Workdir(format!(
                "path escapes workdir: {}",
                path.display()
            )))
        }
    }

    async fn existing_path(&self, path: &Path) -> Result<PathBuf> {
        let logical = self.logical_path(path)?;
        let actual = fs::canonicalize(&logical)
            .await
            .map_err(|error| Error::Workdir(format!("{}: {error}", path.display())))?;
        self.ensure_inside(&actual)?;
        Ok(actual)
    }

    async fn writable_path(&self, path: &Path) -> Result<PathBuf> {
        let logical = self.logical_path(path)?;
        let parent = logical
            .parent()
            .ok_or_else(|| Error::Workdir("path has no parent".into()))?;
        fs::create_dir_all(parent).await?;
        let canonical_parent = fs::canonicalize(parent)
            .await
            .map_err(|error| Error::Workdir(format!("{}: {error}", parent.display())))?;
        self.ensure_inside(&canonical_parent)?;
        let filename = logical
            .file_name()
            .ok_or_else(|| Error::Workdir("path has no filename".into()))?;
        Ok(canonical_parent.join(filename))
    }
}

#[async_trait]
impl Workdir for NativeWorkdir {
    fn root(&self) -> PathBuf {
        (*self.root).clone()
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        let logical = self.logical_path(path)?;
        match fs::canonicalize(&logical).await {
            Ok(actual) => {
                self.ensure_inside(&actual)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(Error::Workdir(format!("{}: {error}", path.display()))),
        }
    }

    async fn list(&self, path: &Path) -> Result<Vec<WorkdirEntry>> {
        let directory = self.existing_path(path).await?;
        let metadata = fs::metadata(&directory).await?;
        if !metadata.is_dir() {
            return Err(Error::Workdir(format!(
                "not a directory: {}",
                path.display()
            )));
        }

        let mut entries = fs::read_dir(&directory).await?;
        let mut result = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_symlink() {
                continue;
            }
            let kind = if file_type.is_dir() {
                WorkdirEntryKind::Directory
            } else if file_type.is_file() {
                WorkdirEntryKind::File
            } else {
                continue;
            };

            let actual = fs::canonicalize(entry.path()).await?;
            self.ensure_inside(&actual)?;
            let path = entry
                .path()
                .strip_prefix(self.root.as_path())
                .map_err(|error| Error::Workdir(error.to_string()))?
                .to_path_buf();
            result.push(WorkdirEntry { path, kind });
        }
        result.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(result)
    }

    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        Ok(fs::read(self.existing_path(path).await?).await?)
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let target = self.writable_path(path).await?;
        let filename = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let temp = target.with_file_name(format!(".{filename}.airicode-{}", uuid::Uuid::new_v4()));
        fs::write(&temp, data).await?;
        if let Err(error) = fs::rename(&temp, &target).await {
            let _ = fs::remove_file(&temp).await;
            return Err(error.into());
        }
        Ok(())
    }

    async fn remove(&self, path: &Path) -> Result<()> {
        fs::remove_file(self.existing_path(path).await?).await?;
        Ok(())
    }

    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandResult> {
        let cwd = match command.cwd.as_deref() {
            Some(path) => self.existing_path(path).await?,
            None => self.root(),
        };
        let mut child = Command::new(&command.program);
        child
            .args(&command.args)
            .current_dir(cwd)
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            child.process_group(0);
        }
        for (key, value) in command.env {
            child.env(key, value);
        }
        child
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = child.spawn().map_err(|error| {
            Error::Workdir(format!("failed to start {}: {error}", command.program))
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Workdir("missing child stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Workdir("missing child stderr".into()))?;
        let output_limit = command.max_output_bytes;
        let stdout_task = tokio::spawn(read_limited(stdout, output_limit));
        let stderr_task = tokio::spawn(read_limited(stderr, output_limit));
        let status = tokio::select! {
            status = child.wait() => status?,
            _ = cancellation.cancelled() => {
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    // Kill the shell and descendants together when the platform supports it.
                    unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
                }
                let _ = child.kill().await;
                return Err(Error::Cancelled);
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| Error::Workdir(error.to_string()))??;
        let stderr = stderr_task
            .await
            .map_err(|error| Error::Workdir(error.to_string()))??;
        let limit = output_limit;
        let truncated = stdout.len() > limit || stderr.len() > limit;
        let stdout = String::from_utf8_lossy(&stdout[..stdout.len().min(limit)]).into_owned();
        let stderr = String::from_utf8_lossy(&stderr[..stderr.len().min(limit)]).into_owned();
        Ok(CommandResult {
            status: status.code(),
            stdout,
            stderr,
            truncated,
        })
    }
}

async fn read_limited<R>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let retained_limit = limit.saturating_add(1);
    let mut retained = Vec::with_capacity(retained_limit.min(8192));
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if retained.len() < retained_limit {
            let remaining = retained_limit - retained.len();
            retained.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(retained)
}
