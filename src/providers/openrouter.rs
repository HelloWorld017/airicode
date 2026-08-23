use std::sync::Arc;

use async_trait::async_trait;

use crate::core::{
    Model, Plugin, PluginId, PluginRegistrar, Provider, ProviderId, ProviderRequest,
    ProviderStream, Result,
};

use super::openai_compatible::OpenAiCompatible;

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouterConfig {
    pub api_key: String,
    pub base_url: Option<String>,
    pub site_url: Option<String>,
    pub app_name: Option<String>,
}

impl OpenRouterConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: None,
            site_url: None,
            app_name: None,
        }
    }
}

pub fn openrouter_plugin(config: OpenRouterConfig) -> Arc<dyn Plugin> {
    Arc::new(OpenRouterPlugin {
        provider: Arc::new(OpenRouterProvider::from_config(config)),
    })
}

struct OpenRouterPlugin {
    provider: Arc<OpenRouterProvider>,
}

struct OpenRouterProvider {
    inner: OpenAiCompatible,
}

impl OpenRouterProvider {
    fn from_config(config: OpenRouterConfig) -> Self {
        let mut headers = Vec::new();
        if let Some(site_url) = config.site_url {
            headers.push(("http-referer", site_url));
        }
        if let Some(app_name) = config.app_name {
            headers.push(("x-title", app_name));
        }
        Self {
            inner: OpenAiCompatible::new(
                "openrouter",
                config.base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
                config.api_key,
                headers,
            ),
        }
    }
}

#[async_trait]
impl Plugin for OpenRouterPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openrouter")
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_provider(0, self.provider.clone())
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
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
    fn applies_openrouter_attribution_headers() {
        let provider = OpenRouterProvider::from_config(OpenRouterConfig {
            api_key: "secret".into(),
            base_url: None,
            site_url: Some("https://example.com".into()),
            app_name: Some("Example".into()),
        });
        let request = provider.inner.test_request();

        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert_eq!(request.headers()["http-referer"], "https://example.com");
        assert_eq!(request.headers()["x-title"], "Example");
    }
}
