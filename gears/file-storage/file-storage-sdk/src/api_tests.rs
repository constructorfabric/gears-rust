use super::*;

/// Minimal in-test implementation to prove the trait is object-safe and usable
/// through a trait object (which is how `ClientHub` stores it).
struct StubClient;

impl FileStorageClientV1 for StubClient {
    fn module_name(&self) -> &'static str {
        "stub"
    }
}

#[test]
fn client_trait_is_object_safe() {
    let client: Box<dyn FileStorageClientV1> = Box::new(StubClient);
    assert_eq!(client.module_name(), "stub");
}

#[test]
fn client_trait_object_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Box<dyn FileStorageClientV1>>();
}
