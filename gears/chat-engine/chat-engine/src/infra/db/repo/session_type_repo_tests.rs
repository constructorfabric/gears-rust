use super::*;

struct MockRepo;

#[async_trait]
impl SessionTypeRepo for MockRepo {
    async fn insert(
        &self,
        _new: crate::domain::ports::NewSessionType,
    ) -> Result<chat_engine_sdk::models::SessionType, ChatEngineError> {
        Err(ChatEngineError::internal("mock: insert not implemented"))
    }

    async fn find_by_id(
        &self,
        _session_type_id: Uuid,
    ) -> Result<Option<chat_engine_sdk::models::SessionType>, ChatEngineError> {
        Ok(None)
    }

    async fn list(&self) -> Result<Vec<chat_engine_sdk::models::SessionType>, ChatEngineError> {
        Ok(Vec::new())
    }
}

#[test]
fn trait_is_object_safe() {
    let _erased: Arc<dyn SessionTypeRepo> = Arc::new(MockRepo);
}
