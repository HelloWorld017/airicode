use std::{
    env,
    error::Error,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use airicode::{
    cli::{Cli, Command},
    config::{Config, ProviderConfig},
    core::{NativeWorkdir, OpenSession, ProviderEvent, ProviderId, RuntimeEvent, Session},
    plugins::{
        agents_instructions_plugin, approval_plugin, base_instructions_plugin,
        bubblewrap_sandbox_plugin, compaction_plugin, fork_plugin, git_worktree_plugin,
        grep_plugin, jsonl_persistence_plugin, patch_plugin, read_plugin, revert_plugin,
        shell_plugin, sidequery_plugin, skills_plugin, subagents_plugin, todo_plugin,
        webfetch_plugin, AgentsInstructionsConfig, ApprovalPolicy, BubblewrapFileOperation,
        BubblewrapPathRule, BubblewrapProcessRule, BubblewrapSandboxConfig, CompactionPluginConfig,
        ForkConfig, GitWorktreeConfig, JsonlPersistenceConfig, RevertConfig, SideQueryConfig,
        SkillsConfig, SubagentConfig, WebFetchConfig,
    },
    providers::{openai_plugin, openrouter_plugin, OpenAiConfig, OpenRouterConfig},
    ui::terminal,
    Core,
};
use clap::Parser;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("airicode: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Chat {
        project: PathBuf::from("."),
        provider: None,
        model: None,
        yes: false,
        no_persist: false,
    }) {
        Command::Run {
            project,
            prompt,
            provider,
            model,
            yes,
            no_persist,
        } => {
            let app = bootstrap(
                &project,
                cli.config.as_deref(),
                provider,
                model,
                yes,
                no_persist,
            )
            .await?;
            run_once(app, prompt).await
        }
        Command::Chat {
            project,
            provider,
            model,
            yes,
            no_persist,
        } => {
            let app = bootstrap(
                &project,
                cli.config.as_deref(),
                provider,
                model,
                yes,
                no_persist,
            )
            .await?;
            let events = app.core.subscribe();
            let options = terminal::Options {
                provider: app.provider_name,
                model: app.model,
                mode: app.config.default_mode.clone(),
                project: canonical_identity(&project),
                color: app.config.ui.color,
                show_reasoning: app.config.ui.show_reasoning,
                show_tool_calls: app.config.ui.show_tool_calls,
            };
            terminal::run(app.session, events, options).await
        }
    }
}

struct App {
    core: Core,
    session: Session,
    config: Config,
    provider_name: String,
    model: String,
}

async fn bootstrap(
    project: &Path,
    explicit_config: Option<&Path>,
    provider_name: Option<String>,
    model: Option<String>,
    yes: bool,
    no_persist: bool,
) -> Result<App, Box<dyn Error>> {
    let config = Config::load(project, explicit_config)?;
    let provider_name = provider_name
        .or_else(|| env::var("AIRICODE_PROVIDER").ok())
        .unwrap_or_else(|| config.default_provider.clone());
    let model = model
        .or_else(|| env::var("AIRICODE_MODEL").ok())
        .unwrap_or_else(|| config.default_model.clone());
    if model.trim().is_empty() {
        return Err("selected model may not be empty".into());
    }
    let provider_config = config
        .provider
        .get(&provider_name)
        .ok_or_else(|| format!("provider {provider_name:?} is not configured"))?;
    let api_key = env::var(provider_config.api_key_env()).map_err(|_| {
        format!(
            "provider {provider_name:?} requires environment variable {}",
            provider_config.api_key_env()
        )
    })?;
    if api_key.is_empty() {
        return Err(format!(
            "provider {provider_name:?} environment variable {} is empty",
            provider_config.api_key_env()
        )
        .into());
    }

    let provider_plugin = match provider_config {
        ProviderConfig::OpenAi { base_url, .. } => {
            let mut provider = OpenAiConfig::new(api_key);
            provider.base_url = base_url.clone();
            openai_plugin(provider)
        }
        ProviderConfig::OpenRouter { base_url, .. } => {
            let mut provider = OpenRouterConfig::new(api_key);
            provider.base_url = base_url.clone();
            openrouter_plugin(provider)
        }
    };

    let approval = if yes {
        ApprovalPolicy::Allow
    } else {
        ApprovalPolicy::Deny
    };
    let mode_instructions = config
        .modes
        .get(&config.default_mode)
        .map(|mode| mode.instructions.clone())
        .unwrap_or_default();
    let mut core = Core::new()
        .with_plugin(provider_plugin)
        .with_plugin(grep_plugin())
        .with_plugin(read_plugin())
        .with_plugin(patch_plugin())
        .with_plugin(shell_plugin())
        .with_plugin(todo_plugin())
        .with_plugin(webfetch_plugin(WebFetchConfig::default()))
        .with_plugin(sidequery_plugin(SideQueryConfig::new(model.clone())))
        .with_plugin(subagents_plugin(SubagentConfig::new(model.clone())))
        .with_plugin(revert_plugin(RevertConfig::default()))
        .with_plugin(approval_plugin(approval))
        .with_plugin(base_instructions_plugin(mode_instructions))
        .with_plugin(agents_instructions_plugin(
            AgentsInstructionsConfig::default(),
        ));
    if config.compaction.auto {
        core = core.with_plugin(compaction_plugin(CompactionPluginConfig {
            reserved_output_tokens: config.compaction.reserved_tokens as usize,
            ..CompactionPluginConfig::default()
        }));
    }
    if is_git_project(project) {
        core = core.with_plugin(git_worktree_plugin(GitWorktreeConfig::new(
            project,
            &config.persistence.data_dir,
        )));
    }
    let skills_dir = project.join(".airicode/skills");
    if skills_dir.is_dir() {
        core = core.with_plugin(skills_plugin(SkillsConfig::new(skills_dir)));
    }
    if !no_persist {
        core = core.with_plugin(jsonl_persistence_plugin(
            JsonlPersistenceConfig::new(&config.persistence.data_dir)
                .with_fsync(config.persistence.fsync),
        ));
        core = core.with_plugin(fork_plugin(ForkConfig::stored_in(
            config.persistence.data_dir.join("forks"),
        )));
    } else {
        core = core.with_plugin(fork_plugin(ForkConfig::default()));
    }
    if config.sandbox.enabled {
        let mut path_rules = vec![BubblewrapPathRule {
            operation: Some(BubblewrapFileOperation::Read),
            path: PathBuf::new(),
            allow: true,
        }];
        for path in &config.sandbox.writable_paths {
            path_rules.push(BubblewrapPathRule {
                operation: Some(BubblewrapFileOperation::Write),
                path: path.clone(),
                allow: true,
            });
            path_rules.push(BubblewrapPathRule {
                operation: Some(BubblewrapFileOperation::Remove),
                path: path.clone(),
                allow: true,
            });
        }
        core = core.with_plugin(bubblewrap_sandbox_plugin(BubblewrapSandboxConfig {
            path_rules,
            process_rules: vec![BubblewrapProcessRule {
                program: None,
                allow: true,
            }],
            writable_paths: config.sandbox.writable_paths.clone(),
            default_allow: false,
            allow_network: config.sandbox.allow_network,
        }));
    }
    let core = core.build().await?;

    let workdir = Arc::new(
        NativeWorkdir::new(project)?.with_max_output_bytes(config.sandbox.max_output_bytes),
    );
    let identity = canonical_identity(project);
    let project_handle = core.open_project(identity, workdir);
    let session = project_handle
        .open_session(OpenSession {
            id: None,
            provider: ProviderId::from(provider_config.runtime_id()),
            model: model.clone(),
        })
        .await?;
    Ok(App {
        core,
        session,
        config,
        provider_name,
        model,
    })
}

async fn run_once(app: App, prompt: String) -> Result<(), Box<dyn Error>> {
    let mut events = app.core.subscribe();
    let turn_id = app.session.send_text(prompt).await?;
    loop {
        match events.recv().await? {
            RuntimeEvent::ProviderEvent {
                session_id,
                turn_id: event_turn,
                event,
            } if session_id == app.session.id() && event_turn == turn_id => match event {
                ProviderEvent::TextDelta { text } => {
                    print!("{text}");
                    io::stdout().flush()?;
                }
                ProviderEvent::ReasoningDelta { text } if app.config.ui.show_reasoning => {
                    eprint!("{text}");
                    io::stderr().flush()?;
                }
                ProviderEvent::ToolCallDelta {
                    name: Some(name), ..
                } if app.config.ui.show_tool_calls => {
                    eprintln!("[tool] {name}");
                }
                _ => {}
            },
            RuntimeEvent::TurnCompleted {
                session_id,
                turn_id: event_turn,
            } if session_id == app.session.id() && event_turn == turn_id => {
                println!();
                return Ok(());
            }
            RuntimeEvent::TurnFailed {
                session_id,
                turn_id: event_turn,
                error,
            } if session_id == app.session.id() && event_turn == turn_id => {
                eprintln!("turn failed: {error}");
                return Err("agent turn failed".into());
            }
            RuntimeEvent::TurnCancelled {
                session_id,
                turn_id: event_turn,
            } if session_id == app.session.id() && event_turn == turn_id => {
                eprintln!("turn cancelled");
                return Err("agent turn was cancelled".into());
            }
            _ => {}
        }
    }
}

fn canonical_identity(project: &Path) -> String {
    std::fs::canonicalize(project)
        .unwrap_or_else(|_| project.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn is_git_project(project: &Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(project)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
