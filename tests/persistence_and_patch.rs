use std::{path::Path, sync::Arc};

use airicode::{
    Result,
    core::{
        CoreBuilder, Plugin, SessionHandle, Tool,
        models::{
            ContextPriority, Message, MessagePart, PluginId, ProviderId, Role, SessionGroupId,
            SessionId, SessionState, ToolContext, ToolOutput, TurnId, UIState,
        },
        persistence::SessionStore,
        project_from_path,
        registry::PluginRegistryScope,
        workdir::{NativeWorkdir, Workdir},
    },
    plugins::{JsonlSessionStore, ToolPatch, ToolPatchApplyPatch, ToolPatchHashline, ToolWrite},
    utils::hashline,
};
use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

struct SessionStorePlugin {
    id: PluginId,
    store: Arc<dyn SessionStore>,
}

#[async_trait]
impl Plugin for SessionStorePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "test_session_store"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_session_store(self.store.clone(), 0)?;
        Ok(())
    }
}

async fn spawn_session(
    directory: &tempfile::TempDir,
    state: SessionState,
    store: Option<Arc<dyn SessionStore>>,
) -> Result<SessionHandle> {
    let mut builder = CoreBuilder::new().project(project_from_path(directory.path().to_path_buf()));
    if let Some(store) = store {
        builder = builder.plugin(Arc::new(SessionStorePlugin {
            id: PluginId::new(),
            store,
        }));
    }
    builder.build().await?.open_session(state)
}

#[tokio::test]
async fn jsonl_store_replays_and_recovers_an_incomplete_tail() -> Result<()> {
    let directory = tempdir()?;
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let session = spawn_session(&directory, SessionState::new(session_id, group_id), None).await?;
    session
        .operations()
        .add_conversation_message(
            Message::text(Role::User, "persist me", "build", None),
            ContextPriority::High,
        )
        .await?;
    let state = session.operations().snapshot().await?;
    let persisted = JsonlSessionStore::new_at(directory.path().join("actor"));
    let handle = spawn_session(
        &directory,
        SessionState::new(session_id, state.group_id),
        Some(Arc::new(persisted.clone())),
    )
    .await?;
    handle
        .operations()
        .add_message(Message::text(Role::Assistant, "durable", "build", None))
        .await?;
    let log = persisted.path_for(session_id);
    let mut partial = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .await?;
    partial.write_all(b"{\"schema_version\":2").await?;
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
    let session = spawn_session(
        &directory,
        SessionState::new(SessionId::new(group_id), group_id),
        Some(Arc::new(store)),
    )
    .await?;
    assert!(
        session
            .operations()
            .add_message(Message::text(Role::User, "write", "build", None))
            .await
            .is_ok()
    );
    assert_eq!(session.operations().snapshot().await?.last_sequence, 1);
    Ok(())
}

#[tokio::test]
async fn ui_state_survives_jsonl_replay() -> Result<()> {
    let directory = tempdir()?;
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let store = JsonlSessionStore::new_at(directory.path().join("ui-state"));
    let session = spawn_session(
        &directory,
        SessionState::new(session_id, group_id),
        Some(Arc::new(store.clone())),
    )
    .await?;
    let ui = UIState {
        selected_model: None,
        selected_mode: Some("plan".into()),
        selected_variant: Some("review".into()),
    };

    session.operations().update_ui_state(ui.clone()).await?;
    let replayed = SessionState::replay(session_id, group_id, store.load(session_id).await?)?;
    assert_eq!(replayed.ui, ui);
    Ok(())
}

#[tokio::test]
async fn provider_data_survives_jsonl_persistence_round_trip() -> Result<()> {
    let directory = tempdir()?;
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let store = JsonlSessionStore::new_at(directory.path().join("provider-data"));
    let handle = spawn_session(
        &directory,
        SessionState::new(session_id, group_id),
        Some(Arc::new(store.clone())),
    )
    .await?;
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
    handle.operations().add_message(message).await?;

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

async fn context(directory: &tempfile::TempDir) -> Result<(SessionHandle, ToolContext)> {
    let group_id = SessionGroupId::new();
    let session = spawn_session(
        directory,
        SessionState::new(SessionId::new(group_id), group_id),
        None,
    )
    .await?;
    let context = ToolContext {
        turn_id: TurnId::new(),
        operations: session.operations(),
        cancellation: CancellationToken::new(),
    };
    Ok((session, context))
}

#[tokio::test]
async fn patch_matches_all_replacements_against_one_snapshot() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("main.txt"), b"first one\nsecond two\n")
        .await?;
    let (_session, context) = context(&directory).await?;
    let output = ToolPatch::new()
        .execute(
            json!({
                "path": "main.txt",
                "edits": [
                    { "oldText": "one", "newText": "ONE" },
                    { "oldText": "two", "newText": "TWO" }
                ]
            }),
            context,
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
    let (_first_session, first_context) = context(&directory).await?;
    let output = ToolPatch::new()
        .execute(
            json!({
                "path": "main.txt",
                "edits": [{ "oldText": "abc", "newText": "def" }]
            }),
            first_context,
        )
        .await?;
    assert!(matches!(output, ToolOutput::Failure { content } if content.contains("exactly once")));
    assert_eq!(workdir.read(Path::new("main.txt")).await?, b"abc abc\n");

    workdir.write(Path::new("main.txt"), b"abcdef\n").await?;
    let (_second_session, second_context) = context(&directory).await?;
    let output = ToolPatch::new()
        .execute(
            json!({
                "path": "main.txt",
                "edits": [
                    { "oldText": "abc", "newText": "x" },
                    { "oldText": "bcd", "newText": "y" }
                ]
            }),
            second_context,
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
    let (_first_session, first_context) = context(&directory).await?;
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
        first_context,
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
    let (_second_session, second_context) = context(&directory).await?;
    tool.execute(input, second_context).await?;
    assert_eq!(workdir.read(Path::new("main.txt")).await?, b"one\nthree\n");
    Ok(())
}

#[tokio::test]
async fn apply_patch_supports_file_lifecycle_operations() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let (_session, ctx) = context(&directory).await?;
    ToolWrite::new()
        .execute(
            json!({ "path": "main.txt", "content": "before\n" }),
            ctx.clone(),
        )
        .await?;
    workdir
        .write(Path::new("obsolete.txt"), b"obsolete\n")
        .await?;
    ToolPatchApplyPatch::new()
        .execute(
            json!({ "patch": "*** Begin Patch\n*** Add File: new.txt\n+new\n*** Update File: main.txt\n*** Move to: moved.txt\n@@\n-before\n+after\n*** Delete File: obsolete.txt\n*** End Patch" }),
            ctx.clone(),
        )
        .await?;
    assert_eq!(workdir.read(Path::new("new.txt")).await?, b"new\n");
    assert_eq!(workdir.read(Path::new("moved.txt")).await?, b"after\n");
    assert!(!workdir.exists(Path::new("main.txt")).await?);
    assert!(!workdir.exists(Path::new("obsolete.txt")).await?);
    Ok(())
}
