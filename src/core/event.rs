use serde::{Deserialize, Serialize};

use super::{CommandId, Message, PluginId, ProjectId, ProviderEvent, SessionId, TurnId};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    ProjectOpened {
        project_id: ProjectId,
    },
    SessionOpened {
        session_id: SessionId,
    },
    TurnStarted {
        session_id: SessionId,
        turn_id: TurnId,
    },
    MessageAdded {
        session_id: SessionId,
        message: Message,
    },
    ProviderEvent {
        session_id: SessionId,
        turn_id: TurnId,
        event: ProviderEvent,
    },
    TurnCompleted {
        session_id: SessionId,
        turn_id: TurnId,
    },
    TurnCancelled {
        session_id: SessionId,
        turn_id: TurnId,
    },
    TurnFailed {
        session_id: SessionId,
        turn_id: TurnId,
        error: String,
    },
    CommandStarted {
        session_id: SessionId,
        command_id: CommandId,
    },
    CommandCompleted {
        session_id: SessionId,
        command_id: CommandId,
    },
    CommandCancelled {
        session_id: SessionId,
        command_id: CommandId,
    },
    CommandFailed {
        session_id: SessionId,
        command_id: CommandId,
        error: String,
    },
    HistoryReplaced {
        session_id: SessionId,
        revision: u64,
        messages: Vec<Message>,
    },
    HookFailed {
        plugin_id: PluginId,
        error: String,
    },
    FeatureEvent {
        plugin_id: PluginId,
        name: String,
        payload: serde_json::Value,
    },
}
