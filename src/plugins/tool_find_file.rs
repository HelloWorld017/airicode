use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::add_output_note;
use crate::core::{
    error::{Error, Result},
    models::{
        Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
        ToolInputDefinition, ToolOutput, WorkdirEntryKind,
    },
    registry::PluginRegistryScope,
};

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FindFileQuery {
    ByFilenameKeyword { keyword: String },
    ByFilenameExact { filename: String },
    ByGlobPattern { pattern: String },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct FindFileInputSchema {
    query: FindFileQuery,
    path: Option<String>,
    max_results: Option<usize>,
}

pub struct ToolFindFile {
    id: ToolId,
    max_output_bytes: usize,
    max_results: usize,
}

impl ToolFindFile {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_output_bytes: 128 * 1024,
            max_results: 500,
        }
    }

    pub fn with_limits(mut self, max_results: usize, max_output_bytes: usize) -> Self {
        self.max_results = max_results;
        self.max_output_bytes = max_output_bytes;
        self
    }
}

impl Default for ToolFindFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolFindFile {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "find_file".into(),
            description: "Find files using exactly one query: a case-insensitive basename keyword, an exact basename, or a filesystem-relative glob pattern. `path` optionally limits the search root and `max_results` limits output.".into(),
            input: ToolInputDefinition::JsonSchema(
                crate::utils::schema::json_schema::<FindFileInputSchema>(),
            ),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Json(input) = input else {
            return Err(Error::Tool("find_file input must be an object".into()));
        };
        let input = parse_input(input).map_err(|error| {
            Error::Tool(format!(
                "find_file input is invalid: {error}. Expected an object with exactly one query variant: by_filename_keyword, by_filename_exact, or by_glob_pattern."
            ))
        })?;
        let path = input
            .path
            .as_deref()
            .filter(|path| !path.is_empty())
            .unwrap_or(".");
        let max_results = input
            .max_results
            .unwrap_or(self.max_results)
            .min(self.max_results);

        let matching = match collect_matching_files(&context, Path::new(path), &input.query).await {
            Ok(matching) => matching,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::Workdir(message)) => {
                let output = ToolOutput::Failure { content: message };
                add_output_note(&context, "find_file", "Find files failed", &output).await?;
                return Ok(output);
            }
            Err(error) => return Err(error),
        };

        let total = matching.len();
        let mut lines = Vec::new();
        let mut shown = 0;
        let mut output_bytes = 0;
        for path in matching.iter().take(max_results) {
            let separator_bytes = if shown > 0 { 1 } else { 0 };
            if output_bytes + separator_bytes + path.len() > self.max_output_bytes {
                break;
            }
            lines.push(path.as_str());
            output_bytes += separator_bytes + path.len();
            shown += 1;
        }
        let truncation =
            (shown < total).then(|| format!("Showing {shown} of {total} matching files."));
        if total == 0 {
            lines.push("No files matched.");
        } else if let Some(truncation) = truncation.as_deref() {
            lines.push(truncation);
        }
        let output = ToolOutput::Success {
            content: lines.join("\n"),
        };
        add_output_note(
            &context,
            "find_file",
            format!("Found {total} file(s) in {path}"),
            &output,
        )
        .await?;
        Ok(output)
    }
}

fn parse_input(input: Value) -> std::result::Result<FindFileInputSchema, String> {
    let query = input
        .get("query")
        .and_then(Value::as_object)
        .ok_or_else(|| "query must be an object".to_string())?;
    let kind = query
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "query.kind must select exactly one query variant".to_string())?;
    let allowed = match kind {
        "by_filename_keyword" => ["kind", "keyword"].as_slice(),
        "by_filename_exact" => ["kind", "filename"].as_slice(),
        "by_glob_pattern" => ["kind", "pattern"].as_slice(),
        _ => return Err(format!("unknown query kind: {kind}")),
    };
    if let Some(unexpected) = query.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "query variant {kind} cannot contain field `{unexpected}`"
        ));
    }
    let parsed: FindFileInputSchema =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    match &parsed.query {
        FindFileQuery::ByFilenameKeyword { keyword } if keyword.is_empty() => {
            Err("keyword cannot be empty".into())
        }
        FindFileQuery::ByFilenameExact { filename } if filename.is_empty() => {
            Err("filename cannot be empty".into())
        }
        FindFileQuery::ByGlobPattern { pattern } if pattern.is_empty() => {
            Err("pattern cannot be empty".into())
        }
        _ => Ok(parsed),
    }
}

async fn collect_matching_files(
    context: &ToolContext,
    root: &Path,
    query: &FindFileQuery,
) -> Result<Vec<String>> {
    let mut files = Vec::new();
    collect_files(context, root, &mut files).await?;
    files.sort();
    files.retain(|path| matches_query(path, root, query));
    Ok(files)
}

async fn collect_files(context: &ToolContext, path: &Path, files: &mut Vec<String>) -> Result<()> {
    let mut directories = vec![path.to_path_buf()];
    while let Some(directory) = directories.pop() {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entries = context.workdir.list(&directory).await?;
        for entry in entries {
            if entry
                .path
                .components()
                .any(|component| component.as_os_str() == ".git")
            {
                continue;
            }
            match entry.kind {
                WorkdirEntryKind::File => files.push(path_string(&entry.path)),
                WorkdirEntryKind::Directory => directories.push(entry.path),
            }
        }
    }
    Ok(())
}

fn matches_query(path: &str, root: &Path, query: &FindFileQuery) -> bool {
    let basename = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match query {
        FindFileQuery::ByFilenameKeyword { keyword } => {
            basename.to_lowercase().contains(&keyword.to_lowercase())
        }
        FindFileQuery::ByFilenameExact { filename } => basename == filename,
        FindFileQuery::ByGlobPattern { pattern } => {
            let path = path.replace('\\', "/");
            let relative_to_root = path
                .strip_prefix(&format!("{}/", path_string(root)))
                .unwrap_or(&path);
            glob_matches(pattern, &path) || glob_matches(pattern, relative_to_root)
        }
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect::<Vec<_>>();
    let path = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>();
    glob_components_match(&pattern, &path)
}

fn glob_components_match(pattern: &[String], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        glob_components_match(&pattern[1..], path)
            || (!path.is_empty() && glob_components_match(pattern, &path[1..]))
    } else {
        !path.is_empty()
            && segment_matches(&pattern[0], path[0])
            && glob_components_match(&pattern[1..], &path[1..])
    }
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut matches = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    matches[0][0] = true;
    for pattern_index in 0..pattern.len() {
        for value_index in 0..=value.len() {
            if !matches[pattern_index][value_index] {
                continue;
            }
            if pattern[pattern_index] == '*' {
                matches[pattern_index + 1][value_index] = true;
                if value_index < value.len() {
                    matches[pattern_index][value_index + 1] = true;
                }
            } else if value_index < value.len()
                && (pattern[pattern_index] == '?' || pattern[pattern_index] == value[value_index])
            {
                matches[pattern_index + 1][value_index + 1] = true;
            }
        }
    }
    matches[pattern.len()][value.len()]
}

pub struct ToolFindFilePlugin {
    id: PluginId,
    tool: Arc<ToolFindFile>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parser_rejects_multiple_search_methods() {
        let error = parse_input(serde_json::json!({
            "query": {
                "kind": "by_filename_keyword",
                "keyword": "message",
                "filename": "message.rs"
            }
        }))
        .expect_err("query methods must be mutually exclusive");
        assert!(error.contains("cannot contain field `filename`"));
    }

    #[test]
    fn glob_matches_zero_or_more_path_components() {
        assert!(glob_matches("**/AGENTS.md", "AGENTS.md"));
        assert!(glob_matches("src/ui/**/*.rs", "src/ui/button/main.rs"));
        assert!(!glob_matches("src/ui/*.rs", "src/ui/button/main.rs"));
    }

    #[test]
    fn definition_exposes_one_of_query_variants() {
        let ToolInputDefinition::JsonSchema(schema) = ToolFindFile::new().definition().input else {
            panic!("find_file must use a JSON schema")
        };
        fn contains_one_of(value: &Value) -> bool {
            match value {
                Value::Object(object) => {
                    object.contains_key("oneOf") || object.values().any(contains_one_of)
                }
                Value::Array(values) => values.iter().any(contains_one_of),
                _ => false,
            }
        }
        assert!(contains_one_of(&schema));
    }
}

impl ToolFindFilePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolFindFile::new()),
        }
    }

    pub fn tool(&self) -> Arc<ToolFindFile> {
        self.tool.clone()
    }
}

#[async_trait]
impl Plugin for ToolFindFilePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_find_file"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
