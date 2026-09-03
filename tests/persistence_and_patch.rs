use std::{path::Path, sync::Arc};

use airicode::{
    Result,
    core::{
        Tool,
        models::{
            ContextPriority, Message, MessagePart, ProjectId, ProviderId, Role, SessionGroupId,
            SessionId, ToolContext, ToolOutput, TurnId,
        },
        operations::new_session,
        persistence::SessionStore,
        workdir::{NativeWorkdir, Workdir},
    },
    plugins::{
        JsonlSessionStore, ToolFsDelete, ToolFsWrite, ToolPatch, ToolPatchApplyPatch,
        ToolPatchHashline,
    },
    utils::hashline,
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
    assert!(
        session
            .operations
            .add_message(Message::text(Role::User, "write", "build", None))
            .await
            .is_ok()
    );
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

fn context(directory: &tempfile::TempDir, workdir: Arc<dyn Workdir>) -> ToolContext {
    let group_id = SessionGroupId::new();
    let session = new_session(SessionId::new(group_id), group_id);
    ToolContext {
        project_id: ProjectId::from_workdir(directory.path()),
        session_group_id: group_id,
        session_id: session.operations.session_id(),
        turn_id: TurnId::new(),
        operations: session.operations,
        workdir,
        cancellation: CancellationToken::new(),
    }
}

#[tokio::test]
async fn patch_matches_all_replacements_against_one_snapshot() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("main.txt"), b"first one\nsecond two\n")
        .await?;
    let output = ToolPatch::new()
        .execute(
            json!({
                "path": "main.txt",
                "edits": [
                    { "oldText": "one", "newText": "ONE" },
                    { "oldText": "two", "newText": "TWO" }
                ]
            }),
            context(&directory, workdir.clone()),
        )
        .await?;
    assert!(matches!(output, ToolOutput::Success { .. }));
    assert_eq!(
        workdir.read(Path::new("main.txt")).await?,
        b"first ONE\nsecond TWO\n"
    );
    Ok(())
}

#[tokio::test]
async fn patch_rejects_non_unique_or_overlapping_edits_without_writing() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir.write(Path::new("main.txt"), b"abc abc\n").await?;
    let output = ToolPatch::new()
        .execute(
            json!({
                "path": "main.txt",
                "edits": [{ "oldText": "abc", "newText": "def" }]
            }),
            context(&directory, workdir.clone()),
        )
        .await?;
    assert!(matches!(output, ToolOutput::Failure { content } if content.contains("exactly once")));
    assert_eq!(workdir.read(Path::new("main.txt")).await?, b"abc abc\n");

    workdir.write(Path::new("main.txt"), b"abcdef\n").await?;
    let output = ToolPatch::new()
        .execute(
            json!({
                "path": "main.txt",
                "edits": [
                    { "oldText": "abc", "newText": "x" },
                    { "oldText": "bcd", "newText": "y" }
                ]
            }),
            context(&directory, workdir.clone()),
        )
        .await?;
    assert!(matches!(output, ToolOutput::Failure { content } if content.contains("overlap")));
    assert_eq!(workdir.read(Path::new("main.txt")).await?, b"abcdef\n");
    Ok(())
}

#[tokio::test]
async fn hashline_patch_json_and_freeform_inputs_share_the_executor() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let original = "one\ntwo\nthree\n";
    let tags = hashline::render(original);
    workdir
        .write(Path::new("main.txt"), original.as_bytes())
        .await?;
    let tool = ToolPatchHashline::new();
    tool.execute(
        json!({
            "operations": [{
                "kind": "replace",
                "path": "main.txt",
                "anchor_start": format!("{}:{}", tags[1].line, tags[1].tag),
                "anchor_end": format!("{}:{}", tags[1].line, tags[1].tag),
                "lines": ["TWO"]
            }]
        }),
        context(&directory, workdir.clone()),
    )
    .await?;
    assert_eq!(
        workdir.read(Path::new("main.txt")).await?,
        b"one\nTWO\nthree\n"
    );

    let fresh = hashline::render("one\nTWO\nthree\n");
    let input = ToolPatchHashline::new()
        .definition()
        .input
        .parse_freeform(&format!(
            "DELETE main.txt FROM {}:{} TO {}:{}",
            fresh[1].line, fresh[1].tag, fresh[1].line, fresh[1].tag
        ))?;
    tool.execute(input, context(&directory, workdir.clone()))
        .await?;
    assert_eq!(workdir.read(Path::new("main.txt")).await?, b"one\nthree\n");
    Ok(())
}

#[tokio::test]
async fn filesystem_tools_own_file_lifecycle_and_apply_patch_edits_existing_files() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let ctx = context(&directory, workdir.clone());
    ToolFsWrite::new()
        .execute(
            json!({ "path": "main.txt", "content": "before\n" }),
            ctx.clone(),
        )
        .await?;
    ToolPatchApplyPatch::new()
        .execute(
            json!({ "patch": "*** Begin Patch\n*** Update File: main.txt\n@@\n-before\n+after\n*** End Patch" }),
            ctx.clone(),
        )
        .await?;
    assert_eq!(workdir.read(Path::new("main.txt")).await?, b"after\n");
    let output = ToolPatchApplyPatch::new()
        .execute(
            json!({ "patch": "*** Begin Patch\n*** Add File: new.txt\n+x\n*** End Patch" }),
            ctx.clone(),
        )
        .await?;
    assert!(matches!(output, ToolOutput::Failure { content } if content.contains("fs_write")));
    ToolFsDelete::new()
        .execute(json!({ "path": "main.txt" }), ctx)
        .await?;
    assert!(!workdir.exists(Path::new("main.txt")).await?);
    Ok(())
}
