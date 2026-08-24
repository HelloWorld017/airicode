use std::{io, path::PathBuf};

use thiserror::Error;

use super::{
    CommandId, HookId, MessageId, PluginId, ProviderId, SessionStoreFactoryId, ToolId,
    WorkdirLayerId,
};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration file {path} could not be read: {source}")]
    ConfigIo { path: PathBuf, source: io::Error },
    #[error("configuration is invalid: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("provider {0} is already registered")]
    DuplicateProvider(ProviderId),
    #[error("tool {0} is already registered")]
    DuplicateTool(ToolId),
    #[error("command {0} is already registered")]
    DuplicateCommand(CommandId),
    #[error("command name {0} is already registered")]
    DuplicateCommandName(String),
    #[error("invalid command descriptor: {0}")]
    InvalidCommandDescriptor(String),
    #[error("command {0} is not registered")]
    CommandNotFound(String),
    #[error("plugin {0} is already registered")]
    DuplicatePlugin(PluginId),
    #[error("hook {0} is already registered for this hook type")]
    DuplicateHook(HookId),
    #[error("session store factory {0} is already registered")]
    DuplicateSessionStoreFactory(SessionStoreFactoryId),
    #[error("workdir layer {0} is already registered")]
    DuplicateWorkdirLayer(WorkdirLayerId),
    #[error("provider {0} is not registered")]
    ProviderNotFound(ProviderId),
    #[error("session already has an active turn")]
    SessionBusy,
    #[error("history revision changed (expected {expected}, actual {actual})")]
    HistoryRevisionMismatch { expected: u64, actual: u64 },
    #[error("message {0} is not in session history")]
    MessageNotFound(MessageId),
    #[error("invalid message range")]
    InvalidMessageRange,
    #[error("session is closed")]
    SessionClosed,
    #[error("operation was cancelled")]
    Cancelled,
    #[error("hook cancelled the operation: {0}")]
    HookCancelled(String),
    #[error("provider failed: {0}")]
    Provider(String),
    #[error("tool failed: {0}")]
    Tool(String),
    #[error("workdir failed: {0}")]
    Workdir(String),
    #[error("session store failed: {0}")]
    Store(String),
    #[error("plugin failed: {0}")]
    Plugin(String),
    #[error("plugin registrar is closed")]
    PluginRegistrarClosed,
    #[error("internal channel closed")]
    ChannelClosed,
}
