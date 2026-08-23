use std::sync::Arc;

use airicode::{
    core::{
        BeforeHookResult, Core, HookContext, NativeWorkdir, ProjectId, SessionId, Tool,
        ToolCallDraft, ToolCallId, ToolContext, ToolExecutionContext, ToolId, TurnId, Workdir,
    },
    plugins::{approval_plugin, grep_plugin, patch_plugin, shell_plugin, ApprovalPolicy},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn context(workdir: NativeWorkdir) -> ToolContext {
    ToolContext {
        project_id: ProjectId::new(),
        session_id: SessionId::new(),
        turn_id: TurnId::new(),
        workdir: Arc::new(workdir),
        cancellation: CancellationToken::new(),
    }
}

async fn tool(plugin: Arc<dyn airicode::core::Plugin>, id: &str) -> Arc<dyn Tool> {
    Core::new()
        .with_plugin(plugin)
        .build()
        .await
        .unwrap()
        .tools()
        .get(&ToolId::new(id))
        .unwrap()
}

#[tokio::test]
async fn tool_plugins_register_only_their_own_tool() {
    for (plugin, expected) in [
        (grep_plugin(), "builtin.grep"),
        (patch_plugin(), "builtin.patch"),
        (shell_plugin(), "builtin.shell"),
    ] {
        let core = Core::new().with_plugin(plugin).build().await.unwrap();
        assert_eq!(core.tools().ids(), vec![ToolId::new(expected)]);
    }

    let core = Core::new().build().await.unwrap();
    for id in ["builtin.grep", "builtin.patch", "builtin.shell"] {
        assert!(core.tools().get(&ToolId::new(id)).is_none());
    }
}

#[tokio::test]
async fn approval_plugin_controls_all_tool_calls_and_is_optional() {
    let denied = Core::new()
        .with_plugin(grep_plugin())
        .with_plugin(patch_plugin())
        .with_plugin(shell_plugin())
        .with_plugin(approval_plugin(ApprovalPolicy::Deny))
        .build()
        .await
        .unwrap();
    let hook_context = HookContext {
        project_id: ProjectId::new(),
        session_id: SessionId::new(),
    };
    for name in ["grep", "patch", "shell"] {
        let mut execution = ToolExecutionContext {
            hook_context: hook_context.clone(),
            turn_id: TurnId::new(),
            workdir: Arc::new(airicode::testkit::StubWorkdir::new("/tmp")),
            call: ToolCallDraft {
                id: ToolCallId::new(format!("{name}-call")),
                name: name.into(),
                arguments: json!({}),
            },
        };
        assert!(matches!(
            denied
                .hooks()
                .before_tool_execution(&mut execution)
                .await
                .unwrap(),
            BeforeHookResult::Cancel { .. }
        ));
    }

    let allowed = Core::new()
        .with_plugin(grep_plugin())
        .with_plugin(patch_plugin())
        .with_plugin(shell_plugin())
        .build()
        .await
        .unwrap();
    let mut execution = ToolExecutionContext {
        hook_context,
        turn_id: TurnId::new(),
        workdir: Arc::new(airicode::testkit::StubWorkdir::new("/tmp")),
        call: ToolCallDraft {
            id: ToolCallId::new("unblocked-call"),
            name: "shell".into(),
            arguments: json!({}),
        },
    };
    assert_eq!(
        allowed
            .hooks()
            .before_tool_execution(&mut execution)
            .await
            .unwrap(),
        BeforeHookResult::Continue
    );

    let explicitly_allowed = Core::new()
        .with_plugin(approval_plugin(ApprovalPolicy::Allow))
        .build()
        .await
        .unwrap();
    assert_eq!(
        explicitly_allowed
            .hooks()
            .before_tool_execution(&mut execution)
            .await
            .unwrap(),
        BeforeHookResult::Continue
    );
}

#[tokio::test]
async fn patch_and_grep_project_files() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    let output = tool(patch_plugin(), "builtin.patch")
        .await
        .execute(
            json!({
                "operations": [
                    { "op": "add", "path": "src/example.txt", "content": "first\nneedle here\n" }
                ]
            }),
            context(workdir.clone()),
        )
        .await
        .unwrap();
    assert!(!output.is_error);

    let output = tool(grep_plugin(), "builtin.grep")
        .await
        .execute(
            json!({ "query": "needle", "path": "src" }),
            context(workdir),
        )
        .await
        .unwrap();
    assert!(output.content.contains("example.txt"));
    assert!(output.content.contains("\"line\":2"));

    let bounded_workdir = NativeWorkdir::new(directory.path()).unwrap();
    bounded_workdir
        .write(
            "src/long.txt".as_ref(),
            format!("needle {}", "x".repeat(10_000)).as_bytes(),
        )
        .await
        .unwrap();
    let bounded = tool(grep_plugin(), "builtin.grep")
        .await
        .execute(
            json!({ "query": "needle", "path": "src", "max_output_bytes": 1024 }),
            context(bounded_workdir),
        )
        .await
        .unwrap();
    assert!(bounded.content.len() <= 1024);
}

#[tokio::test]
async fn patch_checks_expected_text_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    workdir
        .write("file.txt".as_ref(), b"original")
        .await
        .unwrap();

    let result = tool(patch_plugin(), "builtin.patch")
        .await
        .execute(
            json!({
                "operations": [
                    { "op": "update", "path": "file.txt", "content": "changed", "expected_old_text": "other" }
                ]
            }),
            context(workdir.clone()),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        workdir.read("file.txt".as_ref()).await.unwrap(),
        b"original"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn shell_reports_nonzero_exit_as_output() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    let output = tool(shell_plugin(), "builtin.shell")
        .await
        .execute(
            json!({ "program": "sh", "args": ["-c", "printf failure >&2; exit 7"] }),
            context(workdir),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert!(output.content.contains("\"status\":7"));
    assert!(output.content.contains("failure"));
}
