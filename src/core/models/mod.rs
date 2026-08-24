pub mod command;
pub mod context;
pub mod events;
pub mod id;
pub mod message;
pub mod note;
pub mod plugin;
pub mod project;
pub mod provider;
pub mod session;
pub mod session_group;
pub mod tool;
pub mod ui_state;
pub mod workdir;

pub use command::*;
pub use context::*;
pub use events::*;
pub use id::*;
pub use message::*;
pub use note::*;
pub use plugin::*;
pub use project::*;
pub use provider::*;
pub use session::*;
pub use session_group::*;
pub use tool::*;
pub use ui_state::*;
pub use workdir::*;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
