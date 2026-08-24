use std::str::FromStr;

use airicode::{
    core::{project_from_path, ProjectId, SessionGroupId, SessionId},
    plugins::JsonlSessionStore,
    Result,
};
use tempfile::tempdir;

#[test]
fn session_ids_use_embedded_url_safe_base64_group_identity() -> Result<()> {
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let group_text = group_id.to_string();
    let session_text = session_id.to_string();

    assert_eq!(group_text.len(), 16);
    assert_eq!(session_text.len(), 24);
    assert!(group_text
        .chars()
        .all(|value| value.is_ascii_alphanumeric() || value == '-' || value == '_'));
    assert!(session_text
        .chars()
        .all(|value| value.is_ascii_alphanumeric() || value == '-' || value == '_'));
    assert_eq!(SessionGroupId::from_str(&group_text).unwrap(), group_id);
    assert_eq!(SessionId::from_str(&session_text).unwrap(), session_id);
    assert_eq!(session_id.group_id(), group_id);
    assert_eq!(
        serde_json::to_string(&session_id)?,
        format!("\"{session_text}\"")
    );
    Ok(())
}

#[test]
fn project_id_is_the_workdir_hash_string() -> Result<()> {
    let directory = tempdir()?;
    let project = project_from_path(directory.path().to_path_buf());
    let project_id = ProjectId::from_workdir(directory.path());

    assert_eq!(project.id, project_id);
    assert_eq!(project.id.to_string().len(), 16);
    assert_eq!(
        ProjectId::from_str(&project.id.to_string()).unwrap(),
        project.id
    );
    Ok(())
}

#[tokio::test]
async fn session_store_discovers_base64_session_filenames() -> Result<()> {
    let directory = tempdir()?;
    let store = JsonlSessionStore::new_at(directory.path());
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);

    tokio::fs::write(store.path_for(session_id), b"").await?;

    assert_eq!(store.discover().await?, vec![session_id]);
    Ok(())
}
