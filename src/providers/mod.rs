mod openai;
mod openai_compatible;
mod openrouter;

pub use openai::{openai_plugin, OpenAiConfig};
pub use openrouter::{openrouter_plugin, OpenRouterConfig};

#[cfg(test)]
mod tests {
    use crate::core::{PluginId, PluginRegistrar, ProviderId};

    use super::*;

    async fn registrations(
        plugin: std::sync::Arc<dyn crate::core::Plugin>,
    ) -> (PluginId, ProviderId) {
        let plugin_id = plugin.id();
        let registrar = PluginRegistrar::new(plugin_id.clone());
        plugin.init(registrar.clone()).await.unwrap();
        let staged = registrar.take();
        assert_eq!(staged.providers.len(), 1);
        assert!(staged.tools.is_empty());
        assert!(staged.hooks.is_empty());
        assert!(staged.store_factories.is_empty());
        (plugin_id, staged.providers[0].id.clone())
    }

    #[tokio::test]
    async fn provider_plugins_register_independently() {
        let openai = registrations(openai_plugin(OpenAiConfig::new("openai-secret"))).await;
        let openrouter = registrations(openrouter_plugin(OpenRouterConfig::new(
            "openrouter-secret",
        )))
        .await;

        assert_eq!(openai, (PluginId::new("openai"), ProviderId::new("openai")));
        assert_eq!(
            openrouter,
            (PluginId::new("openrouter"), ProviderId::new("openrouter"))
        );
    }
}
