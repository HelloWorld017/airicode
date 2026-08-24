pub mod config;
#[allow(clippy::module_inception)]
pub mod core;
pub mod error;
pub mod hooks;
pub mod models;
pub mod operations;
pub mod registry;
pub mod runtime;
pub mod session;
pub mod shell;
pub mod workdir;

pub use core::{Core, CoreBuilder};
pub use error::{Error, Result};
pub use hooks::{
    AfterToolExecutionHook, BeforeMessageHook, BeforeProviderRequestHook, BeforeToolExecutionHook,
    ContextContributionHook, HookRegistry, RegisterHook,
};
pub use models::*;
pub use operations::{Operations, SessionHandle};
pub use registry::{PluginRegistryScope, RegistrationHandle, Registry};
pub use runtime::{TurnEngine, TurnRequest};
pub use shell::ShellActionHandler;
