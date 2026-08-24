use std::sync::Arc;

use airicode::core::{
    models::{
        ContextPriority, ContextSource, Message, MessagePart, Role, SessionGroupId, SessionId,
        SessionMutation, SessionState, ShellAction, ShellActionContext, ShellActionDefinition,
        ShellActionId, ShellActionInput, ToolDefinition, ToolId, ToolOutput,
    },
    operations::new_session,
    registry::Registry,
    Tool,
};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn reducer_replays_atomic_conversation_and_invalidation() -> airicode::Result<()> {
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let message = Message::text(Role::User, "hello", "build", None);
    let part = airicode::core::models::ContextPart {
        id: airicode::core::models::ContextPartId::new(),
        priority: ContextPriority::High,
        source: ContextSource::Message(message.id),
        created_at: airicode::utils::TimeSeq::new(),
        metadata: Default::default(),
        invalidated: false,
    };
    let first = airicode::core::models::SessionCommit::new(
        1,
        vec![
            SessionMutation::MessageAdded {
                message: message.clone(),
            },
            SessionMutation::ContextPartAdded { part: part.clone() },
        ],
    );
    let second = airicode::core::models::SessionCommit::new(
        2,
        vec![SessionMutation::MessageInvalidated {
            message_id: message.id,
        }],
    );
    let encoded = serde_json::to_vec(&(first.clone(), second.clone()))?;
    let (decoded_first, decoded_second): (
        airicode::core::models::SessionCommit,
        airicode::core::models::SessionCommit,
    ) = serde_json::from_slice(&encoded)?;
    let mut state = SessionState::new(session_id, group_id);
    state.apply(&decoded_first)?;
    state.apply(&decoded_second)?;
    assert!(state.visible_messages().is_empty());
    assert_eq!(state.active_context().len(), 1);
    assert_eq!(state.last_sequence, 2);
    Ok(())
}

#[test]
fn context_is_sorted_by_time_sequence_not_priority() -> airicode::Result<()> {
    let group_id = SessionGroupId::new();
    let session_id = SessionId::new(group_id);
    let older_message = Message::text(Role::User, "older", "build", None);
    let newer_message = Message::text(Role::User, "newer", "build", None);
    let older_message_id = older_message.id;
    let newer_message_id = newer_message.id;
    let older_part = airicode::core::models::ContextPart {
        id: airicode::core::models::ContextPartId::new(),
        priority: ContextPriority::Low,
        source: ContextSource::Message(older_message_id),
        created_at: airicode::utils::TimeSeq::from_parts(10, 0),
        metadata: Default::default(),
        invalidated: false,
    };
    let newer_part = airicode::core::models::ContextPart {
        id: airicode::core::models::ContextPartId::new(),
        priority: ContextPriority::Persistent,
        source: ContextSource::Message(newer_message_id),
        created_at: airicode::utils::TimeSeq::from_parts(20, 0),
        metadata: Default::default(),
        invalidated: false,
    };
    let commit = airicode::core::models::SessionCommit::new(
        1,
        vec![
            SessionMutation::MessageAdded {
                message: older_message,
            },
            SessionMutation::MessageAdded {
                message: newer_message,
            },
            SessionMutation::ContextPartAdded { part: newer_part },
            SessionMutation::ContextPartAdded { part: older_part },
        ],
    );
    let mut state = SessionState::new(session_id, group_id);
    state.apply(&commit)?;

    let context = state.active_context();
    assert_eq!(context[0].source, ContextSource::Message(older_message_id));
    assert_eq!(context[1].source, ContextSource::Message(newer_message_id));
    Ok(())
}

#[tokio::test]
async fn actor_commits_message_and_context_as_one_operation() -> airicode::Result<()> {
    let group_id = SessionGroupId::new();
    let session = new_session(SessionId::new(group_id), group_id);
    let message = Message::text(Role::User, "atomic", "build", None);
    let (message_id, context_id) = session
        .operations
        .add_conversation_message(message, ContextPriority::High)
        .await?;
    let state = session.operations.snapshot().await?;
    assert!(state.messages.contains_key(&message_id));
    assert!(state.context.contains_key(&context_id));
    assert_eq!(state.last_sequence, 1);
    Ok(())
}

struct TestTool {
    id: ToolId,
    name: &'static str,
}

#[async_trait]
impl Tool for TestTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "test".into(),
            input_schema: json!({ "type": "object" }),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: airicode::core::models::ToolContext,
    ) -> airicode::Result<ToolOutput> {
        Ok(ToolOutput::Success {
            content: "ok".into(),
        })
    }
}

#[tokio::test]
async fn registry_snapshots_tools_and_supports_dynamic_removal() -> airicode::Result<()> {
    let registry = Registry::new();
    let registry_scope = registry.scope(airicode::core::models::PluginId::new());
    let first = Arc::new(TestTool {
        id: ToolId::new(),
        name: "first",
    });
    let second = Arc::new(TestTool {
        id: ToolId::new(),
        name: "second",
    });
    let first_handle = registry_scope.register_tool(first, 0)?;
    let second_handle = registry_scope.register_tool(second, 10)?;
    assert_eq!(registry.tools().len(), 2);
    assert_eq!(registry.tools()[0].definition().name, "second");
    second_handle.remove().await?;
    assert_eq!(registry.tools().len(), 1);
    first_handle.remove().await?;
    assert!(registry.tools().is_empty());
    Ok(())
}

struct TestShellAction {
    id: ShellActionId,
    name: &'static str,
}

#[async_trait]
impl ShellAction for TestShellAction {
    fn id(&self) -> ShellActionId {
        self.id
    }

    fn definition(&self) -> ShellActionDefinition {
        ShellActionDefinition::new(
            self.name,
            "test shell action",
            json!({ "arguments": { "type": "string", "remainder": true } }),
        )
    }

    async fn execute(
        &self,
        input: ShellActionInput,
        _context: ShellActionContext,
    ) -> airicode::Result<String> {
        Ok(input.arguments.join("/"))
    }
}

#[tokio::test]
async fn registry_registers_and_dispatches_shell_actions() -> airicode::Result<()> {
    let registry = Registry::new();
    let scope = registry.scope(airicode::core::models::PluginId::new());
    let action = Arc::new(TestShellAction {
        id: ShellActionId::new(),
        name: "inspect",
    });
    let handle = scope.register_shell_action(action, 10)?;
    assert_eq!(registry.shell_actions().len(), 1);
    assert_eq!(
        registry
            .shell_action_by_name("inspect")
            .expect("registered action")
            .definition()
            .scheme,
        json!({ "arguments": { "type": "string", "remainder": true } })
    );

    let directory = tempfile::tempdir()?;
    let result = airicode::core::ShellActionHandler::new(registry.clone())
        .handle_args(
            ["inspect", "one", "two"],
            ShellActionContext {
                project_id: airicode::core::models::ProjectId::from_workdir(directory.path()),
                workdir: Arc::new(airicode::core::workdir::NativeWorkdir::new(PathBuf::from(
                    directory.path(),
                ))?),
                cancellation: tokio_util::sync::CancellationToken::new(),
            },
        )
        .await?;
    assert_eq!(result, "one/two");

    handle.remove().await?;
    assert!(registry.shell_action_by_name("inspect").is_none());
    Ok(())
}

#[test]
fn reducer_rejects_sequence_gaps() {
    let group_id = SessionGroupId::new();
    let mut state = SessionState::new(SessionId::new(group_id), group_id);
    let commit = airicode::core::models::SessionCommit::new(
        2,
        vec![SessionMutation::DurableUIStateUpdated {
            state: Default::default(),
        }],
    );
    assert!(state.apply(&commit).is_err());
}

#[test]
fn reducer_does_not_apply_a_partial_invalid_commit() {
    let group_id = SessionGroupId::new();
    let mut state = SessionState::new(SessionId::new(group_id), group_id);
    let message = Message::text(Role::User, "must not persist", "build", None);
    let commit = airicode::core::models::SessionCommit::new(
        1,
        vec![
            SessionMutation::MessageAdded {
                message: message.clone(),
            },
            SessionMutation::ContextPartInvalidated {
                context_part_id: airicode::core::models::ContextPartId::new(),
            },
        ],
    );
    assert!(state.apply(&commit).is_err());
    assert!(state.messages.is_empty());
    assert_eq!(state.last_sequence, 0);
}

#[test]
fn reducer_rejects_empty_message_parts() {
    let group_id = SessionGroupId::new();
    let mut state = SessionState::new(SessionId::new(group_id), group_id);
    let message = Message {
        content: vec![MessagePart {
            content: None,
            provider_data: None,
        }],
        ..Message::text(Role::User, "invalid", "build", None)
    };
    let commit = airicode::core::models::SessionCommit::new(
        1,
        vec![SessionMutation::MessageAdded { message }],
    );
    assert!(state.apply(&commit).is_err());
    assert!(state.messages.is_empty());
}
