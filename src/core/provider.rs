use async_trait::async_trait;

use super::error::Result;
use super::models::{Model, ProviderId, ProviderRequest, ProviderStream};

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn get_models(&self) -> Result<Vec<Model>>;
    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream>;
}
