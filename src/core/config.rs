use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::{Map, Value};
use tokio::fs;

use super::error::{Error, Result};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolConfig {
    pub enable_hashline: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub raw: Value,
    pub tool: ToolConfig,
    pub plugin_namespaces: BTreeMap<String, Value>,
    schema: Value,
    schemas: Arc<Vec<(String, Value)>>,
}

impl Config {
    pub fn namespace(&self, name: &str) -> Option<&Value> {
        self.plugin_namespaces.get(name)
    }

    pub fn schema(&self) -> &Value {
        &self.schema
    }

    pub fn validate(&self, raw: &Value) -> Result<()> {
        aggregate(raw.clone(), self.schemas.as_ref()).map(|_| ())
    }
}

#[derive(Clone, Debug)]
pub struct ConfigPaths {
    pub global: Option<PathBuf>,
    pub project: PathBuf,
}

impl ConfigPaths {
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        let global = std::env::var_os("XDG_CONFIG_HOME")
            .map(|path| PathBuf::from(path).join("airicode/config.json"))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".config/airicode/config.json"))
            });
        Self {
            global,
            project: project_root.as_ref().join(".airicode/airicode.json"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub raw: Value,
    pub diagnostics: Vec<String>,
}

impl Default for LoadedConfig {
    fn default() -> Self {
        Self {
            raw: Value::Object(Map::new()),
            diagnostics: Vec::new(),
        }
    }
}

pub async fn load_config(paths: &ConfigPaths) -> LoadedConfig {
    let mut loaded = match &paths.global {
        Some(path) => load_config_file(path).await,
        None => LoadedConfig::default(),
    };
    let project = load_config_file(&paths.project).await;
    merge_values(&mut loaded.raw, project.raw);
    loaded.diagnostics.extend(project.diagnostics);
    loaded
}

pub async fn load_config_file(path: &Path) -> LoadedConfig {
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedConfig::default();
        }
        Err(error) => {
            return LoadedConfig {
                raw: Value::Object(Map::new()),
                diagnostics: vec![format!(
                    "cannot read configuration {}: {error}",
                    path.display()
                )],
            };
        }
    };
    let mut raw = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(_) => {
            return LoadedConfig {
                raw: Value::Object(Map::new()),
                diagnostics: vec![format!(
                    "configuration {} must be a JSON object",
                    path.display()
                )],
            };
        }
        Err(error) => {
            return LoadedConfig {
                raw: Value::Object(Map::new()),
                diagnostics: vec![format!(
                    "cannot parse configuration {}: {error}",
                    path.display()
                )],
            };
        }
    };
    raw.as_object_mut()
        .expect("configuration object")
        .remove("$schema");
    LoadedConfig {
        raw,
        diagnostics: Vec::new(),
    }
}

pub async fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::Config(format!(
            "configuration path has no parent: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).await?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let temporary = parent.join(format!(".{filename}.airicode-{}", uuid::Uuid::new_v4()));
    let mut contents = serde_json::to_vec_pretty(value)?;
    contents.push(b'\n');
    fs::write(&temporary, contents).await?;
    if let Err(error) = fs::rename(&temporary, path).await {
        let _ = fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(())
}

pub fn aggregate(raw: Value, schemas: &[(String, Value)]) -> Result<Config> {
    let root = raw.as_object().cloned().unwrap_or_default();
    let tool_value = root
        .get("tool")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let tool_schema = tool_schema();
    validate_shape(&tool_value, &tool_schema, "tool")?;
    let tool = ToolConfig {
        enable_hashline: tool_value
            .get("enable_hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let plugins = root
        .get("plugins")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let plugins = plugins
        .as_object()
        .cloned()
        .ok_or_else(|| Error::Config("plugins must be a table/object".into()))?;
    let mut namespaces = BTreeMap::new();
    for (name, schema) in schemas {
        let value = plugins
            .get(name)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        validate_shape(&value, schema, name)?;
        namespaces.insert(name.clone(), value);
    }
    Ok(Config {
        raw,
        tool,
        plugin_namespaces: namespaces,
        schema: aggregate_schema(schemas),
        schemas: Arc::new(schemas.to_vec()),
    })
}

pub fn aggregate_schema(schemas: &[(String, Value)]) -> Value {
    let plugins = schemas
        .iter()
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect::<Map<_, _>>();
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "tool": tool_schema(),
            "plugins": {
                "type": "object",
                "properties": plugins
            }
        }
    })
}

fn merge_values(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge_values(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "enable_hashline": { "type": "boolean" }
        }
    })
}

fn validate_shape(value: &Value, schema: &Value, path: &str) -> Result<()> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let valid = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !valid {
            return Err(Error::Config(format!("{path} must be {expected}")));
        }
    }
    if let (Some(properties), Some(object)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        for (key, child_schema) in properties {
            if let Some(child) = object.get(key) {
                validate_shape(child, child_schema, &format!("{path}.{key}"))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn project_configuration_overrides_global_configuration() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let global = directory.path().join("global.json");
        let project = directory.path().join("project/.airicode/airicode.json");
        tokio::fs::write(
            &global,
            r#"{"tool":{"enable_hashline":true},"plugins":{"test":{"value":"global"}}}"#,
        )
        .await
        .expect("write global configuration");
        tokio::fs::create_dir_all(project.parent().expect("project parent"))
            .await
            .expect("create project configuration directory");
        tokio::fs::write(
            &project,
            r#"{"$schema":"http://obsolete.test/schema.json","plugins":{"test":{"other":"project"}}}"#,
        )
        .await
        .expect("write project configuration");

        let loaded = load_config(&ConfigPaths {
            global: Some(global),
            project,
        })
        .await;

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(
            loaded.raw,
            serde_json::json!({
                "tool": { "enable_hashline": true },
                "plugins": { "test": { "value": "global", "other": "project" } }
            })
        );
    }
}
