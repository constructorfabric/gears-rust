use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use super::LazyRmsAdapterRegistry;
use crate::domain::rms_registry::RmsAdapterRegistry;

fn registry() -> LazyRmsAdapterRegistry {
    LazyRmsAdapterRegistry::new(Arc::new(ClientHub::new()))
}

#[tokio::test]
async fn lookup_is_fail_closed_none() {
    let reg = registry();
    assert!(
        reg.lookup("gts.cf.rms._.adapter.v1~acme.rms._.s3.v1")
            .await
            .expect("lookup is infallible today")
            .is_none(),
        "the lazy registry stub must fail closed (Ok(None)) until the RMS client is wired"
    );
}

#[tokio::test]
async fn gts_id_by_cert_subject_is_fail_closed_none() {
    let reg = registry();
    assert!(
        reg.gts_id_by_cert_subject("CN=adapter.example")
            .await
            .expect("resolution is infallible today")
            .is_none(),
        "the lazy registry stub must fail closed (Ok(None)) until the RMS client is wired"
    );
}
