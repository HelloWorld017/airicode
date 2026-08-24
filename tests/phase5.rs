use std::{path::Path, sync::Arc, time::Duration};

use airicode::{
    core::{
        models::{
            ProjectId, ProviderEvent, ProviderId, Role, SessionGroupId, ToolContext, ToolOutput,
            TurnId,
        },
        operations::new_session,
        runtime::{TurnEngine, TurnRequest},
        workdir::{NativeWorkdir, Workdir},
        Tool,
    },
    plugins::{
        FakeProvider, FakeProviderPlugin, ToolRead, ToolReadPlugin, ToolShell, ToolShellPlugin,
    },
    Result,
};
use serde_json::json;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn native_workdir_enforces_root_and_executes_commands() -> Result<()> {
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

    assert!(workdir.read(Path::new("../outside")).await.is_err());
    assert!(workdir.read(Path::new("/etc/passwd")).await.is_err());
    Ok(())
}

#[tokio::test]
async fn read_and_shell_tools_use_the_shared_workdir_contract() -> Result<()> {
    let directory = tempdir()?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);
    workdir
        .write(Path::new("main.rs"), b"fn main() {}\n")
        .await?;
    let session = new_session(
        airicode::core::models::SessionId::new(),
        SessionGroupId::new(),
    );
    let context = ToolContext {
        project_id: ProjectId::new(),
        session_group_id: SessionGroupId::new(),
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
    assert!(content.starts_with("1:"));
    assert!(content.contains("|fn main() {}"));

    let shell = ToolShell::new();
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
async fn fake_provider_completes_a_read_tool_turn_on_a_real_project() -> Result<()> {
    let directory = tempdir()?;
    std::fs::write(directory.path().join("hello.txt"), "hello from project\n")?;
    let workdir: Arc<dyn Workdir> = Arc::new(NativeWorkdir::new(directory.path())?);

    let provider_id = ProviderId::new();
    let provider = Arc::new(FakeProvider::new(
        provider_id,
        [
            vec![
                ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call-1".into()),
                    name: Some("read".into()),
                    arguments: "{\"path\":\"hello.txt\"}".into(),
                },
                ProviderEvent::Finished {
                    reason: airicode::core::models::FinishReason::ToolCalls,
                },
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "The file says hello from project.".into(),
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
        .plugin(Arc::new(ToolReadPlugin::new(Arc::new(ToolRead::new()))))
        .plugin(Arc::new(ToolShellPlugin::new(Arc::new(ToolShell::new()))))
        .build()
        .await?;
    let session = core.create_session(SessionGroupId::new());
    let engine = TurnEngine::new(core.registry(), session.operations.clone(), workdir);
    let request = TurnRequest::new(
        ProjectId::new(),
        SessionGroupId::new(),
        session.operations.session_id(),
        provider_id,
        "fake-model",
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
        message.content.iter().any(|part| matches!(part, airicode::core::models::MessagePart::ToolResult { result: ToolOutput::Success { content }, .. } if content.contains("hello from project")))
    }));
    assert!(state.visible_messages().iter().any(|message| {
        message.content.iter().any(|part| matches!(part, airicode::core::models::MessagePart::Text { text } if text.contains("The file says")))
    }));
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
