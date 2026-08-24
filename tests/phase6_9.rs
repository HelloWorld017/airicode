use std::{path::Path, sync::Arc};

use airicode::{
    core::{
        models::{
            ContextPriority, Message, Role, SessionGroupId, SessionId, ToolContext, ToolOutput,
        },
        operations::new_session,
        persistence::SessionStore,
        workdir::{NativeWorkdir, Workdir},
        Tool,
    },
    plugins::{JsonlSessionStore, ToolPatch},
    utils::hashline,
    Result,
};
use serde_json::json;
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn jsonl_store_replays_and_recovers_an_incomplete_tail() -> Result<()> {
    let directory = tempdir()?;
    let session_id = SessionId::new();
    let session = new_session(session_id, SessionGroupId::new());
    session
        .operations
        .add_conversation_message(
            Message::text(Role::User, "persist me", None),
            ContextPriority::High,
        )
        .await?;
    let state = session.operations.snapshot().await?;
    let persisted = JsonlSessionStore::new_at(directory.path().join("actor"));
    let handle = airicode::core::SessionHandle::spawn_with_store(
        airicode::core::models::SessionState::new(session_id, state.group_id),
        Some(Arc::new(persisted.clone())),
    );
    handle
        .operations
        .add_message(Message::text(Role::Assistant, "durable", None))
        .await?;
    let log = persisted.path_for(session_id);
    let mut partial = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .await?;
    partial.write_all(b"{\"schema_version\":1").await?;
    partial.flush().await?;
    assert_eq!(persisted.load(session_id).await?.len(), 1);
    let next = airicode::core::models::SessionCommit::new(
        2,
        vec![airicode::core::models::SessionMutation::NoteAdded {
            note: airicode::core::models::Note {
                id: airicode::core::models::NoteId::new(),
                content: airicode::core::models::NoteContent::Info {
                    content: "after tail".into(),
                },
                created_at: airicode::utils::TimeSeq::new(),
                metadata: Default::default(),
            },
        }],
    );
    persisted.append(session_id, &next).await?;
    assert_eq!(persisted.load(session_id).await?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn persisted_session_actor_does_not_advance_when_append_fails() -> Result<()> {
    let directory = tempdir()?;
    let store = JsonlSessionStore::new_at(directory.path().join("missing"));
    let session = airicode::core::SessionHandle::spawn_with_store(
        airicode::core::models::SessionState::new(SessionId::new(), SessionGroupId::new()),
        Some(Arc::new(store)),
    );
    let result = session
        .operations
        .add_message(Message::text(Role::User, "write", None))
        .await;
    assert!(result.is_ok());
    assert_eq!(session.operations.snapshot().await?.last_sequence, 1);
    Ok(())
}

#[tokio::test]
async fn patch_revalidates_hashline_and_stores_full_diff_as_note() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("main.rs"), b"one\ntwo\nthree\n")
        .await?;
    let session = new_session(SessionId::new(), SessionGroupId::new());
    let context = ToolContext {
        project_id: airicode::core::models::ProjectId::new(),
        session_group_id: session.operations.group_id(),
        session_id: session.operations.session_id(),
        turn_id: airicode::core::models::TurnId::new(),
        operations: session.operations.clone(),
        workdir: workdir.clone(),
        cancellation: CancellationToken::new(),
    };
    let tags = hashline::render("one\ntwo\nthree\n");
    let patch = ToolPatch::new();
    let first = patch
        .execute(
            json!({
                "path": "main.rs",
                "start": format!("{}:{}|", tags[1].line, tags[1].tag),
                "replacement": "TWO"
            }),
            context.clone(),
        )
        .await?;
    assert!(
        matches!(first, ToolOutput::Success { content } if content.contains("Updated main.rs"))
    );
    assert_eq!(
        workdir.read(Path::new("main.rs")).await?,
        b"one\nTWO\nthree\n"
    );
    assert_eq!(session.operations.snapshot().await?.notes.len(), 1);
    let stale = patch
        .execute(
            json!({
                "path": "main.rs",
                "start": format!("{}:{}|", tags[1].line, tags[1].tag),
                "replacement": "again"
            }),
            context,
        )
        .await?;
    assert!(matches!(stale, ToolOutput::Failure { content } if content.contains("stale patch")));
    Ok(())
}
