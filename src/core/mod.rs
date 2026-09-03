pub mod config;
#[allow(clippy::module_inception)]
pub mod core;
pub mod error;
pub mod hooks;
pub mod models;
pub mod operations;
pub mod persistence;
pub mod registry;
pub mod runtime;
pub mod session;
pub mod shell;
pub mod workdir;

pub use core::{Core, CoreBuilder, project_from_path};
pub use error::{Error, Result};
pub use hooks::{
    AfterToolExecutionHook, BeforeMessageHook, BeforeProviderRequestHook, BeforeToolExecutionHook,
    BuildFileContextHook, BuildFileContextHookContext, ConfigReadContext, ConfigReadHook,
    ContextContributionHook, HookRegistry, OpenProjectContext, OpenProjectHook, RegisterHook,
};
pub use models::*;
pub use operations::{Operations, SessionHandle};
pub use persistence::SessionStore;
pub use registry::{PluginRegistryScope, RegistrationHandle, Registry};
pub use runtime::{SessionRuntime, SessionRuntimeDeps, TurnEngine, TurnRequest};
pub use shell::ShellActionHandler;
