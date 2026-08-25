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
    std::fs::write(directory.path().join("src").join("main.rs"), "fn main() {}")?;
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
    let source = directory.path().join("src");
    let components = source.join("components");
    std::fs::create_dir_all(&components)?;
    let expected_file = components.join("button.rs");
    std::fs::write(&expected_file, "pub struct Button;")?;
    let expected_main = source.join("main.rs");
    let expected_main_relative = PathBuf::from("src").join("main.rs");
    std::fs::write(&expected_main, "fn main() {}")?;
    let workdir = NativeWorkdir::new(directory.path())?;

    let file = path_correction(
        Path::new("src/componnts/button.rs"),
        &workdir,
        PathCorrectionKind::File,
    )
    .await?
    .expect("a file correction");
    assert_eq!(
        file.path,
        PathBuf::from("src").join("components").join("button.rs")
    );
    assert!(file.score > 0.9);

    let directory_correction = path_correction(
        Path::new("src/componnts"),
        &workdir,
        PathCorrectionKind::Directory,
    )
    .await?
    .expect("a directory correction");
    assert_eq!(
        directory_correction.path,
        PathBuf::from("src").join("components")
    );

    let exact = path_correction(&expected_main_relative, &workdir, PathCorrectionKind::Any)
        .await?
        .expect("an exact path");
    assert_eq!(exact.path, PathBuf::from("src").join("main.rs"));
    assert_eq!(exact.score, 1.0);
    Ok(())
}

#[tokio::test]
async fn path_correction_normalizes_both_separator_styles() -> Result<()> {
    let directory = tempdir()?;
    let expected = PathBuf::from("src").join("main.rs");
    std::fs::create_dir(directory.path().join("src"))?;
    std::fs::write(directory.path().join(&expected), "fn main() {}")?;
    let workdir = NativeWorkdir::new(directory.path())?;

    let slash = path_correction(Path::new("src/main.rs"), &workdir, PathCorrectionKind::Any)
        .await?
        .expect("slash-separated path");
    let backslash = path_correction(Path::new(r"src\main.rs"), &workdir, PathCorrectionKind::Any)
        .await?
        .expect("backslash-separated path");
    assert_eq!(slash.path, expected);
    assert_eq!(backslash.path, expected);
    assert_eq!(slash.score, 1.0);
    assert_eq!(backslash.score, 1.0);
    Ok(())
}

#[tokio::test]
async fn path_correction_returns_none_below_minimum_score() -> Result<()> {
    let directory = tempdir()?;
    std::fs::write(directory.path().join("readme.md"), "readme")?;
    let workdir = NativeWorkdir::new(directory.path())?;

    assert!(path_correction(
        Path::new("completely-unrelated-name"),
        &workdir,
        PathCorrectionKind::File,
    )
    .await?
    .is_none());
    Ok(())
}
