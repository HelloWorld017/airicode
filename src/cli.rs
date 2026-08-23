use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "airicode", version, about)]
pub struct Cli {
    /// Load a final, highest-precedence TOML configuration file.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start an interactive terminal chat (the default invocation uses the current directory).
    Chat {
        /// Project directory exposed to the agent.
        #[arg(default_value = ".")]
        project: PathBuf,

        /// Named provider from the configuration.
        #[arg(long)]
        provider: Option<String>,

        /// Provider model identifier.
        #[arg(long)]
        model: Option<String>,

        /// Allow patch and shell tool execution without prompting.
        #[arg(long)]
        yes: bool,

        /// Do not write this session to the JSONL store.
        #[arg(long)]
        no_persist: bool,
    },

    /// Run one agent turn without a TUI.
    Run {
        /// Project directory exposed to the agent.
        project: PathBuf,

        /// Prompt to send to the agent.
        prompt: String,

        /// Named provider from the configuration.
        #[arg(long)]
        provider: Option<String>,

        /// Provider model identifier.
        #[arg(long)]
        model: Option<String>,

        /// Allow patch and shell tool execution.
        #[arg(long)]
        yes: bool,

        /// Do not write this session to the JSONL store.
        #[arg(long)]
        no_persist: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_with_default_project_and_options() {
        let cli = Cli::parse_from([
            "airicode",
            "chat",
            "--provider",
            "openrouter",
            "--model",
            "model-id",
            "--yes",
            "--no-persist",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::Chat {
                project,
                provider: Some(provider),
                model: Some(model),
                yes: true,
                no_persist: true,
            }) if project == std::path::Path::new(".") && provider == "openrouter" && model == "model-id"
        ));
    }

    #[test]
    fn run_interface_remains_positional() {
        let cli = Cli::parse_from(["airicode", "run", "/tmp/project", "say hello"]);
        assert!(matches!(
            cli.command,
            Some(Command::Run { project, prompt, .. })
                if project == std::path::Path::new("/tmp/project") && prompt == "say hello"
        ));
    }
}
