use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::error::Result;
use super::models::PluginId;
use super::registry::PluginRegistryScope;

#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> PluginId;
    fn name(&self) -> &str;
    fn config_schema(&self) -> Value {
        Value::Object(Default::default())
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()>;
}
