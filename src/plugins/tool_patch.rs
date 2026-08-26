use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        models::{
            NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::hashline,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    Add,
    Delete,
    Replace,
    InsertBefore,
    InsertAfter,
}

#[derive(Clone, Debug)]
struct Operation {
    kind: OperationKind,
    path: String,
    start: Option<hashline::Anchor>,
    end: Option<hashline::Anchor>,
    body: String,
}

struct AppliedOperation {
    path: String,
    old: String,
    new: String,
    kind: OperationKind,
}

pub struct ToolPatch {
    id: ToolId,
    max_bytes: usize,
}

impl ToolPatch {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_bytes: 1024 * 1024,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Default for ToolPatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolPatch {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "patch_hashline".into(),
            description: include_str!("../prompts/tool_patch.txt").into(),
            input: ToolInputDefinition::Text,
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Text(text) = input else {
            return Err(Error::Tool("patch input must be text".into()));
        };
        let operations = match parse_patch(&text) {
            Ok(operations) => operations,
            Err(message) => return Ok(ToolOutput::Failure { content: message }),
        };
        if operations.is_empty() {
            return Ok(ToolOutput::Failure {
                content: "patch requires at least one operation".into(),
            });
        }

        let mut applied = Vec::with_capacity(operations.len());
        for operation in operations {
            match self.apply_operation(operation, &context).await {
                Ok(value) => applied.push(value),
                Err(ApplyError::Failure(message)) => {
                    return Ok(ToolOutput::Failure { content: message });
                }
                Err(ApplyError::Error(error)) => return Err(error),
            }
        }

        let mut summaries = Vec::with_capacity(applied.len());
        for operation in applied {
            let diff = unified_diff(&operation.path, &operation.old, &operation.new);
            let (added, removed) = diff_stats(&diff);
            context
                .operations
                .add_note(
                    NoteContent::Diff {
                        file: operation.path.clone(),
                        content: diff,
                    },
                    [
                        ("tool".into(), Value::String("patch".into())),
                        (
                            "operation".into(),
                            Value::String(operation_name(operation.kind).into()),
                        ),
                    ],
                )
                .await?;
            let verb = operation_verb(operation.kind);
            summaries.push(format!("{verb} {} (+{added}/-{removed})", operation.path));
        }
        Ok(ToolOutput::Success {
            content: summaries.join("\n"),
        })
    }
}

enum ApplyError {
    Failure(String),
    Error(Error),
}

impl ToolPatch {
    async fn apply_operation(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        match operation.kind {
            OperationKind::Add => self.apply_add(operation, context).await,
            OperationKind::Delete => self.apply_delete(operation, context).await,
            OperationKind::Replace => self.apply_replace(operation, context).await,
            OperationKind::InsertBefore | OperationKind::InsertAfter => {
                self.apply_insert(operation, context).await
            }
        }
    }

    async fn apply_add(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        match context.workdir.read(Path::new(&operation.path)).await {
            Ok(_) => {
                return Err(ApplyError::Failure(format!(
                    "cannot add existing file: {}",
                    operation.path
                )))
            }
            Err(Error::Workdir(_)) => {}
            Err(error) => return Err(ApplyError::Error(error)),
        }
        if operation.body.len() > self.max_bytes {
            return Err(ApplyError::Failure(format!(
                "patched file exceeds limit of {} bytes",
                self.max_bytes
            )));
        }
        if context.cancellation.is_cancelled() {
            return Err(ApplyError::Error(Error::Cancelled));
        }
        context
            .workdir
            .write(Path::new(&operation.path), operation.body.as_bytes())
            .await
            .map_err(ApplyError::Error)?;
        Ok(AppliedOperation {
            path: operation.path,
            old: String::new(),
            new: operation.body,
            kind: OperationKind::Add,
        })
    }

    async fn apply_delete(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        let old = read_text(context, &operation.path).await?;
        if context.cancellation.is_cancelled() {
            return Err(ApplyError::Error(Error::Cancelled));
        }
        context
            .workdir
            .remove(Path::new(&operation.path))
            .await
            .map_err(ApplyError::Error)?;
        Ok(AppliedOperation {
            path: operation.path,
            old,
            new: String::new(),
            kind: OperationKind::Delete,
        })
    }

    async fn apply_replace(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        let old = read_text(context, &operation.path).await?;
        let old_lines = hashline::render(&old);
        let start_anchor = operation
            .start
            .as_ref()
            .ok_or_else(|| ApplyError::Failure("REPLACE requires a start anchor".into()))?;
        let end_anchor = operation
            .end
            .as_ref()
            .ok_or_else(|| ApplyError::Failure("REPLACE requires an end anchor".into()))?;
        let anchors = [start_anchor, end_anchor];
        let start = resolve_anchor(&old_lines, start_anchor)
            .map_err(|error| with_patch_context(error, &operation.path, &old_lines, &anchors))?;
        let end = resolve_anchor(&old_lines, end_anchor)
            .map_err(|error| with_patch_context(error, &operation.path, &old_lines, &anchors))?;
        if start > end {
            return Err(ApplyError::Failure(
                "REPLACE start anchor must not come after end anchor".into(),
            ));
        }
        let new = hashline::replace_lines(&old, start + 1, end + 1, &operation.body)
            .ok_or_else(|| ApplyError::Failure("replace range is out of bounds".into()))?;
        if new == old {
            return Err(ApplyError::Failure(
                "REPLACE does not change the file".into(),
            ));
        }
        if new.len() > self.max_bytes {
            return Err(ApplyError::Failure(format!(
                "patched file exceeds limit of {} bytes",
                self.max_bytes
            )));
        }
        if context.cancellation.is_cancelled() {
            return Err(ApplyError::Error(Error::Cancelled));
        }
        context
            .workdir
            .write(Path::new(&operation.path), new.as_bytes())
            .await
            .map_err(ApplyError::Error)?;
        Ok(AppliedOperation {
            path: operation.path,
            old,
            new,
            kind: OperationKind::Replace,
        })
    }

    async fn apply_insert(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        let old = read_text(context, &operation.path).await?;
        let old_lines = hashline::render(&old);
        let anchor = operation
            .start
            .as_ref()
            .ok_or_else(|| ApplyError::Failure("INSERT requires an anchor".into()))?;
        let anchors = [anchor];
        let index = resolve_anchor(&old_lines, anchor)
            .map_err(|error| with_patch_context(error, &operation.path, &old_lines, &anchors))?;
        let line = match operation.kind {
            OperationKind::InsertBefore => index + 1,
            OperationKind::InsertAfter => index + 2,
            _ => return Err(ApplyError::Failure("invalid INSERT operation".into())),
        };
        let new = insert_lines(&old, line, &operation.body)
            .ok_or_else(|| ApplyError::Failure("insert position is out of bounds".into()))?;
        if new == old {
            return Err(ApplyError::Failure(
                "INSERT does not change the file".into(),
            ));
        }
        if new.len() > self.max_bytes {
            return Err(ApplyError::Failure(format!(
                "patched file exceeds limit of {} bytes",
                self.max_bytes
            )));
        }
        if context.cancellation.is_cancelled() {
            return Err(ApplyError::Error(Error::Cancelled));
        }
        context
            .workdir
            .write(Path::new(&operation.path), new.as_bytes())
            .await
            .map_err(ApplyError::Error)?;
        Ok(AppliedOperation {
            path: operation.path,
            old,
            new,
            kind: operation.kind,
        })
    }
}

fn operation_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Add => "add",
        OperationKind::Delete => "delete",
        OperationKind::Replace => "replace",
        OperationKind::InsertBefore => "insert_before",
        OperationKind::InsertAfter => "insert_after",
    }
}

fn operation_verb(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Add => "Added",
        OperationKind::Delete => "Deleted",
        OperationKind::Replace => "Replaced",
        OperationKind::InsertBefore => "Inserted before",
        OperationKind::InsertAfter => "Inserted after",
    }
}

async fn read_text(context: &ToolContext, path: &str) -> std::result::Result<String, ApplyError> {
    let bytes = context
        .workdir
        .read(Path::new(path))
        .await
        .map_err(|error| match error {
            Error::Workdir(message) => ApplyError::Failure(message),
            error => ApplyError::Error(error),
        })?;
    if bytes.contains(&0) {
        return Err(ApplyError::Failure(
            "cannot patch binary/NUL-containing input".into(),
        ));
    }
    String::from_utf8(bytes).map_err(|_| ApplyError::Failure("cannot patch non-UTF-8 input".into()))
}

#[derive(Clone, Debug)]
struct OperationHeader {
    kind: OperationKind,
    path: String,
    start: Option<hashline::Anchor>,
    end: Option<hashline::Anchor>,
    delimiter: Option<String>,
}

fn parse_patch(text: &str) -> std::result::Result<Vec<Operation>, String> {
    let mut operations = Vec::new();
    let mut lines = text.split_inclusive('\n').enumerate();
    while let Some((line_number, raw_line)) = lines.next() {
        let line = trim_line_ending(raw_line);
        if line.trim().is_empty() {
            continue;
        }
        let header = parse_header(line)
            .map_err(|message| format!("{message} at line {}", line_number + 1))?;
        let Some(delimiter) = header.delimiter.clone() else {
            operations.push(Operation {
                kind: header.kind,
                path: header.path,
                start: header.start,
                end: header.end,
                body: String::new(),
            });
            continue;
        };

        let mut body = String::new();
        let mut closed = false;
        for (_, raw_body_line) in lines.by_ref() {
            if trim_line_ending(raw_body_line) == delimiter {
                closed = true;
                break;
            }
            body.push_str(raw_body_line);
        }
        if !closed {
            return Err(format!(
                "unterminated heredoc for {} at line {}",
                header.path,
                line_number + 1
            ));
        }
        operations.push(Operation {
            kind: header.kind,
            path: header.path,
            start: header.start,
            end: header.end,
            body,
        });
    }
    Ok(operations)
}

fn trim_line_ending(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn parse_header(line: &str) -> std::result::Result<OperationHeader, String> {
    if let Some(rest) = line.strip_prefix("ADD ") {
        let (path, delimiter) = split_heredoc(rest)?;
        return Ok(OperationHeader {
            kind: OperationKind::Add,
            path: parse_path(path)?,
            start: None,
            end: None,
            delimiter: Some(delimiter),
        });
    }
    if let Some(rest) = line.strip_prefix("REPLACE ") {
        let (spec, delimiter) = split_heredoc(rest)?;
        let (path_and_start, end) = spec
            .rsplit_once(" TO ")
            .ok_or_else(|| "REPLACE header must contain FROM and TO anchors".to_string())?;
        let (path, start) = path_and_start
            .rsplit_once(" FROM ")
            .ok_or_else(|| "REPLACE header must contain FROM and TO anchors".to_string())?;
        return Ok(OperationHeader {
            kind: OperationKind::Replace,
            path: parse_path(path)?,
            start: Some(parse_anchor(start)?),
            end: Some(parse_anchor(end)?),
            delimiter: Some(delimiter),
        });
    }
    if let Some(rest) = line.strip_prefix("INSERT ") {
        let (spec, delimiter) = split_heredoc(rest)?;
        if let Some((path, anchor)) = spec.rsplit_once(" BEFORE ") {
            return Ok(OperationHeader {
                kind: OperationKind::InsertBefore,
                path: parse_path(path)?,
                start: Some(parse_anchor(anchor)?),
                end: None,
                delimiter: Some(delimiter),
            });
        }
        if let Some((path, anchor)) = spec.rsplit_once(" AFTER ") {
            return Ok(OperationHeader {
                kind: OperationKind::InsertAfter,
                path: parse_path(path)?,
                start: Some(parse_anchor(anchor)?),
                end: None,
                delimiter: Some(delimiter),
            });
        }
        return Err("INSERT header must contain BEFORE or AFTER and an anchor".into());
    }
    if let Some(rest) = line.strip_prefix("DELETE ") {
        return Ok(OperationHeader {
            kind: OperationKind::Delete,
            path: parse_path(rest)?,
            start: None,
            end: None,
            delimiter: None,
        });
    }
    Err("invalid patch header".into())
}

fn split_heredoc(value: &str) -> std::result::Result<(&str, String), String> {
    let (spec, delimiter) = value
        .rsplit_once(" <<<")
        .ok_or_else(|| "patch header must end with <<<heredoc-tag".to_string())?;
    if delimiter.is_empty() || delimiter.chars().any(char::is_whitespace) {
        return Err("heredoc tag cannot be empty or contain whitespace".into());
    }
    Ok((spec, delimiter.to_string()))
}

fn parse_path(value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.contains("@@") {
        return Err("patch path cannot be empty or contain @@".into());
    }
    Ok(value.to_string())
}

fn parse_anchor(value: &str) -> std::result::Result<hashline::Anchor, String> {
    let (line, tag) = value
        .trim()
        .split_once(':')
        .ok_or_else(|| "anchor must have the form '<line>:<3-character-hash>'".to_string())?;
    let line = line
        .parse::<usize>()
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| "anchor line must be a positive number".to_string())?;
    if tag.len() != 3 || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err("anchor hash must contain exactly three alphanumeric characters".into());
    }
    Ok(hashline::Anchor {
        line,
        tag: tag.to_string(),
    })
}

fn resolve_anchor(
    lines: &[hashline::HashLine],
    anchor: &hashline::Anchor,
) -> std::result::Result<usize, ApplyError> {
    let matches = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line.tag == anchor.tag).then_some(index))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(ApplyError::Failure(
            "stale patch: no matching hashline anchor; read file and retry".into(),
        )),
        _ => {
            let anchored_matches = matches
                .into_iter()
                .filter(|index| lines[*index].line == anchor.line)
                .collect::<Vec<_>>();
            match anchored_matches.as_slice() {
                [index] => Ok(*index),
                [] => Err(ApplyError::Failure(
                    "stale patch: hashline matches found, but supplied line number does not match; read file and retry"
                        .into(),
                )),
                _ => Err(ApplyError::Failure(
                    "ambiguous patch: multiple hashline anchors remain after line validation; read file and retry"
                        .into(),
                )),
            }
        }
    }
}

fn with_patch_context(
    error: ApplyError,
    path: &str,
    lines: &[hashline::HashLine],
    anchors: &[&hashline::Anchor],
) -> ApplyError {
    match error {
        ApplyError::Failure(message) => ApplyError::Failure(format!(
            "{message}\nfailed patch range in {path}: {}\ncurrent context:\n{}",
            patch_range(anchors),
            surrounding_context(lines, anchors),
        )),
        ApplyError::Error(error) => ApplyError::Error(error),
    }
}

fn patch_range(anchors: &[&hashline::Anchor]) -> String {
    let Some(first) = anchors.iter().map(|anchor| anchor.line).min() else {
        return "unknown".into();
    };
    let last = anchors
        .iter()
        .map(|anchor| anchor.line)
        .max()
        .unwrap_or(first);
    format!("{first}-{last}")
}

fn surrounding_context(lines: &[hashline::HashLine], anchors: &[&hashline::Anchor]) -> String {
    if lines.is_empty() {
        return "(file is empty)".into();
    }
    let requested_start = anchors.iter().map(|anchor| anchor.line).min().unwrap_or(1);
    let requested_end = anchors
        .iter()
        .map(|anchor| anchor.line)
        .max()
        .unwrap_or(requested_start);
    let start = requested_start.clamp(1, lines.len());
    let end = requested_end.clamp(start, lines.len());
    let start = start.saturating_sub(3).max(1);
    let end = end.saturating_add(3).min(lines.len());
    lines[start - 1..end]
        .iter()
        .map(|line| format!("{}:{}|{}", line.line, line.tag, line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn insert_lines(text: &str, line: usize, replacement: &str) -> Option<String> {
    let lines = hashline::split_lines_preserving_endings(text);
    if line == 0 || line > lines.len() + 1 {
        return None;
    }
    if replacement.is_empty() {
        return Some(text.to_string());
    }
    let split = line - 1;
    let before = lines[..split].concat();
    let after = lines[split..].concat();
    let mut result = before;
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(replacement);
    if !after.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(&after);
    Some(result)
}

fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines = old.lines().collect::<Vec<_>>();
    let new_lines = new.lines().collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - suffix - 1] == new_lines[new_lines.len() - suffix - 1]
    {
        suffix += 1;
    }
    let old_end = old_lines.len().saturating_sub(suffix);
    let new_end = new_lines.len().saturating_sub(suffix);
    let mut diff = format!(
        "--- {path}\n+++ {path}\n@@ -{},{} +{},{} @@\n",
        prefix + 1,
        old_end - prefix,
        prefix + 1,
        new_end - prefix
    );
    for line in &old_lines[prefix..old_end] {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in &new_lines[prefix..new_end] {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn diff_stats(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

pub struct ToolPatchPlugin {
    id: PluginId,
    tool: Arc<ToolPatch>,
}

impl ToolPatchPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolPatch::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolPatchPlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_patch"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_uses_the_dedicated_patch_prompt() {
        let definition = ToolPatch::new().definition();

        assert_eq!(
            definition.description,
            include_str!("../prompts/tool_patch.txt")
        );
        assert_eq!(definition.input, ToolInputDefinition::Text);
    }
}
