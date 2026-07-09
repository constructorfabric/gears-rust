use super::*;

#[test]
fn module_name_is_file_storage() {
    let client = FileStorageLocalClient::new();
    assert_eq!(client.module_name(), "file-storage");
}

#[test]
fn default_matches_new() {
    let a = FileStorageLocalClient::new();
    let b = FileStorageLocalClient;
    assert_eq!(a.module_name(), b.module_name());
}

#[test]
fn usable_through_sdk_trait_object() {
    // This is how ClientHub stores it: behind `dyn FileStorageClientV1`.
    let client: Box<dyn FileStorageClientV1> = Box::new(FileStorageLocalClient::new());
    assert_eq!(client.module_name(), "file-storage");
}
