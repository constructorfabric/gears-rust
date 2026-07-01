use super::*;

struct MockRepo;

#[async_trait]
impl SessionTypeRepo for MockRepo {
    async fn insert(
        &self,
        _model: session_type_entity::ActiveModel,
    ) -> Result<session_type_entity::Model, ChatEngineError> {
        Err(ChatEngineError::internal("mock: insert not implemented"))
    }

    async fn find_by_id(
        &self,
        _session_type_id: Uuid,
    ) -> Result<Option<session_type_entity::Model>, ChatEngineError> {
        Ok(None)
    }

    async fn list(&self) -> Result<Vec<session_type_entity::Model>, ChatEngineError> {
        Ok(Vec::new())
    }
}

#[test]
fn trait_is_object_safe() {
    let _erased: Arc<dyn SessionTypeRepo> = Arc::new(MockRepo);
}
