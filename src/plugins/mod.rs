pub mod instruction_base;
pub mod persistence;
pub mod provider_openai;
pub mod native_workdir {
    pub use crate::core::workdir::{NativeWorkdir, Workdir, WorkdirLayer};
}
pub mod tool_find;
pub mod tool_grep;
pub mod tool_patch;
pub mod tool_question;
pub mod tool_read;
pub mod tool_shell;
pub mod tool_todo;
pub mod tool_webfetch;

use crate::core::{
    models::{NoteContent, ToolContext, ToolOutput},
    Result,
};

pub(crate) async fn add_tool_note(
    context: &ToolContext,
    content: NoteContent,
    tool: &str,
) -> Result<()> {
    context
        .operations
        .add_note(
            content,
            [("tool".into(), serde_json::Value::String(tool.into()))],
        )
        .await
        .map(|_| ())
}

pub(crate) async fn add_output_note(
    context: &ToolContext,
    tool: &str,
    summary: impl Into<String>,
    output: &ToolOutput,
) -> Result<()> {
    let summary = summary.into();
    let content = match output {
        ToolOutput::Success { .. } => NoteContent::Subtle { content: summary },
        ToolOutput::Failure { content } => NoteContent::Alert {
            content: format!(
                "{summary}: {}",
                content.lines().next().unwrap_or("tool failed")
            ),
        },
        ToolOutput::Stop => NoteContent::Info { content: summary },
    };
    add_tool_note(context, content, tool).await
}

pub use instruction_base::{InstructionBasePlugin, DEFAULT_BASE_INSTRUCTION};
pub use persistence::{
    default_data_root, JsonlSessionStore, JsonlStore, PersistencePlugin, SessionLogRecord,
};
pub use provider_openai::{
    OpenAIProvider, OpenAIProviderPlugin, OpenAiProvider, OpenAiProviderPlugin, ProviderOpenAI,
    ProviderOpenAIPlugin,
};
pub use tool_find::{ToolFind, ToolFindPlugin};
pub use tool_grep::{ToolGrep, ToolGrepPlugin};
pub use tool_patch::{ToolPatch, ToolPatchPlugin};
pub use tool_question::{ToolQuestion, ToolQuestionPlugin};
pub use tool_read::{ToolRead, ToolReadPlugin};
pub use tool_shell::{ToolShell, ToolShellPlugin};
pub use tool_todo::{TodoItem, ToolTodo, ToolTodoPlugin};
pub use tool_webfetch::{ToolWebfetch, ToolWebfetchPlugin};
