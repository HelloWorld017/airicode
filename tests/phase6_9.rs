use std::{path::Path, sync::Arc};

use airicode::{
    core::{
        models::{
            ContextPriority, Message, MessagePart, ProviderId, Role, SessionGroupId, SessionId,
            ToolContext, ToolInput, ToolOutput,
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
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let session = new_session(session_id, group_id);
    session
        .operations
        .add_conversation_message(
            Message::text(Role::User, "persist me", "build", None),
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
        .add_message(Message::text(Role::Assistant, "durable", "build", None))
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
    let group_id = SessionGroupId::new();
    let session = airicode::core::SessionHandle::spawn_with_store(
        airicode::core::models::SessionState::new(SessionId::new(group_id), group_id),
        Some(Arc::new(store)),
    );
    let result = session
        .operations
        .add_message(Message::text(Role::User, "write", "build", None))
        .await;
    assert!(result.is_ok());
    assert_eq!(session.operations.snapshot().await?.last_sequence, 1);
    Ok(())
}

#[tokio::test]
async fn provider_data_survives_jsonl_persistence_round_trip() -> Result<()> {
    let directory = tempdir()?;
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let store = JsonlSessionStore::new_at(directory.path().join("provider-data"));
    let handle = airicode::core::SessionHandle::spawn_with_store(
        airicode::core::models::SessionState::new(session_id, group_id),
        Some(Arc::new(store.clone())),
    );
    let provider_id = ProviderId::new();
    let native_item = json!({
        "type": "reasoning",
        "id": "rs_1",
        "encrypted_content": "must-remain-opaque",
        "summary": []
    });
    let message = Message {
        content: vec![MessagePart::provider_only(provider_id, native_item.clone())],
        ..Message::text(Role::Assistant, "", "build", None)
    };
    handle.operations.add_message(message).await?;

    let commits = store.load(session_id).await?;
    let encoded = serde_json::to_vec(&commits)?;
    let decoded: Vec<airicode::core::models::SessionCommit> = serde_json::from_slice(&encoded)?;
    let airicode::core::models::SessionMutation::MessageAdded { message } =
        &decoded[0].mutations[0]
    else {
        panic!("expected message mutation")
    };
    assert_eq!(
        message.content[0].provider_data.as_ref().unwrap().data,
        native_item
    );
    Ok(())
}

#[tokio::test]
async fn patch_revalidates_hashline_and_stores_full_diff_as_note() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("main.rs"), b"one\ntwo\nthree\n")
        .await?;
    let group_id = SessionGroupId::new();
    let session = new_session(SessionId::new(group_id), group_id);
    let context = ToolContext {
        project_id: airicode::core::models::ProjectId::from_workdir(directory.path()),
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
            ToolInput::Text(format!(
                "EDIT main.rs\n {}|\n-{}|\n+TWO\n {}|",
                tags[0].tag, tags[1].tag, tags[2].tag
            )),
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
            ToolInput::Text(format!(
                "EDIT main.rs\n {}|\n-{}|\n+again\n {}|",
                tags[0].tag, tags[1].tag, tags[2].tag
            )),
            context,
        )
        .await?;
    assert!(matches!(stale, ToolOutput::Failure { content } if content.contains("stale patch")));
    Ok(())
}

#[tokio::test]
async fn patch_supports_add_delete_and_ambiguous_hashline_hints() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let group_id = SessionGroupId::new();
    let session = new_session(SessionId::new(group_id), group_id);
    let context = ToolContext {
        project_id: airicode::core::models::ProjectId::from_workdir(directory.path()),
        session_group_id: session.operations.group_id(),
        session_id: session.operations.session_id(),
        turn_id: airicode::core::models::TurnId::new(),
        operations: session.operations.clone(),
        workdir: workdir.clone(),
        cancellation: CancellationToken::new(),
    };
    let patch = ToolPatch::new();
    assert!(matches!(
        patch
            .execute(
                ToolInput::Text(
                    "ADD src/hi.txt\n+hi\n+how are you\n+i'm fine thank you and you".into()
                ),
                context.clone()
            )
            .await?,
        ToolOutput::Success { .. }
    ));
    assert_eq!(
        workdir.read(Path::new("src/hi.txt")).await?,
        b"hi\nhow are you\ni'm fine thank you and you\n"
    );

    workdir
        .write(
            Path::new("repeated.txt"),
            b"before one\nbefore two\nbefore three\nsame\nafter one\nafter two\nafter three\nbefore one\nbefore two\nbefore three\nsame\nafter one\nafter two\nafter three\n",
        )
        .await?;
    let repeated_tag = hashline::tag("same");
    let before_one_tag = hashline::tag("before one");
    let before_two_tag = hashline::tag("before two");
    let before_three_tag = hashline::tag("before three");
    let after_one_tag = hashline::tag("after one");
    let after_two_tag = hashline::tag("after two");
    let after_three_tag = hashline::tag("after three");
    let repeated_body = format!(
        " {before_one_tag}|\n {before_two_tag}|\n {before_three_tag}|\n-{repeated_tag}|\n+changed\n {after_one_tag}|\n {after_two_tag}|\n {after_three_tag}|"
    );
    let ambiguous = patch
        .execute(
            ToolInput::Text(format!("EDIT repeated.txt\n{repeated_body}")),
            context.clone(),
        )
        .await?;
    assert!(matches!(ambiguous, ToolOutput::Failure { content } if content.contains("ambiguous")));
    patch
        .execute(
            ToolInput::Text(format!("EDIT repeated.txt@@8\n{repeated_body}")),
            context.clone(),
        )
        .await?;
    assert_eq!(
        workdir.read(Path::new("repeated.txt")).await?,
        b"before one\nbefore two\nbefore three\nsame\nafter one\nafter two\nafter three\nbefore one\nbefore two\nbefore three\nchanged\nafter one\nafter two\nafter three\n"
    );

    patch
        .execute(
            airicode::core::models::ToolInput::Text("DEL src/hi.txt".into()),
            context,
        )
        .await?;
    assert!(workdir.read(Path::new("src/hi.txt")).await.is_err());
    Ok(())
}

#[tokio::test]
async fn patch_rejects_insufficient_context_even_with_line_hint() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("context.txt"), b"one\ntwo\nthree\nfour\nfive\n")
        .await?;
    let group_id = SessionGroupId::new();
    let session = new_session(SessionId::new(group_id), group_id);
    let context = ToolContext {
        project_id: airicode::core::models::ProjectId::from_workdir(directory.path()),
        session_group_id: session.operations.group_id(),
        session_id: session.operations.session_id(),
        turn_id: airicode::core::models::TurnId::new(),
        operations: session.operations.clone(),
        workdir: workdir.clone(),
        cancellation: CancellationToken::new(),
    };
    let patch = ToolPatch::new();
    let target_tag = hashline::tag("three");
    let insufficient = patch
        .execute(
            ToolInput::Text(format!("EDIT context.txt@@3\n-{target_tag}|\n+THREE")),
            context.clone(),
        )
        .await?;
    assert!(
        matches!(insufficient, ToolOutput::Failure { content } if content.contains("insufficient hashline context"))
    );
    assert_eq!(
        workdir.read(Path::new("context.txt")).await?,
        b"one\ntwo\nthree\nfour\nfive\n"
    );

    workdir
        .write(Path::new("short.txt"), b"one\ntwo\nthree\n")
        .await?;
    let short = hashline::render("one\ntwo\nthree\n");
    let accepted = patch
        .execute(
            ToolInput::Text(format!(
                "EDIT short.txt@@1\n {}|\n-{}|\n+TWO\n {}|",
                short[0].tag, short[1].tag, short[2].tag
            )),
            context,
        )
        .await?;
    assert!(matches!(accepted, ToolOutput::Success { .. }));
    assert_eq!(
        workdir.read(Path::new("short.txt")).await?,
        b"one\nTWO\nthree\n"
    );
    Ok(())
}
