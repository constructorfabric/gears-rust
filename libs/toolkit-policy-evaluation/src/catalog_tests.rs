#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::BackendCatalog;
use crate::rego::REGO_BACKEND_ID;

#[test]
fn the_default_catalog_serves_the_rego_backend_by_id() {
    let catalog = BackendCatalog::with_default_backends();
    let rego = catalog
        .get(REGO_BACKEND_ID)
        .expect("rego backend registered");
    rego.validate_syntax("package p\nallow := true").unwrap();
    assert!(catalog.get("unknown-backend").is_none());
    assert!(BackendCatalog::default().get(REGO_BACKEND_ID).is_none());
}

#[test]
fn debug_lists_backend_ids_only() {
    let rendered = format!("{:?}", BackendCatalog::with_default_backends());
    assert!(rendered.contains(REGO_BACKEND_ID), "{rendered}");
}
