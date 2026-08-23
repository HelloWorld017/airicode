use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub default_provider: String,
    pub default_model: String,
    pub default_mode: String,
    pub compaction: CompactionConfig,
    pub modes: BTreeMap<String, ModeConfig>,
    pub provider: BTreeMap<String, ProviderConfig>,
    pub sandbox: SandboxConfig,
    pub persistence: PersistenceConfig,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        let mut modes = BTreeMap::new();
        modes.insert("default".into(), ModeConfig::default());

        let mut provider = BTreeMap::new();
        provider.insert(
            "openai".into(),
            ProviderConfig::OpenAi {
                api_key_env: "OPENAI_API_KEY".into(),
                base_url: None,
            },
        );
        provider.insert(
            "openrouter".into(),
            ProviderConfig::OpenRouter {
                api_key_env: "OPENROUTER_API_KEY".into(),
                base_url: None,
            },
        );

        Self {
            default_provider: "openai".into(),
            default_model: "gpt-4.1-mini".into(),
            default_mode: "default".into(),
            compaction: CompactionConfig::default(),
            modes,
            provider,
            sandbox: SandboxConfig::default(),
            persistence: PersistenceConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Config {
    /// Loads built-in defaults followed by user, project, and explicit config files.
    pub fn load(project: impl AsRef<Path>, explicit: Option<&Path>) -> ConfigResult<Self> {
        let user = user_config_path();
        let project = project.as_ref().join(".airicode/config.toml");
        Self::load_paths(user.as_deref(), Some(&project), explicit)
    }

    /// Loads one file over the built-in defaults.
    pub fn from_path(path: impl AsRef<Path>) -> ConfigResult<Self> {
        Self::load_paths(None, None, Some(path.as_ref()))
    }

    pub fn validate(&self) -> ConfigResult<()> {
        if self.default_provider.trim().is_empty() {
            return Err(ConfigError::Validation(
                "default_provider may not be empty".into(),
            ));
        }
        if !self.provider.contains_key(&self.default_provider) {
            return Err(ConfigError::Validation(format!(
                "default_provider {:?} is not configured",
                self.default_provider
            )));
        }
        if self.default_model.trim().is_empty() {
            return Err(ConfigError::Validation(
                "default_model may not be empty".into(),
            ));
        }
        if !self.modes.contains_key(&self.default_mode) {
            return Err(ConfigError::Validation(format!(
                "default_mode {:?} is not configured",
                self.default_mode
            )));
        }
        if self.compaction.reserved_tokens == 0 {
            return Err(ConfigError::Validation(
                "compaction.reserved_tokens must be greater than zero".into(),
            ));
        }
        if self.sandbox.max_output_bytes == 0 {
            return Err(ConfigError::Validation(
                "sandbox.max_output_bytes must be greater than zero".into(),
            ));
        }
        if self.persistence.data_dir.as_os_str().is_empty() {
            return Err(ConfigError::Validation(
                "persistence.data_dir may not be empty".into(),
            ));
        }
        for (name, provider) in &self.provider {
            if name.trim().is_empty() {
                return Err(ConfigError::Validation(
                    "provider names may not be empty".into(),
                ));
            }
            let variable = provider.api_key_env();
            if !is_env_name(variable) {
                return Err(ConfigError::Validation(format!(
                    "provider {name:?} has invalid api_key_env {variable:?}"
                )));
            }
            if provider
                .base_url()
                .is_some_and(|url| !(url.starts_with("https://") || url.starts_with("http://")))
            {
                return Err(ConfigError::Validation(format!(
                    "provider {name:?} base_url must use http or https"
                )));
            }
        }
        Ok(())
    }

    fn load_paths(
        user: Option<&Path>,
        project: Option<&Path>,
        explicit: Option<&Path>,
    ) -> ConfigResult<Self> {
        let mut merged = toml::Value::try_from(Self::default())
            .map_err(|error| ConfigError::Internal(error.to_string()))?;
        for path in [user, project].into_iter().flatten() {
            merge_optional_file(&mut merged, path)?;
        }
        if let Some(path) = explicit {
            merge_required_file(&mut merged, path)?;
        }
        let config: Self = merged.try_into().map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAi {
        api_key_env: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    #[serde(rename = "openrouter")]
    OpenRouter {
        api_key_env: String,
        #[serde(default)]
        base_url: Option<String>,
    },
}

impl ProviderConfig {
    pub fn api_key_env(&self) -> &str {
        match self {
            Self::OpenAi { api_key_env, .. } | Self::OpenRouter { api_key_env, .. } => api_key_env,
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        match self {
            Self::OpenAi { base_url, .. } | Self::OpenRouter { base_url, .. } => {
                base_url.as_deref()
            }
        }
    }

    pub fn runtime_id(&self) -> &'static str {
        match self {
            Self::OpenAi { .. } => "openai",
            Self::OpenRouter { .. } => "openrouter",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactionConfig {
    pub auto: bool,
    pub reserved_tokens: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            auto: true,
            reserved_tokens: 8_192,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModeConfig {
    pub instructions: Vec<String>,
    pub preferred_model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub allow_network: bool,
    pub writable_paths: Vec<PathBuf>,
    pub max_output_bytes: usize,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_network: true,
            writable_paths: Vec::new(),
            max_output_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PersistenceConfig {
    pub data_dir: PathBuf,
    pub fsync: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        let data_dir = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from(".airicode/data"))
            .join("airicode");
        Self {
            data_dir,
            fsync: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub color: bool,
    pub show_reasoning: bool,
    pub show_tool_calls: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            color: true,
            show_reasoning: true,
            show_tool_calls: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration file {path} could not be read: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("configuration file {path} is invalid: {source}")]
    FileParse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("configuration is invalid: {0}")]
    Parse(toml::de::Error),
    #[error("configuration is invalid: {0}")]
    Validation(String),
    #[error("could not construct built-in configuration: {0}")]
    Internal(String),
}

pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

fn user_config_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|base| base.join("airicode/config.toml"))
}

fn merge_optional_file(target: &mut toml::Value, path: &Path) -> ConfigResult<()> {
    match fs::read_to_string(path) {
        Ok(contents) => merge_contents(target, path, &contents),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn merge_required_file(target: &mut toml::Value, path: &Path) -> ConfigResult<()> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    merge_contents(target, path, &contents)
}

fn merge_contents(target: &mut toml::Value, path: &Path, contents: &str) -> ConfigResult<()> {
    let value = toml::from_str(contents).map_err(|source| ConfigError::FileParse {
        path: path.to_path_buf(),
        source,
    })?;
    merge_value(target, value);
    Ok(())
}

fn merge_value(target: &mut toml::Value, overlay: toml::Value) {
    match (target, overlay) {
        (toml::Value::Table(target), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match target.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, overlay) => *target = overlay,
    }
}

fn is_env_name(name: &str) -> bool {
    let mut characters = name.bytes();
    matches!(characters.next(), Some(b'A'..=b'Z' | b'_'))
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == b'_'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_merge_tables_and_later_values_win() {
        let directory = tempfile::tempdir().unwrap();
        let user = directory.path().join("user.toml");
        let project = directory.path().join("project.toml");
        let explicit = directory.path().join("explicit.toml");
        fs::write(
            &user,
            "default_model = 'user-model'\n[compaction]\nauto = false\n",
        )
        .unwrap();
        fs::write(
            &project,
            "default_model = 'project-model'\n[compaction]\nreserved_tokens = 2048\n",
        )
        .unwrap();
        fs::write(&explicit, "default_model = 'explicit-model'\n").unwrap();

        let config = Config::load_paths(Some(&user), Some(&project), Some(&explicit)).unwrap();
        assert_eq!(config.default_model, "explicit-model");
        assert!(!config.compaction.auto);
        assert_eq!(config.compaction.reserved_tokens, 2_048);
        assert!(config.provider.contains_key("openai"));
    }

    #[test]
    fn adds_a_typed_named_provider() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "default_provider = 'company'\n[provider.company]\ntype = 'openai'\napi_key_env = 'COMPANY_API_KEY'\nbase_url = 'https://llm.example/v1'\n",
        )
        .unwrap();

        let config = Config::from_path(&path).unwrap();
        assert!(matches!(
            config.provider["company"],
            ProviderConfig::OpenAi { .. }
        ));
    }

    #[test]
    fn rejects_plaintext_keys_and_invalid_references() {
        let directory = tempfile::tempdir().unwrap();
        let plaintext = directory.path().join("plaintext.toml");
        fs::write(&plaintext, "[provider.openai]\napi_key = 'secret'\n").unwrap();
        assert!(Config::from_path(&plaintext).is_err());

        let missing = directory.path().join("missing.toml");
        fs::write(&missing, "default_provider = 'missing'\n").unwrap();
        let error = Config::from_path(&missing).unwrap_err().to_string();
        assert!(error.contains("not configured"));
    }
}
