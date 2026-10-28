use super::*;

#[test]
fn local_client_reports_module_name_through_the_sdk_trait() {
    let client: Box<dyn FileStorageClientV1> = Box::new(FileStorageLocalClient::new());
    assert_eq!(client.module_name(), "file-storage");
}
