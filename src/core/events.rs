use serde::{Deserialize, Serialize};

use super::models::{FinishReason, Message, ToolCallId, ToolOutput, TurnId, Usage};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RuntimeEvent {
    TurnStarted {
        turn_id: TurnId,
    },
    ProviderStreamDelta {
        turn_id: TurnId,
        text: String,
        reasoning: bool,
    },
    ProviderUsageUpdated {
        turn_id: TurnId,
        usage: Usage,
    },
    ProviderRoundFinished {
        turn_id: TurnId,
        reason: FinishReason,
    },
    AssistantMessageCommitted {
        turn_id: TurnId,
        message: Message,
    },
    ToolExecutionStarted {
        turn_id: TurnId,
        call_id: ToolCallId,
        name: String,
    },
    ToolExecutionFinished {
        turn_id: TurnId,
        call_id: ToolCallId,
        output: ToolOutput,
    },
    TurnCompleted {
        turn_id: TurnId,
    },
    TurnCancelled {
        turn_id: TurnId,
    },
    SessionSnapshotChanged,
    RegistryChanged {
        revision: u64,
    },
}
