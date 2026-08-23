use std::sync::Arc;

use async_trait::async_trait;

use crate::core::{
    Model, Plugin, PluginId, PluginRegistrar, Provider, ProviderId, ProviderRequest,
    ProviderStream, Result,
};

use super::openai_compatible::OpenAiCompatible;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: Option<String>,
    pub organization: Option<String>,
    pub project: Option<String>,
}

impl OpenAiConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: None,
            organization: None,
            project: None,
        }
    }
}

pub fn openai_plugin(config: OpenAiConfig) -> Arc<dyn Plugin> {
    Arc::new(OpenAiPlugin {
        provider: Arc::new(OpenAiProvider::from_config(config)),
    })
}

struct OpenAiPlugin {
    provider: Arc<OpenAiProvider>,
}

struct OpenAiProvider {
    inner: OpenAiCompatible,
}

impl OpenAiProvider {
    fn from_config(config: OpenAiConfig) -> Self {
        let mut headers = Vec::new();
        if let Some(organization) = config.organization {
            headers.push(("openai-organization", organization));
        }
        if let Some(project) = config.project {
            headers.push(("openai-project", project));
        }
        Self {
            inner: OpenAiCompatible::new(
                "openai",
                config.base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
                config.api_key,
                headers,
            ),
        }
    }
}

#[async_trait]
impl Plugin for OpenAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_provider(0, self.provider.clone())
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    async fn get_models(&self) -> Result<Vec<Model>> {
        self.inner.get_models().await
    }

    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream> {
        self.inner.request(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_openai_account_headers() {
        let provider = OpenAiProvider::from_config(OpenAiConfig {
            api_key: "secret".into(),
            base_url: None,
            organization: Some("org-123".into()),
            project: Some("proj-456".into()),
        });
        let request = provider.inner.test_request();

        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert_eq!(request.headers()["openai-organization"], "org-123");
        assert_eq!(request.headers()["openai-project"], "proj-456");
    }
}
