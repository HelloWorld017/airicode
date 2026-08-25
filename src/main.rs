use std::{path::PathBuf, sync::Arc};

use airicode::{
    core::{project_from_path, CoreBuilder, SessionGroupId},
    plugins::{
        InstructionBasePlugin, OpenAiProviderPlugin, PersistencePlugin, ToolGrepPlugin,
        ToolPatchPlugin, ToolQuestionPlugin, ToolReadPlugin, ToolShellPlugin, ToolTodoPlugin,
        ToolWebfetchPlugin,
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
    let persistence = Arc::new(PersistencePlugin::new());
    let builder = CoreBuilder::new()
        .project(project.clone())
        .plugin(persistence.clone())
        .plugin(Arc::new(InstructionBasePlugin::new()))
        .plugin(Arc::new(ToolReadPlugin::new()))
        .plugin(Arc::new(ToolShellPlugin::new()))
        .plugin(Arc::new(ToolPatchPlugin::new()))
        .plugin(Arc::new(ToolGrepPlugin::new()))
        .plugin(Arc::new(ToolTodoPlugin::new()))
        .plugin(Arc::new(ToolQuestionPlugin::new()))
        .plugin(Arc::new(ToolWebfetchPlugin::new()))
        .plugin(Arc::new(OpenAiProviderPlugin::new()));

    let core = builder.build().await?;
    let provider_id = match core.registry().providers().iter().next() {
        Some(provider) => provider.id(),
        None => {
            return Err(airicode::Error::Config(
                "Provider is not configured; setup provider before starting airicode".into(),
            ));
        }
    };

    let (session, group_id) = match persistence.discover().await?.into_iter().next() {
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
        std::env::var("AIRICODE_MODEL").unwrap_or_else(|_| "gpt-5.6-luna".into()),
    )
    .run()
    .await
}
