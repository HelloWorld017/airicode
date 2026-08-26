use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::Arc,
};

use async_trait::async_trait;
use serde_json::Value;

use crate::plugins::add_tool_note;
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

const PATCH_FORMAT: &str = r#"Expected format:
ADD path <<<TAG
content
TAG
REPLACE path FROM line:hash TO line:hash <<<TAG
content
TAG
INSERT path BEFORE line:hash <<<TAG
content
TAG
INSERT path AFTER line:hash <<<TAG
content
TAG
DELETE path"#;

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

#[derive(Clone, Debug)]
struct LineEdit {
    start: usize,
    end: usize,
    replacement: String,
}

struct FileSnapshot {
    exists: bool,
    content: Option<String>,
    lines: Vec<hashline::HashLine>,
    error: Option<String>,
}

struct OperationOutcome {
    operation: Operation,
    edit: Option<LineEdit>,
    failure: Option<String>,
    applied: bool,
    final_range: Option<(usize, usize)>,
}

struct FilePlan {
    content: Option<String>,
    old_content: Option<String>,
    output_ranges: HashMap<usize, (usize, usize)>,
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
            Err(message) => {
                let output = ToolOutput::Failure {
                    content: syntax_error(&message),
                };
                add_tool_note(
                    &context,
                    NoteContent::Alert {
                        content: format!("Patch failed: {}", message),
                    },
                    "patch",
                )
                .await?;
                return Ok(output);
            }
        };
        if operations.is_empty() {
            let output = ToolOutput::Failure {
                content: syntax_error("patch requires at least one operation"),
            };
            add_tool_note(
                &context,
                NoteContent::Alert {
                    content: format!("Patch failed: {}", output.content().unwrap_or_default()),
                },
                "patch",
            )
            .await?;
            return Ok(output);
        }

        // Read every path before resolving any anchor or writing any result.
        let snapshots = collect_snapshots(&operations, &context).await?;
        let mut outcomes = operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| {
                prepare_operation(index, operation, &snapshots, self.max_bytes)
            })
            .collect::<Vec<_>>();
        detect_conflicts(&mut outcomes);
        let mut plans = build_file_plans(&mut outcomes, &snapshots, self.max_bytes)?;

        for (path, plan) in plans.iter_mut() {
            if context.cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let result = match plan.content.as_deref() {
                Some(content) => {
                    context
                        .workdir
                        .write(Path::new(path), content.as_bytes())
                        .await
                }
                None => context.workdir.remove(Path::new(path)).await,
            };
            if let Err(error) = result {
                if matches!(error, Error::Cancelled) {
                    return Err(error);
                }
                let reason = error.to_string();
                for outcome in outcomes.iter_mut() {
                    if outcome.operation.path == *path && outcome.failure.is_none() {
                        outcome.failure =
                            Some(format!("could not write operation result: {reason}"));
                        outcome.final_range = None;
                        outcome.applied = false;
                    }
                }
                continue;
            }
            for (index, outcome) in outcomes.iter_mut().enumerate() {
                if outcome.operation.path == *path && outcome.failure.is_none() {
                    outcome.applied = true;
                    outcome.final_range = plan.output_ranges.get(&index).copied();
                }
            }
        }

        let applied_count = outcomes.iter().filter(|outcome| outcome.applied).count();
        let mut result = String::new();
        result.push_str(&patch_header(applied_count, outcomes.len()));

        for (index, outcome) in outcomes.iter().enumerate() {
            result.push_str(&format!(
                "\n\n[{}] {} \"{}\"",
                index + 1,
                if outcome.applied { "APPLIED" } else { "FAILED" },
                operation_display(&outcome.operation)
            ));
            if outcome.applied {
                if matches!(
                    outcome.operation.kind,
                    OperationKind::Replace
                        | OperationKind::InsertBefore
                        | OperationKind::InsertAfter
                ) {
                    if let Some(plan) = plans.get(&outcome.operation.path) {
                        if let (Some(content), Some(range)) =
                            (plan.content.as_deref(), outcome.final_range)
                        {
                            result.push_str("\nUpdated file:\n");
                            result.push_str(&render_updated_region(content, range));
                        }
                    }
                }
            } else {
                let reason = outcome
                    .failure
                    .as_deref()
                    .unwrap_or("operation could not be applied");
                result.push_str(&format!("\nReason: {reason}"));
                if is_anchored(&outcome.operation) {
                    if let Some(snapshot) = snapshots.get(&outcome.operation.path) {
                        result.push_str("\nCurrent file:\n");
                        if snapshot.content.is_some() {
                            result.push_str(&surrounding_context(
                                &snapshot.lines,
                                operation_anchors(&outcome.operation),
                            ));
                        } else if snapshot.exists {
                            result.push_str("(file contents are unavailable)");
                        } else {
                            result.push_str("(file does not exist in the original snapshot)");
                        }
                    }
                }
            }
        }

        let applied_plan_by_path = outcomes
            .iter()
            .filter(|outcome| outcome.applied)
            .fold(
                HashMap::new(),
                |mut acc, outcome| match plans.get(&outcome.operation.path) {
                    Some(plan) => {
                        acc.entry(outcome.operation.path.clone()).or_insert(plan);
                        acc
                    },
                    _ => acc
                }
            );

        for (path, plan) in applied_plan_by_path.iter() {
            let diff = unified_diff(
                path,
                plan.old_content.as_deref().unwrap_or_default(),
                plan.content.as_deref().unwrap_or_default(),
            );
            context
                .operations
                .add_note(
                    NoteContent::Diff {
                        file: path.clone(),
                        content: diff,
                    },
                    [
                        ("tool".into(), Value::String("patch".into())),
                    ],
                )
                .await?;
        }

        if applied_count < outcomes.len() {
            let failed_summary = outcomes
                .iter()
                .filter(|outcome| !outcome.applied)
                .fold(
                    "Patch failed:\n".to_string(),
                    |mut body, outcome| {
                        let reason = match outcome.failure.as_ref() {
                            Some(failure) => &format!(": {}", failure),
                            None => ""
                        };

                        body.push_str(&outcome.operation.path);
                        body.push_str(reason);
                        body.push('\n');
                        body
                    }
                );

            add_tool_note(
                &context,
                NoteContent::Alert {
                    content: failed_summary,
                },
                "patch",
            ).await?;
        }

        if applied_count == 0 {
            Ok(ToolOutput::Failure { content: result })
        } else {
            Ok(ToolOutput::Success { content: result })
        }
    }
}

fn syntax_error(message: &str) -> String {
    format!("Patch syntax error: {message}\n\n{PATCH_FORMAT}")
}

async fn collect_snapshots(
    operations: &[Operation],
    context: &ToolContext,
) -> Result<HashMap<String, FileSnapshot>> {
    let paths = operations
        .iter()
        .map(|operation| operation.path.clone())
        .collect::<BTreeSet<_>>();
    let mut snapshots = HashMap::with_capacity(paths.len());
    for path in paths {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        snapshots.insert(path.clone(), snapshot_file(context, &path).await?);
    }
    Ok(snapshots)
}

async fn snapshot_file(context: &ToolContext, path: &str) -> Result<FileSnapshot> {
    let exists = match context.workdir.exists(Path::new(path)).await {
        Ok(exists) => exists,
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(error) => {
            return Ok(FileSnapshot {
                exists: false,
                content: None,
                lines: Vec::new(),
                error: Some(error.to_string()),
            })
        }
    };
    if !exists {
        return Ok(FileSnapshot {
            exists: false,
            content: None,
            lines: Vec::new(),
            error: None,
        });
    }
    let bytes = match context.workdir.read(Path::new(path)).await {
        Ok(bytes) => bytes,
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(error) => {
            return Ok(FileSnapshot {
                exists: true,
                content: None,
                lines: Vec::new(),
                error: Some(error.to_string()),
            })
        }
    };
    if bytes.contains(&0) {
        return Ok(FileSnapshot {
            exists: true,
            content: None,
            lines: Vec::new(),
            error: Some("cannot patch binary/NUL-containing input".into()),
        });
    }
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(_) => {
            return Ok(FileSnapshot {
                exists: true,
                content: None,
                lines: Vec::new(),
                error: Some("cannot patch non-UTF-8 input".into()),
            })
        }
    };
    let lines = hashline::render(&content);
    Ok(FileSnapshot {
        exists: true,
        content: Some(content),
        lines,
        error: None,
    })
}

fn prepare_operation(
    _index: usize,
    operation: Operation,
    snapshots: &HashMap<String, FileSnapshot>,
    max_bytes: usize,
) -> OperationOutcome {
    let mut outcome = OperationOutcome {
        operation,
        edit: None,
        failure: None,
        applied: false,
        final_range: None,
    };
    let snapshot = snapshots
        .get(&outcome.operation.path)
        .expect("all operation paths have a snapshot");
    if let Some(error) = snapshot.error.as_deref() {
        outcome.failure = Some(if is_anchored(&outcome.operation) {
            format!("{error}; {}", unavailable_anchor_reason(&outcome.operation))
        } else {
            error.into()
        });
        return outcome;
    }

    match outcome.operation.kind {
        OperationKind::Add => {
            if snapshot.exists {
                outcome.failure = Some(format!(
                    "cannot add existing file: {}",
                    outcome.operation.path
                ));
            } else if outcome.operation.body.len() > max_bytes {
                outcome.failure = Some(format!("patched file exceeds limit of {max_bytes} bytes"));
            }
        }
        OperationKind::Delete => {
            if !snapshot.exists {
                outcome.failure = Some(format!(
                    "cannot delete missing file: {}",
                    outcome.operation.path
                ));
            }
        }
        OperationKind::Replace => {
            let anchors = operation_anchors(&outcome.operation);
            if !snapshot.exists {
                outcome.failure = Some(format!(
                    "{} File does not exist in the original snapshot.",
                    unavailable_anchor_reason(&outcome.operation)
                ));
            } else {
                let start = resolve_anchor(&snapshot.lines, anchors[0]);
                let end = resolve_anchor(&snapshot.lines, anchors[1]);
                let mut reasons = Vec::new();
                if let Err(error) = &start {
                    reasons.push(anchor_reason(
                        if anchors[0] == anchors[1] {
                            "Anchor"
                        } else {
                            "Start"
                        },
                        anchors[0],
                        error,
                    ));
                }
                if anchors[0] != anchors[1] {
                    if let Err(error) = &end {
                        reasons.push(anchor_reason("End", anchors[1], error));
                    }
                }
                if !reasons.is_empty() {
                    outcome.failure = Some(reasons.join(" "));
                } else {
                    let start = start.expect("checked above");
                    let end = end.expect("checked above");
                    if start > end {
                        outcome.failure =
                            Some("start anchor must not come after end anchor".into());
                    } else {
                        let edit = line_edit(
                            snapshot.content.as_deref().unwrap_or_default(),
                            outcome.operation.kind,
                            start,
                            end,
                            &outcome.operation.body,
                        );
                        if edit.replacement
                            == source_slice(
                                snapshot.content.as_deref().unwrap_or_default(),
                                edit.start,
                                edit.end,
                            )
                        {
                            outcome.failure = Some("REPLACE does not change the file".into());
                        } else {
                            outcome.edit = Some(edit);
                        }
                    }
                }
            }
        }
        OperationKind::InsertBefore | OperationKind::InsertAfter => {
            let anchors = operation_anchors(&outcome.operation);
            if !snapshot.exists {
                outcome.failure = Some(format!(
                    "{} File does not exist in the original snapshot.",
                    unavailable_anchor_reason(&outcome.operation)
                ));
            } else {
                match resolve_anchor(&snapshot.lines, anchors[0]) {
                    Ok(index) => {
                        let edit = line_edit(
                            snapshot.content.as_deref().unwrap_or_default(),
                            outcome.operation.kind,
                            index,
                            index,
                            &outcome.operation.body,
                        );
                        if edit.replacement.is_empty() {
                            outcome.failure = Some("INSERT does not change the file".into());
                        } else {
                            outcome.edit = Some(edit);
                        }
                    }
                    Err(error) => {
                        outcome.failure = Some(anchor_reason("Anchor", anchors[0], &error));
                    }
                }
            }
        }
    }
    outcome
}

fn detect_conflicts(outcomes: &mut [OperationOutcome]) {
    for current in 0..outcomes.len() {
        if outcomes[current].failure.is_some() {
            continue;
        }
        for previous in 0..current {
            if outcomes[previous].failure.is_some()
                || outcomes[previous].operation.path != outcomes[current].operation.path
            {
                continue;
            }
            let conflict = match (&outcomes[previous].edit, &outcomes[current].edit) {
                (Some(previous), Some(current)) => edits_conflict(previous, current),
                _ => match (outcomes[previous].operation.kind, outcomes[current].operation.kind) {
                    (OperationKind::Delete, OperationKind::Add) => false,
                    _ => true,
                }
            };
            if conflict {
                outcomes[current].failure = Some(format!(
                    "conflicts with operation [{}] {}",
                    previous + 1,
                    operation_display(&outcomes[previous].operation)
                ));
                break;
            }
        }
    }
}

fn edits_conflict(left: &LineEdit, right: &LineEdit) -> bool {
    if left.start == left.end && right.start == right.end {
        return false;
    }
    if left.start == left.end {
        return left.start > right.start && left.start < right.end;
    }
    if right.start == right.end {
        return right.start > left.start && right.start < left.end;
    }
    left.start < right.end && right.start < left.end
}

fn build_file_plans(
    outcomes: &mut [OperationOutcome],
    snapshots: &HashMap<String, FileSnapshot>,
    max_bytes: usize,
) -> Result<HashMap<String, FilePlan>> {
    let paths = outcomes
        .iter()
        .filter(|outcome| outcome.failure.is_none())
        .map(|outcome| outcome.operation.path.clone())
        .collect::<BTreeSet<_>>();
    let mut plans = HashMap::new();
    for path in paths {
        let indexes = outcomes
            .iter()
            .enumerate()
            .filter(|(_, outcome)| outcome.failure.is_none() && outcome.operation.path == path)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let snapshot = snapshots
            .get(&path)
            .expect("all operation paths have a snapshot");
        let first = indexes[0];
        let first_kind = outcomes[first].operation.kind;
        let old_content = snapshot.content.clone();
        let (content, output_ranges) = match first_kind {
            OperationKind::Add => (Some(outcomes[first].operation.body.clone()), HashMap::new()),
            OperationKind::Delete => (None, HashMap::new()),
            OperationKind::Replace | OperationKind::InsertBefore | OperationKind::InsertAfter => {
                let source = snapshot.content.as_deref().unwrap_or_default();
                let mut edits = indexes
                    .iter()
                    .map(|index| {
                        (
                            *index,
                            outcomes[*index]
                                .edit
                                .clone()
                                .expect("valid edit operation has an edit"),
                        )
                    })
                    .collect::<Vec<_>>();
                edits.sort_by(|(left_index, left), (right_index, right)| {
                    left.start
                        .cmp(&right.start)
                        .then_with(|| (left.start != left.end).cmp(&(right.start != right.end)))
                        .then(left_index.cmp(right_index))
                });
                let (content, output_ranges) = apply_edits(source, &edits);
                (Some(content), output_ranges)
            }
        };
        if content
            .as_deref()
            .is_some_and(|content| content.len() > max_bytes)
        {
            for index in indexes {
                outcomes[index].failure =
                    Some(format!("patched file exceeds limit of {max_bytes} bytes"));
            }
            continue;
        }
        for (index, range) in &output_ranges {
            outcomes[*index].final_range = Some(*range);
        }
        plans.insert(
            path,
            FilePlan {
                content,
                old_content,
                output_ranges,
            },
        );
    }
    Ok(plans)
}

fn apply_edits(
    source: &str,
    edits: &[(usize, LineEdit)],
) -> (String, HashMap<usize, (usize, usize)>) {
    let mut result = String::new();
    let mut ranges = HashMap::new();
    let mut cursor = 0;
    for (index, edit) in edits {
        result.push_str(&source[cursor..edit.start]);
        let start = result.len();
        result.push_str(&edit.replacement);
        let end = result.len();
        ranges.insert(*index, (start, end));
        cursor = edit.end;
    }
    result.push_str(&source[cursor..]);
    (result, ranges)
}

fn line_edit(source: &str, kind: OperationKind, start: usize, end: usize, body: &str) -> LineEdit {
    let lines = hashline::split_lines_preserving_endings(source);
    let offsets = line_offsets(&lines);
    match kind {
        OperationKind::Replace => LineEdit {
            start: offsets[start].0,
            end: offsets[end].1,
            replacement: normalized_replacement(&lines[end], body),
        },
        OperationKind::InsertBefore | OperationKind::InsertAfter => {
            let split = if kind == OperationKind::InsertBefore {
                start
            } else {
                start + 1
            };
            let before = lines[..split].concat();
            let after = lines[split..].concat();
            let mut replacement = String::new();
            if !body.is_empty() {
                if !before.is_empty() && !before.ends_with('\n') {
                    replacement.push('\n');
                }
                replacement.push_str(body);
                if !after.is_empty() && !replacement.ends_with('\n') {
                    replacement.push('\n');
                }
            }
            let offset = offsets
                .get(split)
                .map(|(start, _)| *start)
                .unwrap_or_else(|| source.len());
            LineEdit {
                start: offset,
                end: offset,
                replacement,
            }
        }
        OperationKind::Add | OperationKind::Delete => unreachable!(),
    }
}

fn line_offsets(lines: &[String]) -> Vec<(usize, usize)> {
    let mut offset = 0;
    lines
        .iter()
        .map(|line| {
            let range = (offset, offset + line.len());
            offset += line.len();
            range
        })
        .collect()
}

fn normalized_replacement(last_source_line: &str, body: &str) -> String {
    let mut lines = hashline::split_lines_preserving_endings(body);
    if last_source_line.ends_with('\n')
        && !body.is_empty()
        && lines.last().is_some_and(|line| !line.ends_with('\n'))
    {
        if let Some(line) = lines.last_mut() {
            line.push('\n');
        }
    }
    lines.concat()
}

#[derive(Debug)]
enum AnchorError {
    Stale,
    Ambiguous,
}

fn resolve_anchor(
    lines: &[hashline::HashLine],
    anchor: &hashline::Anchor,
) -> std::result::Result<usize, AnchorError> {
    let matches = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line.tag == anchor.tag).then_some(index))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(AnchorError::Stale),
        _ => {
            let anchored_matches = matches
                .into_iter()
                .filter(|index| lines[*index].line == anchor.line)
                .collect::<Vec<_>>();
            match anchored_matches.as_slice() {
                [index] => Ok(*index),
                [] => Err(AnchorError::Stale),
                _ => Err(AnchorError::Ambiguous),
            }
        }
    }
}

fn anchor_reason(label: &str, anchor: &hashline::Anchor, error: &AnchorError) -> String {
    match error {
        AnchorError::Stale if label == "Anchor" => {
            format!("Anchor is stale ({}:{}).", anchor.line, anchor.tag)
        }
        AnchorError::Stale => format!("{label} anchor is stale ({}:{}).", anchor.line, anchor.tag),
        AnchorError::Ambiguous if label == "Anchor" => {
            format!("Anchor is ambiguous ({}:{}).", anchor.line, anchor.tag)
        }
        AnchorError::Ambiguous => format!(
            "{label} anchor is ambiguous ({}:{}).",
            anchor.line, anchor.tag
        ),
    }
}

fn operation_anchors(operation: &Operation) -> Vec<&hashline::Anchor> {
    operation.start.iter().chain(operation.end.iter()).collect()
}

fn is_anchored(operation: &Operation) -> bool {
    matches!(
        operation.kind,
        OperationKind::Replace | OperationKind::InsertBefore | OperationKind::InsertAfter
    )
}

fn unavailable_anchor_reason(operation: &Operation) -> String {
    let anchors = operation_anchors(operation);
    if anchors.len() == 1 {
        return anchor_reason("Anchor", anchors[0], &AnchorError::Stale);
    }
    let mut reasons = Vec::new();
    reasons.push(anchor_reason(
        if anchors[0] == anchors[1] {
            "Anchor"
        } else {
            "Start"
        },
        anchors[0],
        &AnchorError::Stale,
    ));
    if anchors[0] != anchors[1] {
        reasons.push(anchor_reason("End", anchors[1], &AnchorError::Stale));
    }
    reasons.join(" ")
}

fn source_slice(source: &str, start: usize, end: usize) -> String {
    source[start..end].to_string()
}

fn surrounding_context(lines: &[hashline::HashLine], anchors: Vec<&hashline::Anchor>) -> String {
    if lines.is_empty() {
        return "(file is empty)".into();
    }
    let mut windows = anchors
        .into_iter()
        .map(|anchor| {
            let line = anchor.line.clamp(1, lines.len());
            (
                line.saturating_sub(1).max(1),
                line.saturating_add(1).min(lines.len()),
            )
        })
        .collect::<Vec<_>>();
    windows.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for window in windows {
        if let Some(last) = merged.last_mut() {
            if window.0 <= last.1.saturating_add(1) {
                last.1 = last.1.max(window.1);
                continue;
            }
        }
        merged.push(window);
    }
    let mut output = Vec::new();
    for (index, (start, end)) in merged.iter().enumerate() {
        if index > 0 {
            let previous_end = merged[index - 1].1;
            output.push(format!(
                "... {} line(s) omitted ...",
                start.saturating_sub(previous_end + 1)
            ));
        }
        output.extend(lines[start - 1..*end].iter().map(format_hash_line));
    }
    output.join("\n")
}

fn render_updated_region(source: &str, byte_range: (usize, usize)) -> String {
    let lines = hashline::render(source);
    if lines.is_empty() {
        return "(file is empty)".into();
    }
    let start_line = source[..byte_range.0]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(1)
        .min(lines.len());
    let end_prefix = &source[..byte_range.1];
    let end_line = end_prefix
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(if end_prefix.ends_with('\n') { 0 } else { 1 })
        .max(start_line)
        .min(lines.len());
    let changed_count = end_line - start_line + 1;
    let mut output = Vec::new();
    if changed_count > 6 {
        if start_line > 1 {
            output.push(format_hash_line(&lines[start_line - 2]));
        }
        output.extend(
            lines[start_line - 1..start_line + 2]
                .iter()
                .map(format_hash_line),
        );
        output.push(format!("... {} line(s) omitted ...", changed_count - 6));
        output.extend(lines[end_line - 3..end_line].iter().map(format_hash_line));
        if end_line < lines.len() {
            output.push(format_hash_line(&lines[end_line]));
        }
    } else {
        let start = start_line.saturating_sub(2);
        let end = (end_line + 1).min(lines.len());
        output.extend(lines[start..end].iter().map(format_hash_line));
    }
    output.join("\n")
}

fn format_hash_line(line: &hashline::HashLine) -> String {
    format!("{}:{}|{}", line.line, line.tag, line.text)
}

fn patch_header(applied: usize, total: usize) -> String {
    if applied == total {
        format!("Success: Applied {total} operations.")
    } else if applied == 0 {
        format!("Failure: Applied 0 of {total} operations.")
    } else {
        format!("Partial Failure: Applied {applied} of {total} operations.")
    }
}

fn operation_display(operation: &Operation) -> String {
    match operation.kind {
        OperationKind::Add => format!("ADD {}", operation.path),
        OperationKind::Delete => format!("DELETE {}", operation.path),
        OperationKind::Replace => format!(
            "REPLACE {} FROM {} TO {}",
            operation.path,
            format_anchor(operation.start.as_ref().expect("REPLACE start anchor")),
            format_anchor(operation.end.as_ref().expect("REPLACE end anchor")),
        ),
        OperationKind::InsertBefore => format!(
            "INSERT {} BEFORE {}",
            operation.path,
            format_anchor(operation.start.as_ref().expect("INSERT anchor")),
        ),
        OperationKind::InsertAfter => format!(
            "INSERT {} AFTER {}",
            operation.path,
            format_anchor(operation.start.as_ref().expect("INSERT anchor")),
        ),
    }
}

fn format_anchor(anchor: &hashline::Anchor) -> String {
    format!("{}:{}", anchor.line, anchor.tag)
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

struct OperationHeader {
    kind: OperationKind,
    path: String,
    start: Option<hashline::Anchor>,
    end: Option<hashline::Anchor>,
    delimiter: Option<String>,
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

    #[test]
    fn failed_context_merges_adjacent_anchor_windows() {
        let lines = hashline::render("one\ntwo\nthree\nfour\nfive\nsix\n");
        let first = hashline::Anchor {
            line: 2,
            tag: "bad".into(),
        };
        let second = hashline::Anchor {
            line: 4,
            tag: "bad".into(),
        };
        let anchors = vec![&first, &second];
        let context = surrounding_context(&lines, anchors);
        assert!(!context.contains("omitted"));
        assert!(context.contains("1:"));
        assert!(context.contains("5:"));
    }
}
