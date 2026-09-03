use std::{path::Path, sync::Arc, time::Duration};

use airicode::{
    Result,
    core::{
        Tool,
        models::{
            MessagePart, ProjectId, ProviderEvent, ProviderId, Role, RuntimeEvent, SessionGroupId,
            ToolContext, ToolOutput, TurnId,
        },
        operations::new_session,
        runtime::{TurnEngine, TurnRequest},
        workdir::{NativeWorkdir, Workdir},
    },
    plugins::{ToolFindFile, ToolGrep, ToolRead, ToolReadPlugin, ToolShell, ToolShellPlugin},
};
use serde_json::json;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

mod utils;

use utils::{FakeProvider, FakeProviderPlugin};

#[tokio::test]
async fn native_workdir_reads_writes_and_executes_commands() -> Result<()> {
    let directory = tempdir()?;
    let workdir = NativeWorkdir::new(directory.path())?;

    workdir.write(Path::new("src/file.txt"), b"hello").await?;
    assert_eq!(workdir.read(Path::new("src/file.txt")).await?, b"hello");

    let result = workdir
        .execute(
            airicode::core::models::CommandSpec::new("sh", ["-c", "printf command-output"]),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(result.status, Some(0));
    assert_eq!(result.stdout, "command-output");
    Ok(())
}

#[tokio::test]
async fn read_and_shell_tools_use_the_shared_workdir_contract() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("main.rs"), b"fn main() {}\n")
        .await?;
    let group_id = SessionGroupId::new();
    let session = new_session(airicode::core::models::SessionId::new(group_id), group_id);
    let context = ToolContext {
        project_id: ProjectId::from_workdir(directory.path()),
        session_group_id: group_id,
        session_id: session.operations.session_id(),
        turn_id: TurnId::new(),
        operations: session.operations.clone(),
        workdir: workdir.clone(),
        cancellation: CancellationToken::new(),
    };

    let read = ToolRead::new();
    let output = read
        .execute(json!({ "path": "main.rs" }), context.clone())
        .await?;
    let ToolOutput::Success { content } = output else {
        panic!("read failed")
    };
    assert!(content.starts_with("1|"));
    assert!(content.contains("|fn main() {}"));

    let shell = ToolShell::new();
    assert!(shell.definition().input.freeform_parser.is_some());
    let output = shell
        .execute(json!({ "command": "printf shell-output" }), context)
        .await?;
    let ToolOutput::Success { content } = output else {
        panic!("shell failed")
    };
    assert!(content.contains("exit 0"));
    assert!(content.contains("shell-output"));
    Ok(())
}

#[tokio::test]
async fn read_suggests_a_corrected_path_and_grep_accepts_an_empty_path() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("src/main.rs"), b"before\nneedle\nafter\n")
        .await?;
    let group_id = SessionGroupId::new();
    let session = new_session(airicode::core::models::SessionId::new(group_id), group_id);
    let context = ToolContext {
        project_id: ProjectId::from_workdir(directory.path()),
        session_group_id: group_id,
        session_id: session.operations.session_id(),
        turn_id: TurnId::new(),
        operations: session.operations.clone(),
        workdir,
        cancellation: CancellationToken::new(),
    };

    let read = ToolRead::new();
    let output = read
        .execute(json!({ "path": "src/mian.rs" }), context.clone())
        .await?;
    assert!(matches!(
        output,
        ToolOutput::Failure { content }
            if content.contains("Did you mean? src/main.rs")
    ));

    let grep = ToolGrep::new();
    let output = grep
        .execute(json!({ "pattern": "needle", "path": "" }), context)
        .await?;
    assert!(
        matches!(output, ToolOutput::Success { content } if content == "./src/main.rs:2|needle")
    );
    Ok(())
}

#[tokio::test]
async fn find_file_supports_exact_keyword_and_glob_queries() -> Result<()> {
    let directory = tempdir()?;
    std::fs::create_dir_all(directory.path().join("src/models"))?;
    std::fs::write(directory.path().join("src/main.rs"), "fn main() {}")?;
    std::fs::write(directory.path().join("src/models/Message.rs"), "model")?;
    std::fs::write(directory.path().join("README.md"), "readme")?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let group_id = SessionGroupId::new();
    let session = new_session(airicode::core::models::SessionId::new(group_id), group_id);
    let context = ToolContext {
        project_id: ProjectId::from_workdir(directory.path()),
        session_group_id: group_id,
        session_id: session.operations.session_id(),
        turn_id: TurnId::new(),
        operations: session.operations,
        workdir,
        cancellation: CancellationToken::new(),
    };

    let find = ToolFindFile::new();
    let output = find
        .execute(
            json!({
                "query": { "kind": "by_filename_exact", "filename": "main.rs" }
            }),
            context.clone(),
        )
        .await?;
    assert!(matches!(output, ToolOutput::Success { content } if content == "src/main.rs"));

    let output = find
        .execute(
            json!({
                "query": { "kind": "by_filename_keyword", "keyword": "MESSAGE" },
                "path": "src"
            }),
            context.clone(),
        )
        .await?;
    assert!(
        matches!(output, ToolOutput::Success { content } if content == "src/models/Message.rs")
    );

    let output = find
        .execute(
            json!({
                "query": { "kind": "by_glob_pattern", "pattern": "src/**/*.rs" }
            }),
            context.clone(),
        )
        .await?;
    assert!(
        matches!(output, ToolOutput::Success { content } if content == "src/main.rs\nsrc/models/Message.rs")
    );

    let limited = ToolFindFile::new().with_limits(1, 128 * 1024);
    let output = limited
        .execute(
            json!({
                "query": { "kind": "by_filename_keyword", "keyword": ".rs" }
            }),
            context.clone(),
        )
        .await?;
    assert!(matches!(
        output,
        ToolOutput::Success { content }
            if content.starts_with("src/main.rs\n")
                && content.contains("Showing 1 of 2 matching files.")
    ));

    let output = find
        .execute(
            json!({
                "query": { "kind": "by_filename_exact", "filename": "missing.txt" }
            }),
            context,
        )
        .await?;
    assert!(matches!(output, ToolOutput::Success { content } if content == "No files matched."));
    Ok(())
}

#[tokio::test]
async fn fake_provider_completes_a_read_tool_turn_on_a_real_project() -> Result<()> {
    let directory = tempdir()?;
    std::fs::write(directory.path().join("hello.txt"), "hello from project\n")?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);

    let provider_id = ProviderId::new();
    let provider = Arc::new(FakeProvider::new(
        provider_id,
        [
            vec![
                ProviderEvent::TextDelta {
                    text: "discarded delta".into(),
                },
                ProviderEvent::OutputPart {
                    index: 0,
                    part: MessagePart::tool_call(
                        airicode::core::models::ToolCallId::from_external("call-1"),
                        "read".into(),
                        json!({ "path": "hello.txt" }),
                    )
                    .with_provider_data(
                        provider_id,
                        json!({
                            "type": "function_call",
                            "id": "fc-1",
                            "call_id": "call-1",
                            "name": "read",
                            "arguments": "{\"path\":\"hello.txt\"}"
                        }),
                    ),
                },
                ProviderEvent::Finished {
                    reason: airicode::core::models::FinishReason::ToolCalls,
                },
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "discarded delta".into(),
                },
                ProviderEvent::OutputPart {
                    index: 0,
                    part: MessagePart::text("The file says hello from project.")
                        .with_provider_data(
                            provider_id,
                            json!({
                                "type": "message",
                                "id": "msg-2",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": "The file says hello from project.",
                                    "annotations": []
                                }]
                            }),
                        ),
                },
                ProviderEvent::Finished {
                    reason: airicode::core::models::FinishReason::Stop,
                },
            ],
        ],
    ));
    let fake_plugin = Arc::new(FakeProviderPlugin::new(provider));
    let core = airicode::core::CoreBuilder::new()
        .plugin(fake_plugin)
        .plugin(Arc::new(ToolReadPlugin::new()))
        .plugin(Arc::new(ToolShellPlugin::new()))
        .build()
        .await?;
    let group_id = SessionGroupId::new();
    let session = core.create_session(group_id);
    let engine = TurnEngine::new(core.registry(), session.operations.clone(), workdir);
    let request = TurnRequest::new(
        ProjectId::from_workdir(directory.path()),
        group_id,
        session.operations.session_id(),
        provider_id,
        "fake-model",
        "build",
        "Read hello.txt",
    );
    engine.run(request).await?;

    let state = session.operations.snapshot().await?;
    assert_eq!(
        state
            .visible_messages()
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert_eq!(
        state
            .visible_messages()
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count(),
        2
    );
    assert!(state.visible_messages().iter().any(|message| {
        message.content.iter().any(|part| matches!(part.content.as_ref(), Some(airicode::core::models::MessagePartContent::ToolResult { result: ToolOutput::Success { content }, .. }) if content.contains("hello from project")))
    }));
    assert!(state.visible_messages().iter().any(|message| {
        message.content.iter().any(|part| matches!(part.content.as_ref(), Some(airicode::core::models::MessagePartContent::Text { text }) if text.contains("The file says")))
    }));
    Ok(())
}

#[tokio::test]
async fn fake_provider_executes_custom_shell_input_after_streaming_done() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let provider_id = ProviderId::new();
    let provider = Arc::new(FakeProvider::new(
        provider_id,
        [
            vec![
                ProviderEvent::CustomToolCallInputDelta {
                    index: 0,
                    id: Some("call-shell".into()),
                    name: Some("shell".into()),
                    input: "printf custom-".into(),
                },
                ProviderEvent::CustomToolCallInputDelta {
                    index: 0,
                    id: None,
                    name: None,
                    input: "output".into(),
                },
                ProviderEvent::CustomToolCallInputDone {
                    index: 0,
                    input: "printf custom-output".into(),
                },
                ProviderEvent::Finished {
                    reason: airicode::core::models::FinishReason::ToolCalls,
                },
            ],
            vec![
                ProviderEvent::OutputPart {
                    index: 0,
                    part: MessagePart::text("The command completed."),
                },
                ProviderEvent::Finished {
                    reason: airicode::core::models::FinishReason::Stop,
                },
            ],
        ],
    ));
    let fake_plugin = Arc::new(FakeProviderPlugin::new(provider));
    let core = airicode::core::CoreBuilder::new()
        .plugin(fake_plugin)
        .plugin(Arc::new(ToolShellPlugin::new()))
        .build()
        .await?;
    let group_id = SessionGroupId::new();
    let session = core.create_session(group_id);
    let mut events = session.subscribe();
    let engine = TurnEngine::new(core.registry(), session.operations.clone(), workdir);
    let request = TurnRequest::new(
        ProjectId::from_workdir(directory.path()),
        group_id,
        session.operations.session_id(),
        provider_id,
        "fake-model",
        "build",
        "run the command",
    );
    engine.run(request).await?;

    let state = session.operations.snapshot().await?;
    assert!(state.visible_messages().iter().any(|message| {
        message.content.iter().any(|part| {
            matches!(
                part.content.as_ref(),
                Some(airicode::core::models::MessagePartContent::ToolResult {
                    result: ToolOutput::Success { content }, ..
                }) if content.contains("custom-output")
            )
        })
    }));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            RuntimeEvent::ToolInputDelta { name, input, .. }
                if name == "shell" && input == "printf custom-"
        ))
    );
    Ok(())
}

#[tokio::test]
async fn failed_turn_emits_error_after_persisting_user_message() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let provider_id = ProviderId::new();
    let provider = Arc::new(FakeProvider::new(provider_id, std::iter::empty()));
    let fake_plugin = Arc::new(FakeProviderPlugin::new(provider));
    let core = airicode::core::CoreBuilder::new()
        .plugin(fake_plugin)
        .build()
        .await?;
    let group_id = SessionGroupId::new();
    let session = core.create_session(group_id);
    let mut events = session.subscribe();
    let engine = TurnEngine::new(core.registry(), session.operations.clone(), workdir);
    let request = TurnRequest::new(
        ProjectId::from_workdir(directory.path()),
        group_id,
        session.operations.session_id(),
        provider_id,
        "fake-model",
        "build",
        "hello",
    );

    let error = engine.run(request).await.expect_err("turn should fail");
    assert!(error.to_string().contains("no scripted response"));

    let mut failed = None;
    while let Ok(event) = events.try_recv() {
        if let RuntimeEvent::TurnFailed { error, .. } = event {
            failed = Some(error);
        }
    }
    assert_eq!(
        failed.as_deref(),
        Some("provider error: fake provider has no scripted response")
    );

    let state = session.operations.snapshot().await?;
    assert!(
        state
            .visible_messages()
            .iter()
            .any(|message| message.role == Role::User)
    );
    Ok(())
}

#[tokio::test]
async fn shell_cancellation_stops_a_running_command() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    let token = CancellationToken::new();
    let child_token = token.clone();
    let workdir_for_task = workdir.clone();
    let task = tokio::spawn(async move {
        workdir_for_task
            .execute(
                airicode::core::models::CommandSpec::new("sh", ["-c", "sleep 10"]),
                child_token,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .map_err(|error| airicode::Error::Workdir(error.to_string()))?
        .map_err(|error| airicode::Error::Workdir(error.to_string()))?;
    assert!(matches!(result, Err(airicode::Error::Cancelled)));
    Ok(())
}
