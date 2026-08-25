use std::path::{Path, PathBuf};

use airicode::{
    core::workdir::{NativeWorkdir, Workdir, WorkdirEntryKind},
    utils::{path_correction, PathCorrectionKind},
    Result,
};
use tempfile::tempdir;

#[tokio::test]
async fn native_workdir_exists_and_lists_root_relative_entries() -> Result<()> {
    let directory = tempdir()?;
    std::fs::create_dir(directory.path().join("src"))?;
    std::fs::write(directory.path().join("root.txt"), "root")?;
    std::fs::write(directory.path().join("src/main.rs"), "fn main() {}")?;
    let workdir = NativeWorkdir::new(directory.path())?;

    assert!(workdir.exists(Path::new("root.txt")).await?);
    assert!(workdir.exists(Path::new("src")).await?);
    assert!(!workdir.exists(Path::new("missing.txt")).await?);
    assert!(workdir.exists(Path::new("../outside")).await.is_err());

    let entries = workdir.list(Path::new(".")).await?;
    assert_eq!(
        entries
            .iter()
            .map(|entry| (&entry.path, entry.kind))
            .collect::<Vec<_>>(),
        vec![
            (&PathBuf::from("root.txt"), WorkdirEntryKind::File),
            (&PathBuf::from("src"), WorkdirEntryKind::Directory),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn path_correction_supports_file_and_directory_filters() -> Result<()> {
    let directory = tempdir()?;
    std::fs::create_dir_all(directory.path().join("src/components"))?;
    std::fs::write(
        directory.path().join("src/components/button.rs"),
        "pub struct Button;",
    )?;
    std::fs::write(directory.path().join("src/main.rs"), "fn main() {}")?;
    let workdir = NativeWorkdir::new(directory.path())?;

    let file = path_correction(
        Path::new("src/componnts/button.rs"),
        &workdir,
        PathCorrectionKind::File,
    )
    .await?
    .expect("a file correction");
    assert_eq!(file.path, PathBuf::from("src/components/button.rs"));
    assert!(file.score > 0.9);

    let directory_correction = path_correction(
        Path::new("src/componnts"),
        &workdir,
        PathCorrectionKind::Directory,
    )
    .await?
    .expect("a directory correction");
    assert_eq!(directory_correction.path, PathBuf::from("src/components"));

    let exact = path_correction(Path::new("src/main.rs"), &workdir, PathCorrectionKind::Any)
        .await?
        .expect("an exact path");
    assert_eq!(exact.path, PathBuf::from("src/main.rs"));
    assert_eq!(exact.score, 1.0);
    Ok(())
}

#[tokio::test]
async fn path_correction_returns_none_when_no_path_matches_kind() -> Result<()> {
    let directory = tempdir()?;
    let workdir = NativeWorkdir::new(directory.path())?;

    assert!(
        path_correction(Path::new("missing.txt"), &workdir, PathCorrectionKind::File)
            .await?
            .is_none()
    );
    let root = path_correction(
        Path::new("missing"),
        &workdir,
        PathCorrectionKind::Directory,
    )
    .await?
    .expect("the workdir root is a directory");
    assert_eq!(root.path, PathBuf::from("."));
    Ok(())
}
