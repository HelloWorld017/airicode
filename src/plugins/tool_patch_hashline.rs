use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::Arc,
};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        hooks::{
            BuildFileContextHook, BuildFileContextHookContext, ConfigReadContext, ConfigReadHook,
        },
        models::{
            FileContext, NoteContent, Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId,
            ToolInput, ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::{hashline, note::add_tool_note, schema::json_schema},
};

const PATCH_FORMAT: &str = r#"Expected format:
REPLACE path FROM line:hash TO line:hash <<<TAG
content
TAG
INSERT path BEFORE line:hash <<<TAG
content
TAG
INSERT path AFTER line:hash <<<TAG
content
TAG
DELETE path FROM line:hash TO line:hash"#;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PatchHashlineOperation {
    InsertBefore {
        path: String,
        anchor: String,
        content: String,
    },
    InsertAfter {
        path: String,
        anchor: String,
        content: String,
    },
    Replace {
        path: String,
        anchor_start: String,
        anchor_end: String,
        content: String,
    },
    Delete {
        path: String,
        anchor_start: String,
        anchor_end: String,
    },
}

struct HashlineFileContextHook;

#[async_trait]
impl BuildFileContextHook for HashlineFileContextHook {
    async fn augment_file_context(
        &self,
        context: BuildFileContextHookContext,
        file_context: &mut FileContext,
    ) -> Result<()> {
        for (line, rendered) in file_context
            .lines
            .iter_mut()
            .zip(hashline::render(&context.source))
        {
            line.display_prefix = format!("{}:{}", rendered.line, rendered.tag);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
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

pub struct ToolPatchHashline {
    id: ToolId,
    max_bytes: usize,
}

impl ToolPatchHashline {
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

impl Default for ToolPatchHashline {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolPatchHashline {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "patch_hashline".into(),
            description: include_str!("../prompts/tool_patch_hashline.txt").into(),
            input: ToolInputDefinition::new(json_schema::<PatchHashlineOperation>()),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let input: PatchHashlineOperation = match serde_json::from_value(input) {
            Ok(input) => input,
            Err(error) => {
                let message = format!("invalid patch_hashline input: {error}");
                let output = ToolOutput::Failure {
                    content: syntax_error(&message),
                };
                add_tool_note(
                    &context,
                    NoteContent::Alert {
                        content: format!("Patch failed: {message}"),
                    },
                    "patch_hashline",
                )
                .await?;
                return Ok(output);
            }
        };
        let operation = match operation_to_domain(&input) {
            Ok(operation) => operation,
            Err(message) => {
                let output = ToolOutput::Failure {
                    content: syntax_error(&message),
                };
                add_tool_note(
                    &context,
                    NoteContent::Alert {
                        content: format!("Patch failed: {}", message),
                    },
                    "patch_hashline",
                )
                .await?;
                return Ok(output);
            }
        };
        let operations = vec![operation];

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
            let result = context
                .operations
                .workdir()?
                .write(
                    Path::new(path),
                    plan.content.as_deref().unwrap_or_default().as_bytes(),
                )
                .await;
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
                    OperationKind::Delete
                        | OperationKind::Replace
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

        let applied_plan_by_path = outcomes.iter().filter(|outcome| outcome.applied).fold(
            HashMap::new(),
            |mut acc, outcome| match plans.get(&outcome.operation.path) {
                Some(plan) => {
                    acc.entry(outcome.operation.path.clone()).or_insert(plan);
                    acc
                }
                _ => acc,
            },
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
                    [("tool".into(), Value::String("patch_hashline".into()))],
                )
                .await?;
        }

        if applied_count < outcomes.len() {
            let failed_summary = outcomes.iter().filter(|outcome| !outcome.applied).fold(
                "Patch failed:\n".to_string(),
                |mut body, outcome| {
                    let reason = match outcome.failure.as_ref() {
                        Some(failure) => &format!(": {}", failure),
                        None => "",
                    };

                    body.push_str(&outcome.operation.path);
                    body.push_str(reason);
                    body.push('\n');
                    body
                },
            );

            add_tool_note(
                &context,
                NoteContent::Alert {
                    content: failed_summary,
                },
                "patch_hashline",
            )
            .await?;

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
    let workdir = context.operations.workdir()?;
    let exists = match workdir.exists(Path::new(path)).await {
        Ok(exists) => exists,
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(error) => {
            return Ok(FileSnapshot {
                exists: false,
                content: None,
                lines: Vec::new(),
                error: Some(error.to_string()),
            });
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
    let bytes = match workdir.read(Path::new(path)).await {
        Ok(bytes) => bytes,
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(error) => {
            return Ok(FileSnapshot {
                exists: true,
                content: None,
                lines: Vec::new(),
                error: Some(error.to_string()),
            });
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
            });
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
    _max_bytes: usize,
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
        OperationKind::Delete | OperationKind::Replace => {
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
                            outcome.failure = Some(format!(
                                "{} does not change the file",
                                if outcome.operation.kind == OperationKind::Delete {
                                    "DELETE"
                                } else {
                                    "REPLACE"
                                }
                            ));
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
                _ => true,
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
        let old_content = snapshot.content.clone();
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
        if content.len() > max_bytes {
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
                content: Some(content),
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
        OperationKind::Delete => LineEdit {
            start: offsets[start].0,
            end: offsets[end].1,
            replacement: String::new(),
        },
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
        OperationKind::Delete
            | OperationKind::Replace
            | OperationKind::InsertBefore
            | OperationKind::InsertAfter
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
        OperationKind::Delete => format!(
            "DELETE {} FROM {} TO {}",
            operation.path,
            format_anchor(operation.start.as_ref().expect("DELETE start anchor")),
            format_anchor(operation.end.as_ref().expect("DELETE end anchor")),
        ),
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

fn operation_to_domain(
    operation: &PatchHashlineOperation,
) -> std::result::Result<Operation, String> {
    match operation {
        PatchHashlineOperation::InsertBefore {
            path,
            anchor,
            content,
        } => Ok(Operation {
            kind: OperationKind::InsertBefore,
            path: parse_path(path)?,
            start: Some(parse_anchor(anchor)?),
            end: None,
            body: content.clone(),
        }),
        PatchHashlineOperation::InsertAfter {
            path,
            anchor,
            content,
        } => Ok(Operation {
            kind: OperationKind::InsertAfter,
            path: parse_path(path)?,
            start: Some(parse_anchor(anchor)?),
            end: None,
            body: content.clone(),
        }),
        PatchHashlineOperation::Replace {
            path,
            anchor_start,
            anchor_end,
            content,
        } => Ok(Operation {
            kind: OperationKind::Replace,
            path: parse_path(path)?,
            start: Some(parse_anchor(anchor_start)?),
            end: Some(parse_anchor(anchor_end)?),
            body: content.clone(),
        }),
        PatchHashlineOperation::Delete {
            path,
            anchor_start,
            anchor_end,
        } => Ok(Operation {
            kind: OperationKind::Delete,
            path: parse_path(path)?,
            start: Some(parse_anchor(anchor_start)?),
            end: Some(parse_anchor(anchor_end)?),
            body: String::new(),
        }),
    }
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

pub struct ToolPatchHashlinePlugin {
    id: PluginId,
}

impl ToolPatchHashlinePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
        }
    }
}

#[async_trait]
impl Plugin for ToolPatchHashlinePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_patch_hashline"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        let hook: Arc<dyn ConfigReadHook> = self;
        registry.register_hook(hook)
    }
}

#[async_trait]
impl ConfigReadHook for ToolPatchHashlinePlugin {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()> {
        if context.config.tool.enable_hashline {
            let hook: Arc<dyn BuildFileContextHook> = Arc::new(HashlineFileContextHook);
            context.registry.register_hook(hook)?;
            context
                .registry
                .register_tool(Arc::new(ToolPatchHashline::new()), 0)
                .map(|_| ())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_uses_hashline_patch_prompt() {
        let definition = ToolPatchHashline::new().definition();

        assert_eq!(
            definition.description,
            include_str!("../prompts/tool_patch_hashline.txt")
        );
    }

    #[test]
    fn definition_generates_a_tagged_operation_schema() {
        let schema = ToolPatchHashline::new()
            .definition()
            .input
            .schema
            .to_string();

        assert!(schema.contains("oneOf"));
        assert!(schema.contains("insert_before"));
        assert!(schema.contains("insert_after"));
        assert!(schema.contains("anchor_start"));
        assert!(schema.contains("anchor_end"));
        assert!(schema.contains("content"));
        assert!(!schema.contains("operations"));
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
