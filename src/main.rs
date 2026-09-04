use std::{ffi::OsString, path::PathBuf, sync::Arc};

use airicode::{
    core::{
        CoreBuilder, SessionGroupId,
        config::{ConfigPaths, load_config},
        models::ShellActionContext,
        project_from_path,
        workdir::NativeWorkdir,
    },
    plugins::{
        ActionConfigPlugin, InstructionBasePlugin, OpenAiProviderPlugin, PersistencePlugin,
        ToolDeletePlugin, ToolFindFilePlugin, ToolGrepPlugin, ToolPatchApplyPatchPlugin,
        ToolPatchHashlinePlugin, ToolPatchPlugin, ToolQuestionPlugin, ToolReadPlugin,
        ToolRenamePlugin, ToolShellPlugin, ToolTodoPlugin, ToolWebfetchPlugin, ToolWritePlugin,
    },
    ui::terminal::TerminalApp,
};

#[tokio::main]
async fn main() -> airicode::Result<()> {
    let (root, action) = parse_cli(std::env::args_os().skip(1).collect())?;
    let root = std::fs::canonicalize(root)?;

    let project = project_from_path(root.clone());
    let loaded_config = load_config(&ConfigPaths::for_project(&project.root)).await;
    for diagnostic in &loaded_config.diagnostics {
        eprintln!("warning: {diagnostic}");
    }
    let persistence = Arc::new(PersistencePlugin::new());
    let builder = CoreBuilder::new()
        .project(project.clone())
        .config(loaded_config.raw)
        .plugin(persistence.clone())
        .plugin(Arc::new(ActionConfigPlugin::new()))
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
    for diagnostic in core.startup_diagnostics() {
        eprintln!("warning: {diagnostic}");
    }
    if let Some((name, arguments)) = action {
        let output = core
            .shell_action_handler()
            .handle_args(
                std::iter::once(name).chain(arguments),
                ShellActionContext {
                    project_id: project.id,
                    project_root: project.root.clone(),
                    workdir: Arc::new(NativeWorkdir::new(project.root.clone())?),
                    cancellation: tokio_util::sync::CancellationToken::new(),
                },
            )
            .await?;
        println!("{output}");
        return Ok(());
    }
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

fn parse_cli(
    arguments: Vec<OsString>,
) -> airicode::Result<(PathBuf, Option<(String, Vec<String>)>)> {
    let mut arguments = arguments.into_iter();
    let mut root = std::env::current_dir()?;
    let first = arguments.next();
    let action = match first {
        Some(flag) if flag == "--project" => {
            root = PathBuf::from(arguments.next().ok_or_else(|| {
                airicode::Error::Command("--project requires a directory".into())
            })?);
            arguments
                .next()
                .map(|name| -> airicode::Result<(String, Vec<String>)> {
                    let name = name.into_string().map_err(|_| {
                        airicode::Error::Command("shell action names must be valid UTF-8".into())
                    })?;
                    let arguments = arguments
                        .map(|argument| {
                            argument.into_string().map_err(|_| {
                                airicode::Error::Command(
                                    "shell action arguments must be valid UTF-8".into(),
                                )
                            })
                        })
                        .collect::<airicode::Result<Vec<_>>>()?;
                    Ok((name, arguments))
                })
                .transpose()?
        }
        Some(name) => {
            let name = name.into_string().map_err(|_| {
                airicode::Error::Command("shell action names must be valid UTF-8".into())
            })?;
            let arguments = arguments
                .map(|argument| {
                    argument.into_string().map_err(|_| {
                        airicode::Error::Command(
                            "shell action arguments must be valid UTF-8".into(),
                        )
                    })
                })
                .collect::<airicode::Result<Vec<_>>>()?;
            Some((name, arguments))
        }
        None => None,
    };
    Ok((root, action))
}
