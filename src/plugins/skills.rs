use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{
    Context, ContextContributionHook, ContextPart, ContextPriority, ContextSource, Error,
    HookContext, Plugin, PluginId, PluginRegistrar, Result, SessionId, Tool, ToolContext,
    ToolDefinition, ToolId, ToolOutput, Workdir,
};

const PLUGIN_ID: &str = "builtin.skills";
const TOOL_ID: &str = "builtin.skills";
const TOOL_NAME: &str = "skills";
const HOOK_ID: &str = "builtin.skills.context";
const DEFAULT_MAX_SKILL_BYTES: usize = 256 * 1024;
const MAX_METADATA_BYTES: usize = 16 * 1024;
const MAX_SKILL_DIRECTORIES: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkillsConfig {
    pub root: PathBuf,
    pub max_skill_bytes: usize,
}

impl SkillsConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ..Self::default()
        }
    }
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from(".opencode/skills"),
            max_skill_bytes: DEFAULT_MAX_SKILL_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SkillMetadata {
    name: String,
    description: String,
    directory: PathBuf,
}

struct SkillCatalog {
    root: PathBuf,
    max_skill_bytes: usize,
    skills: BTreeMap<String, SkillMetadata>,
}

impl SkillCatalog {
    fn discover(root: &Path, max_skill_bytes: usize) -> Result<Self> {
        if max_skill_bytes == 0 {
            return Err(Error::Plugin("skill size limit must be non-zero".into()));
        }
        let root = fs::canonicalize(root).map_err(|error| {
            Error::Plugin(format!(
                "could not resolve skill root {}: {error}",
                root.display()
            ))
        })?;
        if !root.is_dir() {
            return Err(Error::Plugin(format!(
                "skill root is not a directory: {}",
                root.display()
            )));
        }

        let mut pending = vec![root.clone()];
        let mut visited = 0usize;
        let mut skills = BTreeMap::new();
        while let Some(directory) = pending.pop() {
            visited += 1;
            if visited > MAX_SKILL_DIRECTORIES {
                return Err(Error::Plugin("skill directory limit exceeded".into()));
            }
            let skill_path = directory.join("SKILL.md");
            if is_regular_file(&skill_path)? {
                let (name, description) = read_metadata(&skill_path)?;
                validate_name(&name)?;
                validate_description(&description)?;
                let expected_directory = directory
                    .file_name()
                    .and_then(|part| part.to_str())
                    .ok_or_else(|| {
                        Error::Plugin(format!("invalid skill directory {}", directory.display()))
                    })?;
                if expected_directory != name {
                    return Err(Error::Plugin(format!(
                        "skill name {name:?} must match directory {expected_directory:?}"
                    )));
                }
                let metadata = SkillMetadata {
                    name: name.clone(),
                    description,
                    directory: directory
                        .strip_prefix(&root)
                        .unwrap_or(&directory)
                        .to_path_buf(),
                };
                if skills.insert(name.clone(), metadata).is_some() {
                    return Err(Error::Plugin(format!("duplicate skill name {name:?}")));
                }
                continue;
            }

            let mut entries = fs::read_dir(&directory)
                .map_err(|error| {
                    Error::Plugin(format!("could not list {}: {error}", directory.display()))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| {
                    Error::Plugin(format!("could not list {}: {error}", directory.display()))
                })?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries.into_iter().rev() {
                let file_type = entry.file_type().map_err(|error| {
                    Error::Plugin(format!(
                        "could not inspect {}: {error}",
                        entry.path().display()
                    ))
                })?;
                if file_type.is_dir() && !file_type.is_symlink() && !is_ignored(&entry.path()) {
                    pending.push(entry.path());
                }
            }
        }
        Ok(Self {
            root,
            max_skill_bytes,
            skills,
        })
    }

    fn load(&self, name: &str) -> Result<String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| Error::Tool(format!("skill {name:?} was not found")))?;
        let path = self.root.join(&skill.directory).join("SKILL.md");
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            Error::Plugin(format!("could not inspect {}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Plugin(format!(
                "skill file must be a regular non-symlink file: {}",
                path.display()
            )));
        }
        if metadata.len() > self.max_skill_bytes as u64 {
            return Err(Error::Plugin(format!(
                "skill {name:?} exceeds the size limit"
            )));
        }
        let resolved = fs::canonicalize(&path).map_err(|error| {
            Error::Plugin(format!("could not resolve {}: {error}", path.display()))
        })?;
        if !resolved.starts_with(&self.root) {
            return Err(Error::Plugin(format!(
                "skill {name:?} escapes the catalog root"
            )));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        fs::File::open(&resolved)
            .and_then(|file| {
                file.take(self.max_skill_bytes as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|error| {
                Error::Plugin(format!("could not read {}: {error}", path.display()))
            })?;
        if bytes.len() > self.max_skill_bytes {
            return Err(Error::Plugin(format!(
                "skill {name:?} exceeds the size limit"
            )));
        }
        String::from_utf8(bytes)
            .map_err(|error| Error::Plugin(format!("skill {name:?} is not UTF-8: {error}")))
    }
}

struct SkillsState {
    catalog: SkillCatalog,
    selected: Mutex<BTreeMap<SessionId, BTreeSet<String>>>,
}

struct SkillsPlugin {
    config: SkillsConfig,
    state: OnceLock<Arc<SkillsState>>,
}

struct SkillsTool {
    state: Arc<SkillsState>,
}

struct SkillsHook {
    state: Arc<SkillsState>,
}

pub fn skills_plugin(config: SkillsConfig) -> Arc<dyn Plugin> {
    Arc::new(SkillsPlugin {
        config,
        state: OnceLock::new(),
    })
}

#[async_trait]
impl Plugin for SkillsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        let state = if let Some(state) = self.state.get() {
            state.clone()
        } else {
            let candidate = Arc::new(SkillsState {
                catalog: SkillCatalog::discover(&self.config.root, self.config.max_skill_bytes)?,
                selected: Mutex::new(BTreeMap::new()),
            });
            if self.state.set(candidate.clone()).is_ok() {
                candidate
            } else {
                self.state
                    .get()
                    .expect("skills state was set by another initializer")
                    .clone()
            }
        };
        registrar.register_tool(
            0,
            Arc::new(SkillsTool {
                state: state.clone(),
            }),
        )?;
        registrar.register_context_contribution(HOOK_ID, 0, Arc::new(SkillsHook { state }))
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum SkillsInput {
    List,
    Select { name: String },
    Clear,
}

#[derive(Serialize)]
struct SkillsResponse<'a> {
    skills: Vec<&'a SkillMetadata>,
    selected: Vec<String>,
}

#[async_trait]
impl Tool for SkillsTool {
    fn id(&self) -> ToolId {
        ToolId::new(TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_NAME.into(),
            description: "List, select, or clear skills for this session.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["action"],
                "properties": {
                    "action": { "type": "string", "enum": ["list", "select", "clear"] },
                    "name": { "type": "string", "minLength": 1 }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let input: SkillsInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid skills input: {error}")))?;
        let mut selected = self
            .state
            .selected
            .lock()
            .map_err(|_| Error::Tool("skills state lock is poisoned".into()))?;
        let session = selected.entry(context.session_id).or_default();
        match input {
            SkillsInput::List => {}
            SkillsInput::Select { name } => {
                if !self.state.catalog.skills.contains_key(&name) {
                    return Err(Error::Tool(format!("skill {name:?} was not found")));
                }
                session.insert(name);
            }
            SkillsInput::Clear => session.clear(),
        }
        let content = serde_json::to_string(&SkillsResponse {
            skills: self.state.catalog.skills.values().collect(),
            selected: session.iter().cloned().collect(),
        })
        .map_err(|error| Error::Tool(format!("could not encode skills output: {error}")))?;
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

#[async_trait]
impl ContextContributionHook for SkillsHook {
    async fn contribute_context(
        &self,
        hook_context: &HookContext,
        _workdir: Arc<dyn Workdir>,
        context: &mut Context,
    ) -> Result<()> {
        let selected = self
            .state
            .selected
            .lock()
            .map_err(|_| Error::Plugin("skills state lock is poisoned".into()))?
            .get(&hook_context.session_id)
            .cloned()
            .unwrap_or_default();
        for name in selected {
            context.push(ContextPart {
                priority: ContextPriority::High,
                source: ContextSource::Plugin(PLUGIN_ID.into()),
                content: format!(
                    "Selected skill {name}:\n{}",
                    self.state.catalog.load(&name)?
                ),
            });
        }
        Ok(())
    }
}

fn is_regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::Plugin(format!(
            "skill file may not be a symlink: {}",
            path.display()
        ))),
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Plugin(format!(
            "could not inspect {}: {error}",
            path.display()
        ))),
    }
}

fn is_ignored(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|part| part.to_str()),
        Some(".git" | "target")
    )
}

fn read_metadata(path: &Path) -> Result<(String, String)> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_METADATA_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| Error::Plugin(format!("could not read {}: {error}", path.display())))?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(Error::Plugin(format!(
            "skill metadata {} exceeds the size limit",
            path.display()
        )));
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::Plugin(format!(
            "skill metadata {} is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Err(Error::Plugin(format!(
            "skill {} must start with YAML-style front matter",
            path.display()
        )));
    }
    let mut name = None;
    let mut description = None;
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        let (key, value) = line.split_once(':').ok_or_else(|| {
            Error::Plugin(format!(
                "invalid skill metadata line in {}: {line:?}",
                path.display()
            ))
        })?;
        let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    if !closed {
        return Err(Error::Plugin(format!(
            "unterminated skill metadata in {}",
            path.display()
        )));
    }
    Ok((
        name.ok_or_else(|| Error::Plugin(format!("skill {} has no name", path.display())))?,
        description
            .ok_or_else(|| Error::Plugin(format!("skill {} has no description", path.display())))?,
    ))
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(Error::Plugin(format!("invalid skill name {name:?}")))
    }
}

fn validate_description(description: &str) -> Result<()> {
    if !description.trim().is_empty()
        && description.len() <= 1024
        && !description.contains(['\n', '\r'])
    {
        Ok(())
    } else {
        Err(Error::Plugin(
            "skill description must be 1..=1024 bytes on one line".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{Core, ProjectId, SessionId, TurnId},
        testkit::StubWorkdir,
    };
    use tokio_util::sync::CancellationToken;

    fn tool_context(root: &Path, session_id: SessionId) -> ToolContext {
        ToolContext {
            project_id: ProjectId::new(),
            session_id,
            turn_id: TurnId::new(),
            workdir: Arc::new(StubWorkdir::new(root)),
            cancellation: CancellationToken::new(),
        }
    }

    fn create_skill(root: &Path) {
        let skill = root.join("review-code");
        fs::create_dir(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: review-code\ndescription: Reviews code safely\n---\n\nBe precise.\n",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn plugin_tool_and_hook_share_session_selection() {
        let temp = tempfile::tempdir().unwrap();
        create_skill(temp.path());
        let core = Core::new()
            .with_plugin(skills_plugin(SkillsConfig::new(temp.path())))
            .build()
            .await
            .unwrap();
        let tool = core.tools().get(&ToolId::new(TOOL_ID)).unwrap();
        let session_id = SessionId::new();
        tool.execute(
            serde_json::json!({"action": "select", "name": "review-code"}),
            tool_context(temp.path(), session_id),
        )
        .await
        .unwrap();

        let mut selected_context = Context::default();
        core.hooks()
            .contribute_context(
                &HookContext {
                    project_id: ProjectId::new(),
                    session_id,
                },
                Arc::new(StubWorkdir::new(temp.path())),
                &mut selected_context,
            )
            .await
            .unwrap();
        assert!(selected_context.parts()[0].content.contains("Be precise."));

        let mut other_context = Context::default();
        core.hooks()
            .contribute_context(
                &HookContext {
                    project_id: ProjectId::new(),
                    session_id: SessionId::new(),
                },
                Arc::new(StubWorkdir::new(temp.path())),
                &mut other_context,
            )
            .await
            .unwrap();
        assert!(other_context.parts().is_empty());
    }

    #[tokio::test]
    async fn omitting_plugin_omits_tool_and_context_hook() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new().build().await.unwrap();
        assert!(core.tools().get(&ToolId::new(TOOL_ID)).is_none());
        let mut context = Context::default();
        core.hooks()
            .contribute_context(
                &HookContext {
                    project_id: ProjectId::new(),
                    session_id: SessionId::new(),
                },
                Arc::new(StubWorkdir::new(temp.path())),
                &mut context,
            )
            .await
            .unwrap();
        assert!(context.parts().is_empty());
    }
}
