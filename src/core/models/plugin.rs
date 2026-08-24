use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::super::error::Result;
use super::super::registry::PluginRegistryScope;
use super::id::PluginId;

#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> PluginId;
    fn name(&self) -> &str;
    fn config_schema(&self) -> Value {
        Value::Object(Default::default())
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()>;
}
