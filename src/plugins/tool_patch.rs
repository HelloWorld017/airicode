use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        models::{
            NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::hashline,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    Add,
    Delete,
    Edit,
}

#[derive(Clone, Debug)]
struct Operation {
    kind: OperationKind,
    path: String,
    line_hint: Option<usize>,
    body: Vec<BodyLine>,
}

#[derive(Clone, Debug)]
enum BodyLine {
    Context(String),
    Delete(String),
    Add(String),
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
            name: "patch".into(),
            description: r#"Apply a raw diff-like patch to root-relative workdir files. Headers are `ADD path`, `DEL path`, or `EDIT path`; an EDIT may use `EDIT path@@<line>` to disambiguate a repeated match. In an EDIT body, context lines use ` <hash>|`, deleted lines use `-<hash>|`, and new lines use `+<text>`; hashes must be copied from read output (the part after `<line>:` and before `|`). Context is preserved, deletion lines are removed, and additions are inserted. The complete sequence of context/deletion hashes must match the current file exactly once. A stale match fails, and multiple matches fail unless `@@<line>` identifies the starting line. ADD accepts only `+` lines and DEL has no body."#.into(),
            input_schema: crate::utils::schema::json_schema::<String>(),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let text = input
            .as_str()
            .ok_or_else(|| Error::Tool("patch input must be text".into()))?;
        let operations = match parse_patch(text) {
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
                            Value::String(
                                match operation.kind {
                                    OperationKind::Add => "add",
                                    OperationKind::Delete => "delete",
                                    OperationKind::Edit => "edit",
                                }
                                .into(),
                            ),
                        ),
                    ],
                )
                .await?;
            let verb = match operation.kind {
                OperationKind::Add => "Added",
                OperationKind::Delete => "Deleted",
                OperationKind::Edit => "Updated",
            };
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
            OperationKind::Edit => self.apply_edit(operation, context).await,
        }
    }

    async fn apply_add(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        if operation
            .body
            .iter()
            .any(|line| !matches!(line, BodyLine::Add(_)))
        {
            return Err(ApplyError::Failure("ADD accepts only + lines".into()));
        }
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
        let new = added_text(&operation.body);
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
            old: String::new(),
            new,
            kind: OperationKind::Add,
        })
    }

    async fn apply_delete(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        if !operation.body.is_empty() {
            return Err(ApplyError::Failure(
                "DEL does not accept patch body lines".into(),
            ));
        }
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

    async fn apply_edit(
        &self,
        operation: Operation,
        context: &ToolContext,
    ) -> std::result::Result<AppliedOperation, ApplyError> {
        let old = read_text(context, &operation.path).await?;
        let old_lines = hashline::render(&old);
        let pattern = operation
            .body
            .iter()
            .filter_map(|line| match line {
                BodyLine::Context(tag) | BodyLine::Delete(tag) => Some(tag.as_str()),
                BodyLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let start = find_unique_match(&old_lines, &pattern, operation.line_hint)?;
        let replacement = replacement_text(&operation.body, &old_lines, start)?;
        let end = start + pattern.len();
        let new = if pattern.is_empty() {
            insert_lines(&old, start + 1, &replacement)
                .ok_or_else(|| ApplyError::Failure("edit insertion is out of bounds".into()))?
        } else {
            hashline::replace_lines(&old, start + 1, end, &replacement)
                .ok_or_else(|| ApplyError::Failure("edit match is out of bounds".into()))?
        };
        if new == old {
            return Err(ApplyError::Failure("edit does not change the file".into()));
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
            kind: OperationKind::Edit,
        })
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

fn parse_patch(text: &str) -> std::result::Result<Vec<Operation>, String> {
    let mut operations = Vec::new();
    let mut current: Option<Operation> = None;
    for (line_number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some((kind, rest)) = parse_header(line) {
            if let Some(operation) = current.take() {
                operations.push(operation);
            }
            let (path, line_hint) = parse_path_hint(rest)
                .ok_or_else(|| format!("invalid patch header at line {}", line_number + 1))?;
            current = Some(Operation {
                kind,
                path,
                line_hint,
                body: Vec::new(),
            });
            continue;
        }
        let operation = current
            .as_mut()
            .ok_or_else(|| format!("patch body before header at line {}", line_number + 1))?;
        operation.body.push(
            parse_body_line(line)
                .map_err(|message| format!("{message} at line {}", line_number + 1))?,
        );
    }
    if let Some(operation) = current {
        operations.push(operation);
    }
    for operation in &operations {
        if operation.path.is_empty() {
            return Err("patch path cannot be empty".into());
        }
        if operation.kind == OperationKind::Delete && !operation.body.is_empty() {
            return Err(format!("DEL {} does not accept body lines", operation.path));
        }
    }
    Ok(operations)
}

fn parse_header(line: &str) -> Option<(OperationKind, &str)> {
    [
        ("ADD ", OperationKind::Add),
        ("DEL ", OperationKind::Delete),
        ("EDIT ", OperationKind::Edit),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| line.strip_prefix(prefix).map(|rest| (kind, rest)))
}

fn parse_path_hint(value: &str) -> Option<(String, Option<usize>)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (path, hint) = match value.rsplit_once("@@") {
        Some((path, hint)) if !hint.is_empty() => (path, Some(hint.parse().ok()?)),
        _ => (value, None),
    };
    if path.is_empty() || hint == Some(0) {
        return None;
    }
    Some((path.to_string(), hint))
}

fn parse_body_line(line: &str) -> std::result::Result<BodyLine, String> {
    let prefix = line
        .chars()
        .next()
        .ok_or_else(|| "patch body line cannot be empty".to_string())?;
    let body = &line[prefix.len_utf8()..];
    match prefix {
        '+' => Ok(BodyLine::Add(body.to_string())),
        ' ' => Ok(BodyLine::Context(parse_hash(body)?)),
        '-' => Ok(BodyLine::Delete(parse_hash(body)?)),
        _ => Err("patch body lines must start with space, +, or -".into()),
    }
}

fn parse_hash(value: &str) -> std::result::Result<String, String> {
    let (hash, remainder) = value
        .split_once('|')
        .ok_or_else(|| "hashline body must have the form '${hash}|'".to_string())?;
    let hash = hash.trim();
    if hash.is_empty() || !remainder.trim().is_empty() || hash.chars().any(char::is_whitespace) {
        return Err("hashline body must have the form '${hash}|'".into());
    }
    Ok(hash.to_string())
}

fn find_unique_match(
    lines: &[hashline::HashLine],
    pattern: &[&str],
    line_hint: Option<usize>,
) -> std::result::Result<usize, ApplyError> {
    if pattern.is_empty() {
        return line_hint.map(|line| line.saturating_sub(1)).ok_or_else(|| {
            ApplyError::Failure(
                "EDIT requires hashline context or an @@<line> insertion hint".into(),
            )
        });
    }
    if pattern.len() > lines.len() {
        return Err(ApplyError::Failure(
            "stale patch: no matching hashline context".into(),
        ));
    }
    let mut matches = Vec::new();
    for start in 0..=lines.len() - pattern.len() {
        if lines[start..start + pattern.len()]
            .iter()
            .map(|line| line.tag.as_str())
            .eq(pattern.iter().copied())
        {
            matches.push(start);
        }
    }
    if let Some(line_hint) = line_hint {
        matches.retain(|start| *start + 1 == line_hint);
    }
    match matches.as_slice() {
        [start] => Ok(*start),
        [] => Err(ApplyError::Failure(
            "stale patch: no matching hashline context".into(),
        )),
        _ => Err(ApplyError::Failure(
            "ambiguous patch: multiple hashline matches; add @@<line> to the header".into(),
        )),
    }
}

fn replacement_text(
    body: &[BodyLine],
    lines: &[hashline::HashLine],
    start: usize,
) -> std::result::Result<String, ApplyError> {
    let mut old_offset = 0;
    let mut replacement = Vec::new();
    for line in body {
        match line {
            BodyLine::Context(_) => {
                let value = lines.get(start + old_offset).ok_or_else(|| {
                    ApplyError::Failure("hashline context is out of bounds".into())
                })?;
                replacement.push(value.text.clone());
                old_offset += 1;
            }
            BodyLine::Delete(_) => old_offset += 1,
            BodyLine::Add(value) => replacement.push(value.clone()),
        }
    }
    Ok(replacement.join("\n"))
}

fn added_text(body: &[BodyLine]) -> String {
    let mut lines = body.iter().filter_map(|line| match line {
        BodyLine::Add(value) => Some(value.as_str()),
        BodyLine::Context(_) | BodyLine::Delete(_) => None,
    });
    let Some(first) = lines.next() else {
        return String::new();
    };
    let mut text = first.to_string();
    for line in lines {
        text.push('\n');
        text.push_str(line);
    }
    text.push('\n');
    text
}

fn insert_lines(text: &str, line: usize, replacement: &str) -> Option<String> {
    let mut lines = hashline::split_lines_preserving_endings(text);
    if line == 0 || line > lines.len() + 1 {
        return None;
    }
    let mut replacement_lines = hashline::split_lines_preserving_endings(replacement);
    if !replacement_lines.is_empty()
        && line <= lines.len()
        && lines[line - 1].ends_with('\n')
        && !replacement_lines
            .last()
            .is_some_and(|value| value.ends_with('\n'))
    {
        replacement_lines.last_mut()?.push('\n');
    }
    lines.splice(line - 1..line - 1, replacement_lines);
    Some(lines.concat())
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
    pub fn new(tool: Arc<ToolPatch>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
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
