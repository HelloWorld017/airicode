use serde::{Deserialize, Serialize};

use super::id::{ContextPartId, MessageId};
use super::message::Metadata;
use crate::utils::TimeSeq;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContextContributionPosition {
    Start,
    Timeline(TimeSeq),
    End,
}

pub type ContextPosition = ContextContributionPosition;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContextPriority {
    Persistent,
    High,
    Low,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ContextSource {
    Message(MessageId),
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextPart {
    pub id: ContextPartId,
    pub priority: ContextPriority,
    pub source: ContextSource,
    pub created_at: TimeSeq,
    pub metadata: Metadata,
    pub invalidated: bool,
}

#[derive(Clone, Debug)]
pub struct ContextContribution {
    pub priority: ContextPriority,
    pub position: ContextContributionPosition,
    pub text: String,
    pub metadata: Metadata,
}
