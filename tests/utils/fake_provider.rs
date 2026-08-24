use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures_util::stream;
use serde_json::Value;

use airicode::core::{
    error::{Error, Result},
    models::{
        Model, ModelCapabilities, Plugin, PluginId, Provider, ProviderEvent, ProviderId,
        ProviderRequest, ProviderStream,
    },
    registry::PluginRegistryScope,
};

pub struct FakeProvider {
    id: ProviderId,
    responses: Mutex<VecDeque<Vec<ProviderEvent>>>,
    model: String,
}

impl FakeProvider {
    pub fn new(id: ProviderId, responses: impl IntoIterator<Item = Vec<ProviderEvent>>) -> Self {
        Self {
            id,
            responses: Mutex::new(responses.into_iter().collect()),
            model: "fake-model".into(),
        }
    }

    #[allow(dead_code)]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    #[allow(dead_code)]
    pub fn push_response(&self, response: Vec<ProviderEvent>) -> Result<()> {
        self.responses
            .lock()
            .map_err(|_| Error::Provider("fake provider poisoned".into()))?
            .push_back(response);
        Ok(())
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> ProviderId {
        self.id
    }

    async fn get_models(&self) -> Result<Vec<Model>> {
        Ok(vec![Model {
            id: self.model.clone(),
            display_name: "Fake model".into(),
            capabilities: ModelCapabilities {
                context_window: None,
                tools: true,
                reasoning: true,
            },
        }])
    }

    async fn request(&self, _request: ProviderRequest) -> Result<ProviderStream> {
        let response = self
            .responses
            .lock()
            .map_err(|_| Error::Provider("fake provider poisoned".into()))?
            .pop_front()
            .ok_or_else(|| Error::Provider("fake provider has no scripted response".into()))?;
        Ok(Box::pin(stream::iter(response.into_iter().map(Ok))))
    }
}

pub struct FakeProviderPlugin {
    id: PluginId,
    provider: Arc<FakeProvider>,
}

impl FakeProviderPlugin {
    pub fn new(provider: Arc<FakeProvider>) -> Self {
        Self {
            id: PluginId::new(),
            provider,
        }
    }
}

#[async_trait]
impl Plugin for FakeProviderPlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "fake_provider"
    }

    fn config_schema(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry
            .register_provider(self.provider.clone(), 0)
            .map(|_| ())
    }
}
