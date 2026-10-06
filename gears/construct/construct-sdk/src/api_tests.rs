use std::sync::Arc;

use super::*;

/// Minimal in-test implementation to prove the trait is object-safe and usable
/// as `Arc<dyn ConstructClientV1>`, which is how `ClientHub` stores it.
struct StubClient;

#[async_trait]
impl ConstructClientV1 for StubClient {
    async fn create_note(
        &self,
        _ctx: &SecurityContext,
        note: NewFoundationNote,
    ) -> Result<FoundationNote, CanonicalError> {
        Ok(FoundationNote::new(Uuid::nil(), Uuid::nil(), note.text))
    }

    async fn get_note(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<FoundationNote, CanonicalError> {
        Ok(FoundationNote::new(Uuid::nil(), Uuid::nil(), String::new()))
    }
}

#[test]
fn client_trait_is_object_safe() {
    let client: Arc<dyn ConstructClientV1> = Arc::new(StubClient);
    assert_eq!(Arc::strong_count(&client), 1);
}

#[test]
fn client_trait_object_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Arc<dyn ConstructClientV1>>();
}
