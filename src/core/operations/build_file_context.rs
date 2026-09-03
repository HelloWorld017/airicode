use std::sync::Arc;

use crate::core::{
    error::{Error, Result},
    hooks::BuildFileContextHookContext,
    models::{BuildFileContextRequest, FileContext, FileContextLine},
};

use super::Operations;

impl Operations {
    pub async fn build_file_context(
        &self,
        request: BuildFileContextRequest,
    ) -> Result<FileContext> {
        let bytes = self.workdir()?.read(&request.path).await?;
        if bytes.contains(&0) {
            return Err(Error::Tool(
                "cannot read binary/NUL-containing input".into(),
            ));
        }
        if request
            .max_bytes
            .is_some_and(|max_bytes| bytes.len() > max_bytes)
        {
            return Err(Error::Tool(format!(
                "file exceeds read limit of {} bytes",
                request.max_bytes.unwrap_or_default()
            )));
        }
        let source = Arc::<str>::from(
            String::from_utf8(bytes)
                .map_err(|_| Error::Tool("cannot read non-UTF-8 input".into()))?,
        );
        let total_lines = source.lines().count();
        let start_line = request.start_line.unwrap_or(1);
        let end_line = request.end_line.unwrap_or(total_lines);
        if (total_lines == 0 && (request.start_line.is_some() || request.end_line.is_some()))
            || (total_lines > 0
                && (start_line == 0
                    || end_line == 0
                    || start_line > end_line
                    || start_line > total_lines
                    || end_line > total_lines))
        {
            return Err(Error::Tool("invalid line range".into()));
        }
        let selected_lines = if total_lines == 0 {
            0
        } else {
            end_line - start_line + 1
        };
        if request
            .max_lines
            .is_some_and(|max_lines| selected_lines > max_lines)
        {
            return Err(Error::Tool(format!(
                "line range exceeds read limit of {} lines",
                request.max_lines.unwrap_or_default()
            )));
        }

        let mut file_context = FileContext {
            path: request.path.clone(),
            byte_len: source.len(),
            total_lines,
            lines: source
                .lines()
                .enumerate()
                .map(|(index, text)| FileContextLine {
                    line_number: index + 1,
                    text: text.to_string(),
                    display_prefix: (index + 1).to_string(),
                })
                .collect(),
        };
        let hook_context = BuildFileContextHookContext {
            path: request.path,
            source,
        };
        for (_, hook) in self.registry()?.hooks().build_file_context.clone() {
            hook.augment_file_context(hook_context.clone(), &mut file_context)
                .await?;
        }
        if total_lines > 0 {
            file_context
                .lines
                .retain(|line| line.line_number >= start_line && line.line_number <= end_line);
        }
        Ok(file_context)
    }
}
