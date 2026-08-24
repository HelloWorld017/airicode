use std::sync::Arc;

use airicode::{
    core::{
        BeforeHookResult, Core, HookContext, MessageId, NativeWorkdir, ProjectId, SessionId, Tool,
        ToolCallDraft, ToolCallId, ToolContext, ToolExecutionContext, ToolId, TurnId, Workdir,
    },
    plugins::{
        approval_plugin, grep_plugin, patch_plugin, read_plugin, shell_plugin, ApprovalPolicy,
    },
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
        (read_plugin(), "builtin.read"),
        (shell_plugin(), "builtin.shell"),
    ] {
        let core = Core::new().with_plugin(plugin).build().await.unwrap();
        assert_eq!(core.tools().ids(), vec![ToolId::new(expected)]);
    }

    let core = Core::new().build().await.unwrap();
    for id in [
        "builtin.grep",
        "builtin.patch",
        "builtin.read",
        "builtin.shell",
    ] {
        assert!(core.tools().get(&ToolId::new(id)).is_none());
    }
}

async fn read_file(workdir: NativeWorkdir, path: &str, range: Option<(usize, usize)>) -> String {
    let mut input = json!({ "path": path });
    if let Some((start, end)) = range {
        input["range"] = json!({ "start": start, "end": end });
    }
    tool(read_plugin(), "builtin.read")
        .await
        .execute(input, context(workdir))
        .await
        .unwrap()
        .content
}

fn marker(record: &str) -> &str {
    record.split_once('|').unwrap().0
}

#[tokio::test]
async fn read_returns_exact_ranges_and_a_stable_short_hash() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    workdir
        .write("file.txt".as_ref(), b"hello\nsecond\nthird")
        .await
        .unwrap();

    let whole = read_file(workdir.clone(), "file.txt", None).await;
    assert_eq!(whole.lines().next().unwrap(), "1:92|hello");
    let range = read_file(workdir, "file.txt", Some((2, 3))).await;
    assert_eq!(
        range
            .lines()
            .map(|line| line.split_once('|').unwrap().1)
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    assert!(range.starts_with("2:"));
}

#[tokio::test]
async fn read_rejects_oversized_line_and_byte_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    workdir
        .write("many.txt".as_ref(), "x\n".repeat(2_001).as_bytes())
        .await
        .unwrap();
    let error = tool(read_plugin(), "builtin.read")
        .await
        .execute(json!({ "path": "many.txt" }), context(workdir.clone()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("use range"));

    workdir
        .write("wide.txt".as_ref(), "x".repeat(200_001).as_bytes())
        .await
        .unwrap();
    let error = tool(read_plugin(), "builtin.read")
        .await
        .execute(
            json!({ "path": "wide.txt", "range": { "start": 1, "end": 1 } }),
            context(workdir),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("200 KB"));
}

#[tokio::test]
async fn hashline_patch_preserves_crlf_and_final_newline() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    workdir
        .write("crlf.txt".as_ref(), b"first\r\nsecond\r\n")
        .await
        .unwrap();
    let read = read_file(workdir.clone(), "crlf.txt", None).await;
    assert_eq!(
        read.lines()
            .map(|line| line.split_once('|').unwrap().1)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    let second = marker(read.lines().nth(1).unwrap());
    tool(patch_plugin(), "builtin.patch")
        .await
        .execute(
            json!({ "operations": [{
                "op": "update", "path": "crlf.txt",
                "edits": [{ "hashline": second, "new_content": "SECOND" }]
            }] }),
            context(workdir.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        workdir.read("crlf.txt".as_ref()).await.unwrap(),
        b"first\r\nSECOND\r\n"
    );
}

#[tokio::test]
async fn hashline_patch_replaces_deletes_blanks_and_expands_lines() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    workdir
        .write("file.txt".as_ref(), b"one\ntwo\nthree\nfour")
        .await
        .unwrap();
    let read = read_file(workdir.clone(), "file.txt", None).await;
    let markers = read.lines().map(marker).collect::<Vec<_>>();
    let output = tool(patch_plugin(), "builtin.patch")
        .await
        .execute(
            json!({ "operations": [{
                "op": "update", "path": "file.txt", "edits": [
                    { "hashline": markers[0], "new_content": "ONE" },
                    { "hashline": markers[1], "new_content": null },
                    { "hashline": markers[2], "new_content": "" },
                    { "hashline": markers[3], "new_content": "four-a\nfour-b" }
                ]
            }] }),
            context(workdir.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        workdir.read("file.txt".as_ref()).await.unwrap(),
        b"ONE\n\nfour-a\nfour-b"
    );
    let response: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(response["changed"], json!(["file.txt"]));
    assert_eq!(
        response["hashlines"]["file.txt"].as_array().unwrap().len(),
        4
    );
}

#[tokio::test]
async fn stale_hashline_rejects_a_multi_file_patch_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let workdir = NativeWorkdir::new(directory.path()).unwrap();
    workdir.write("a.txt".as_ref(), b"a\n").await.unwrap();
    workdir.write("b.txt".as_ref(), b"b\n").await.unwrap();
    let a = read_file(workdir.clone(), "a.txt", None).await;
    let b = read_file(workdir.clone(), "b.txt", None).await;
    workdir.write("b.txt".as_ref(), b"changed\n").await.unwrap();

    let result = tool(patch_plugin(), "builtin.patch")
        .await
        .execute(
            json!({ "operations": [
                { "op": "update", "path": "a.txt", "edits": [{ "hashline": marker(&a), "new_content": "A" }] },
                { "op": "update", "path": "b.txt", "edits": [{ "hashline": marker(&b), "new_content": "B" }] }
            ] }),
            context(workdir.clone()),
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("stale hashline"));
    assert_eq!(workdir.read("a.txt".as_ref()).await.unwrap(), b"a\n");
    assert_eq!(workdir.read("b.txt".as_ref()).await.unwrap(), b"changed\n");
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
            assistant_message_id: MessageId::new(),
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
        assistant_message_id: MessageId::new(),
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
