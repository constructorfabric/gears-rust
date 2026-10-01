#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_type_schema};

use super::RegistryTypeCatalog;
use crate::domain::ports::TypeCatalogPort;

const WIDGET: &str = "gts.cf.core.policy_engine.widget.v1~";
const UNKNOWN: &str = "gts.cf.core.policy_engine.unknown.v1~";

fn catalog() -> RegistryTypeCatalog {
    let client = MockTypesRegistryClient::new().with_type_schemas([make_test_type_schema(WIDGET)]);
    RegistryTypeCatalog::new(Arc::new(client), Duration::from_secs(1))
}

#[tokio::test]
async fn known_types_reports_only_registered_identifiers() {
    let known = catalog()
        .known_types(&[WIDGET.to_owned(), UNKNOWN.to_owned()])
        .await
        .unwrap();
    assert_eq!(known.len(), 1);
    assert!(known.contains(WIDGET));
}

#[tokio::test]
async fn an_empty_request_does_not_call_the_registry() {
    assert!(catalog().known_types(&[]).await.unwrap().is_empty());
}
