use std::{path::PathBuf, sync::Arc};

use airicode::{
    core::{project_from_path, CoreBuilder, ProviderId, SessionGroupId},
    plugins::{
        InstructionBasePlugin, JsonlSessionStore, OpenAiProvider, OpenAiProviderPlugin,
        PersistencePlugin, ToolGrepPlugin, ToolPatchPlugin, ToolQuestion, ToolQuestionPlugin,
        ToolReadPlugin, ToolShellPlugin, ToolTodoPlugin, ToolWebfetch, ToolWebfetchPlugin,
    },
    ui::terminal::TerminalApp,
};

#[tokio::main]
async fn main() -> airicode::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let root = std::fs::canonicalize(root)?;

    let project = project_from_path(root.clone());
    let workdir = Arc::new(airicode::core::workdir::NativeWorkdir::new(root.clone())?);
    let store = Arc::new(JsonlSessionStore::new(root)?);
    let provider_id = ProviderId::new();
    let mut builder = CoreBuilder::new()
        .plugin(Arc::new(PersistencePlugin::new(store.clone())))
        .plugin(Arc::new(InstructionBasePlugin::new()))
        .plugin(Arc::new(ToolReadPlugin::new()))
        .plugin(Arc::new(ToolShellPlugin::new()))
        .plugin(Arc::new(ToolPatchPlugin::new()))
        .plugin(Arc::new(ToolGrepPlugin::new()))
        .plugin(Arc::new(ToolTodoPlugin::new()))
        .plugin(Arc::new(ToolQuestionPlugin::new(Arc::new(
            ToolQuestion::new(),
        ))))
        .plugin(Arc::new(ToolWebfetchPlugin::new(Arc::new(
            ToolWebfetch::new(),
        ))));
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        builder = builder.plugin(Arc::new(OpenAiProviderPlugin::new(Arc::new(
            OpenAiProvider::from_env(provider_id)?,
        ))));
    }
    let core = builder.build().await?;
    let (session, group_id) = match store.discover().await?.into_iter().next() {
        Some(session_id) => {
            let group_id = session_id.group_id();
            (core.load_session(session_id, group_id).await?, group_id)
        }
        None => {
            let group_id = SessionGroupId::new();
            (core.create_session(group_id), group_id)
        }
    };
    TerminalApp::new(
        session,
        core.registry(),
        workdir,
        project.id,
        group_id,
        provider_id,
        std::env::var("AIRICODE_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
    )
    .run()
    .await
}
