use thiserror::Error as ThisError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("registry error: {0}")]
    Registry(String),
    #[error("plugin error: {0}")]
    Plugin(String),
    #[error("session error: {0}")]
    Session(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("command error: {0}")]
    Command(String),
    #[error("workdir error: {0}")]
    Workdir(String),
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
