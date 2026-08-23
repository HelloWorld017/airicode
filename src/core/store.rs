use std::sync::Arc;

use async_trait::async_trait;

use super::{Message, OpenSession, ProjectId, Result, SessionId, SessionStoreFactoryId, Workdir};

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn load_messages(&self, session_id: SessionId) -> Result<Vec<Message>>;
    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()>;
}

#[derive(Clone)]
pub struct SessionStoreContext {
    pub project_id: ProjectId,
    pub project_name: String,
    pub workdir: Arc<dyn Workdir>,
    pub session: OpenSession,
}

#[async_trait]
pub trait SessionStoreFactory: Send + Sync {
    fn id(&self) -> SessionStoreFactoryId;
    async fn open(&self, context: &SessionStoreContext) -> Result<Option<Arc<dyn SessionStore>>>;
}
