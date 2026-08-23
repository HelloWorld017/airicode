use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::{
    CommandSpec, Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext,
    ToolDefinition, ToolId, ToolOutput,
};

const SHELL_TOOL_ID: &str = "builtin.shell";
const SHELL_TOOL_NAME: &str = "shell";
const SHELL_PLUGIN_ID: &str = "builtin.tool-shell";

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const DEFAULT_OUTPUT_BYTES: usize = 200_000;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default)]
struct ShellTool;

struct ShellPlugin;

pub fn shell_plugin() -> Arc<dyn Plugin> {
    Arc::new(ShellPlugin)
}

#[async_trait]
impl Plugin for ShellPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(SHELL_PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_tool(0, Arc::new(ShellTool))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellInput {
    #[serde(default)]
    program: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_output_bytes")]
    max_output_bytes: usize,
}

#[derive(Serialize)]
struct ShellResponse {
    status: i32,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    timed_out: bool,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_output_bytes() -> usize {
    DEFAULT_OUTPUT_BYTES
}

#[async_trait]
impl Tool for ShellTool {
    fn id(&self) -> ToolId {
        ToolId::new(SHELL_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: SHELL_TOOL_NAME.into(),
            description:
                "Run a program with arguments or a shell script inside the project workdir.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "oneOf": [
                    { "required": ["program"], "not": { "required": ["script"] } },
                    { "required": ["script"], "not": { "required": ["program"] } }
                ],
                "properties": {
                    "program": { "type": "string", "minLength": 1 },
                    "args": { "type": "array", "items": { "type": "string" }, "default": [] },
                    "script": { "type": "string" },
                    "cwd": { "type": "string" },
                    "env": { "type": "object", "additionalProperties": { "type": "string" }, "default": {} },
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS },
                    "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": MAX_OUTPUT_BYTES, "default": DEFAULT_OUTPUT_BYTES }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let input: ShellInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid shell input: {error}")))?;
        if input.timeout_ms == 0 || input.timeout_ms > MAX_TIMEOUT_MS {
            return Err(Error::Tool(format!(
                "timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"
            )));
        }
        if input.max_output_bytes == 0 || input.max_output_bytes > MAX_OUTPUT_BYTES {
            return Err(Error::Tool(format!(
                "max_output_bytes must be between 1 and {MAX_OUTPUT_BYTES}"
            )));
        }
        let (program, args) =
            match (input.program, input.script) {
                (Some(program), None) if !program.is_empty() => (program, input.args),
                (None, Some(script)) if input.args.is_empty() => {
                    ("sh".into(), vec!["-c".into(), script])
                }
                _ => return Err(Error::Tool(
                    "provide exactly one of program or script; args are only valid with program"
                        .into(),
                )),
            };
        let command = CommandSpec {
            program,
            args,
            cwd: input.cwd,
            env: input.env,
        };
        let command_cancellation = CancellationToken::new();
        let execution = context
            .workdir
            .execute(command, command_cancellation.clone());
        tokio::pin!(execution);

        let (output, timed_out) = tokio::select! {
            result = &mut execution => (result?, false),
            _ = context.cancellation.cancelled() => {
                command_cancellation.cancel();
                let _ = execution.await;
                return Err(Error::Cancelled);
            }
            _ = tokio::time::sleep(Duration::from_millis(input.timeout_ms)) => {
                command_cancellation.cancel();
                let _ = execution.await;
                return Ok(ToolOutput {
                    content: serde_json::to_string(&ShellResponse {
                        status: -1,
                        stdout: String::new(),
                        stderr: "command timed out".into(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        timed_out: true,
                    }).map_err(|error| Error::Tool(format!("could not encode shell output: {error}")))?,
                    is_error: true,
                });
            }
        };
        let stdout_truncated = output.stdout.len() > input.max_output_bytes;
        let stderr_truncated = output.stderr.len() > input.max_output_bytes;
        let stdout = String::from_utf8_lossy(
            &output.stdout[..output.stdout.len().min(input.max_output_bytes)],
        )
        .into_owned();
        let stderr = String::from_utf8_lossy(
            &output.stderr[..output.stderr.len().min(input.max_output_bytes)],
        )
        .into_owned();
        let response = ShellResponse {
            status: output.status,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            timed_out,
        };
        Ok(ToolOutput {
            content: serde_json::to_string(&response)
                .map_err(|error| Error::Tool(format!("could not encode shell output: {error}")))?,
            is_error: false,
        })
    }
}
