use crate::core::{
    Result,
    models::{NoteContent, ToolContext, ToolOutput},
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
