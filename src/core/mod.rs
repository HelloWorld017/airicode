pub mod command;
pub mod config;
#[allow(clippy::module_inception)]
pub mod core;
pub mod error;
pub mod events;
pub mod hooks;
pub mod models;
pub mod operations;
pub mod plugin;
pub mod provider;
pub mod registry;
pub mod runtime;
pub mod tool;
pub mod workdir;

pub use command::Command;
pub use core::{Core, CoreBuilder};
pub use error::{Error, Result};
pub use hooks::{
    AfterToolExecutionHook, BeforeMessageHook, BeforeProviderRequestHook, BeforeToolExecutionHook,
    ContextContributionHook, HookRegistry, RegisterHook,
};
pub use models::*;
pub use operations::{Operations, SessionHandle};
pub use plugin::Plugin;
pub use provider::Provider;
pub use registry::{PluginRegistryScope, RegistrationHandle, Registry};
pub use runtime::{TurnEngine, TurnRequest};
pub use tool::Tool;
