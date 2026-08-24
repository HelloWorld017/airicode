use std::path::PathBuf;

use airicode::core::workdir::{NativeWorkdir, Workdir};

#[tokio::main]
async fn main() -> airicode::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);

    let workdir = NativeWorkdir::new(root)?;
    println!("AiriCode workdir: {}", workdir.root().display());
    Ok(())
}
