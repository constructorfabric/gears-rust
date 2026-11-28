//! The discovery client a target's plugin instance resolves to (P-D-268): the scoped client
//! registered under that instance id, or closed.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::quota_test_support::{FakeDiscovery, TEST_PLUGIN, snapshot_of, vcpu_record};
use crate::test_support::authed_ctx;
use bss_products_sdk::quota::discovery::QuotaDiscoveryRequest;
use toolkit::client_hub::ClientScope;
use uuid::Uuid;

#[tokio::test]
async fn a_registered_scoped_client_resolves_and_none_is_closed() {
    let hub = Arc::new(ClientHub::new());
    let fake = FakeDiscovery::paged(snapshot_of(&[vcpu_record()], "7"), 50);
    hub.register_scoped::<dyn ProductsQuotaDiscoveryPluginClientV1>(
        ClientScope::gts_id(TEST_PLUGIN),
        Arc::new(fake.clone()),
    );
    let resolver = HubQuotaDiscovery::new(hub);
    let ResolvedDiscovery::Client(client) = resolver.resolve(TEST_PLUGIN) else {
        panic!("the registered plugin instance resolves");
    };
    let page = client
        .list_capabilities(
            &authed_ctx(Uuid::new_v4()),
            QuotaDiscoveryRequest {
                target_key: "t".to_owned(),
                cursor: None,
                page_size: 50,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![vcpu_record()]);
    assert_eq!(
        fake.requests().len(),
        1,
        "the resolved client is the registered one"
    );
    let other = TEST_PLUGIN.replace("test.v1", "other.v1");
    assert!(matches!(
        resolver.resolve(&other),
        ResolvedDiscovery::Closed
    ));
    assert!(matches!(
        ClosedQuotaDiscovery.resolve(TEST_PLUGIN),
        ResolvedDiscovery::Closed
    ));
}
