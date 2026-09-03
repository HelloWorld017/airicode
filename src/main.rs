use std::{path::PathBuf, sync::Arc};

use airicode::{
    core::{CoreBuilder, SessionGroupId, project_from_path},
    plugins::{
        InstructionBasePlugin, OpenAiProviderPlugin, PersistencePlugin, ToolDeletePlugin,
        ToolFindFilePlugin, ToolGrepPlugin, ToolPatchApplyPatchPlugin, ToolPatchHashlinePlugin,
        ToolPatchPlugin, ToolQuestionPlugin, ToolReadPlugin, ToolRenamePlugin, ToolShellPlugin,
        ToolTodoPlugin, ToolWebfetchPlugin, ToolWritePlugin,
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
    let persistence = Arc::new(PersistencePlugin::new());
    let builder = CoreBuilder::new()
        .project(project.clone())
        .plugin(persistence.clone())
        .plugin(Arc::new(InstructionBasePlugin::new()))
        .plugin(Arc::new(ToolReadPlugin::new()))
        .plugin(Arc::new(ToolShellPlugin::new()))
        .plugin(Arc::new(ToolPatchPlugin::new()))
        .plugin(Arc::new(ToolPatchHashlinePlugin::new()))
        .plugin(Arc::new(ToolPatchApplyPatchPlugin::new()))
        .plugin(Arc::new(ToolWritePlugin::new()))
        .plugin(Arc::new(ToolRenamePlugin::new()))
        .plugin(Arc::new(ToolDeletePlugin::new()))
        .plugin(Arc::new(ToolGrepPlugin::new()))
        .plugin(Arc::new(ToolFindFilePlugin::new()))
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

    let session = match persistence.discover().await?.into_iter().next() {
        Some(session_id) => {
            let group_id = session_id.group_id();
            core.load_session(session_id, group_id).await?
        }
        None => {
            let group_id = SessionGroupId::new();
            core.create_session(group_id)?
        }
    };
    TerminalApp::new(
        session,
        provider_id,
        std::env::var("AIRICODE_MODEL").unwrap_or_else(|_| "minimax-m3".into()),
    )
    .run()
    .await
}
