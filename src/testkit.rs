use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures_util::stream;
use tokio::sync::Notify;

use crate::core::*;

struct StubProviderPlugin {
    id: PluginId,
    provider: Arc<dyn Provider>,
}

pub fn stub_provider_plugin(provider: StubProvider) -> Arc<dyn Plugin> {
    Arc::new(StubProviderPlugin {
        id: PluginId::new(format!("test-provider-{}", provider.id())),
        provider: Arc::new(provider),
    })
}

#[async_trait]
impl Plugin for StubProviderPlugin {
    fn id(&self) -> PluginId {
        self.id.clone()
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_provider(0, self.provider.clone())
    }
}

pub struct StubProvider {
    id: ProviderId,
    response: Mutex<Option<Vec<ProviderEvent>>>,
    gate: Option<Arc<Notify>>,
}

impl StubProvider {
    pub fn responding(id: impl Into<ProviderId>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            response: Mutex::new(Some(vec![
                ProviderEvent::TextDelta { text: text.into() },
                ProviderEvent::Finished {
                    reason: FinishReason::Stop,
                },
            ])),
            gate: None,
        }
    }

    pub fn blocked(id: impl Into<ProviderId>, gate: Arc<Notify>) -> Self {
        Self {
            id: id.into(),
            response: Mutex::new(None),
            gate: Some(gate),
        }
    }
}

#[async_trait]
impl Provider for StubProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }
    async fn get_models(&self) -> Result<Vec<Model>> {
        Ok(vec![Model {
            id: "test".into(),
            display_name: "Test".into(),
        }])
    }
    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream> {
        if let Some(gate) = &self.gate {
            tokio::select! {
                _ = request.cancellation.cancelled() => return Err(Error::Cancelled),
                _ = gate.notified() => {}
            }
        }
        let events = self
            .response
            .lock()
            .expect("stub lock poisoned")
            .clone()
            .ok_or_else(|| Error::Provider("stub has no response".into()))?;
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

pub struct StubWorkdir {
    root: PathBuf,
}

impl StubWorkdir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl Workdir for StubWorkdir {
    fn root(&self) -> &Path {
        &self.root
    }
    async fn read(&self, _path: &Path) -> Result<Vec<u8>> {
        Err(Error::Workdir("not configured".into()))
    }
    async fn write(&self, _path: &Path, _data: &[u8]) -> Result<()> {
        Err(Error::Workdir("not configured".into()))
    }
    async fn remove(&self, _path: &Path) -> Result<()> {
        Err(Error::Workdir("not configured".into()))
    }
    async fn execute(
        &self,
        _command: CommandSpec,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<CommandOutput> {
        Err(Error::Workdir("not configured".into()))
    }
}
