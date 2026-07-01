use super::*;

/// In-memory mock used to confirm the trait is object-safe and the service
/// layer's `Arc<dyn PluginConfigRepo>` parameter compiles.
struct MockRepo;

#[async_trait]
impl PluginConfigRepo for MockRepo {
    async fn find(
        &self,
        _plugin_instance_id: &str,
        _session_type_id: Uuid,
    ) -> Result<Option<JsonValue>, ChatEngineError> {
        Ok(None)
    }

    async fn upsert(
        &self,
        _plugin_instance_id: &str,
        _session_type_id: Uuid,
        _config: JsonValue,
    ) -> Result<(), ChatEngineError> {
        Ok(())
    }

    async fn delete(
        &self,
        _plugin_instance_id: &str,
        _session_type_id: Uuid,
    ) -> Result<(), ChatEngineError> {
        Ok(())
    }
}

#[test]
fn trait_is_object_safe() {
    let _erased: Arc<dyn PluginConfigRepo> = Arc::new(MockRepo);
}

#[tokio::test]
async fn mock_find_returns_none() {
    let repo: Arc<dyn PluginConfigRepo> = Arc::new(MockRepo);
    let got = repo.find("p1", Uuid::nil()).await.expect("ok");
    assert!(got.is_none());
}
