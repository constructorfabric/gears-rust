#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_type_schema};

use super::RegistryTypeCatalog;
use crate::domain::ports::TypeCatalogPort;

use crate::test_support::WIDGET;
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

/// How [`BatchOnly`] answers the batch lookup; every other call is unused.
enum Batch {
    /// Never answers.
    Hang,
    /// Answers the first id with an internal failure.
    FailFirst,
    /// Answers nothing at all.
    Omit,
}

/// A registry fake for the outage paths the shared mock cannot produce.
struct BatchOnly(Batch);

#[async_trait::async_trait]
impl types_registry_sdk::TypesRegistryClient for BatchOnly {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_type_schema(
        &self,
        _type_id: &str,
    ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError> {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_type_schema_by_uuid(
        &self,
        _id: uuid::Uuid,
    ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError> {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_type_schemas(
        &self,
        ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
    > {
        match self.0 {
            Batch::Hang => std::future::pending().await,
            Batch::FailFirst => ids
                .into_iter()
                .take(1)
                .map(|id| {
                    (
                        id,
                        Err(toolkit_canonical_errors::CanonicalError::internal("boom").create()),
                    )
                })
                .collect(),
            Batch::Omit => std::collections::HashMap::new(),
        }
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _ids: Vec<uuid::Uuid>,
    ) -> std::collections::HashMap<
        uuid::Uuid,
        Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
    > {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn list_type_schemas(
        &self,
        _query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, toolkit_canonical_errors::CanonicalError>
    {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_instance(
        &self,
        _id: &str,
    ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError> {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_instance_by_uuid(
        &self,
        _id: uuid::Uuid,
    ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError> {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
    > {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn get_instances_by_uuid(
        &self,
        _ids: Vec<uuid::Uuid>,
    ) -> std::collections::HashMap<
        uuid::Uuid,
        Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
    > {
        unreachable!("the catalog only reads type schemas in batch")
    }
    async fn list_instances(
        &self,
        _query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<types_registry_sdk::GtsInstance>, toolkit_canonical_errors::CanonicalError>
    {
        unreachable!("the catalog only reads type schemas in batch")
    }
}

fn outage(batch: Batch) -> RegistryTypeCatalog {
    RegistryTypeCatalog::new(Arc::new(BatchOnly(batch)), Duration::from_millis(20))
}

#[tokio::test]
async fn an_outage_is_never_read_as_an_absence() {
    use crate::domain::ports::PortError;

    let ids = [WIDGET.to_owned()];
    assert_eq!(
        outage(Batch::Hang).known_types(&ids).await,
        Err(PortError::Timeout)
    );
    for batch in [Batch::FailFirst, Batch::Omit] {
        assert!(matches!(
            outage(batch).known_types(&ids).await,
            Err(PortError::Unavailable(_))
        ));
    }
}
