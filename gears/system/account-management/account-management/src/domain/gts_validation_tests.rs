//! Unit tests for [`super::validate_new_user_payload_via_gts`] and
//! [`super::validate_tenant_name_via_gts`].
//!
//! Pin the documented failure-mode arms (mirrors the production
//! gear doc on `gts_validation.rs`):
//!
//! 1. Schema not registered (`CanonicalError::NotFound`):
//!    * `validate_new_user_payload_via_gts` →
//!      `DomainError::ServiceUnavailable` (fail-closed — no
//!      AM-side storage gate for users, so the boundary helper
//!      MUST be authoritative on `format` / `pattern` rules).
//!    * `validate_tenant_name_via_gts` → `Ok(())` (DB CHECK +
//!      bootstrap-before-catalog ordering remain authoritative).
//! 2. Other `CanonicalError` (transport / availability) →
//!    `DomainError::ServiceUnavailable`.
//! 3. Schema returned but `jsonschema::validator_for` rejects it
//!    (catalog drift) → `DomainError::Internal` — schema published
//!    by the deploy bundle is not a valid JSON Schema; operator
//!    action required. Exercised implicitly by the production
//!    `Internal` arm in the helpers.
//! 4. Schema returned + instance fails validation →
//!    `DomainError::Validation` (HTTP 400 — the client can retry
//!    with a corrected payload).
//! 5. Schema returned + valid instance → `Ok(())` (happy path).
//!
//! `MockTypesRegistryClient::with_type_schemas` is the seam used to
//! pin the registered-schema path; it pre-links the chain so
//! `effective_properties()` returns the merged property map the
//! helper validates against.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use account_management_sdk::IdpNewUser;
use async_trait::async_trait;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{
    GtsInstance, GtsTypeId, GtsTypeSchema, RegisterResult, TypesRegistryClient,
};
use uuid::Uuid;

use crate::domain::error::DomainError;

// ---- helpers -------------------------------------------------------

fn user_payload(username: &str) -> IdpNewUser {
    IdpNewUser::new(username.to_owned())
}

fn user_schema_with_username_max(max_chars: usize) -> GtsTypeSchema {
    let body = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "username"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "username": {
                "type": "string",
                "minLength": 1,
                "maxLength": max_chars,
            },
            "email": { "type": "string", "format": "email" },
            "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
        },
    });
    GtsTypeSchema::try_new(GtsTypeId::new(super::USER_TYPE_ID), body, None, None)
        .expect("synthetic user schema is valid")
}

fn tenant_schema_with_name_max(max_chars: usize) -> GtsTypeSchema {
    let body = json!({
        "type": "object",
        "additionalProperties": true,
        "required": ["id", "name"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "name": {
                "type": "string",
                "minLength": 1,
                "maxLength": max_chars,
            },
            "parent_id": { "type": ["string", "null"], "format": "uuid" },
        },
    });
    GtsTypeSchema::try_new(GtsTypeId::new(super::TENANT_TYPE_ID), body, None, None)
        .expect("synthetic tenant schema is valid")
}

/// Registry stub: every read returns `ServiceUnavailable`. Used to
/// pin the transport-error arm of the GTS-validation helper without
/// reaching for the heavier `MockTypesRegistryClient` (which always
/// returns `CanonicalError::NotFound` for unknown ids and offers no
/// inject-error seam).
///
/// Methods that the helper does not exercise return empty / not-found
/// to keep the impl narrow; a full `TypesRegistryClient` mock lives in
/// `types-registry-sdk::testing`.
#[derive(Debug, Default)]
struct UnavailableRegistry;

fn unavailable() -> CanonicalError {
    // Model the scenario under test directly: a temporarily unavailable
    // registry. The production helper short-circuits only `NotFound` and
    // routes every other canonical category (this one included) to its
    // `ServiceUnavailable` arm.
    CanonicalError::service_unavailable()
        .with_detail("registry transport down (test stub)")
        .create()
}

#[async_trait]
impl TypesRegistryClient for UnavailableRegistry {
    async fn register(&self, _entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unavailable())
    }
    async fn register_type_schemas(
        &self,
        _schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unavailable())
    }
    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        Err(unavailable())
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        Err(unavailable())
    }
    async fn get_type_schemas(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _ids: Vec<Uuid>,
    ) -> std::collections::HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        Ok(Vec::new())
    }
    async fn register_instances(
        &self,
        _instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unavailable())
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        Err(unavailable())
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        Err(unavailable())
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<String, Result<GtsInstance, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn get_instances_by_uuid(
        &self,
        _ids: Vec<Uuid>,
    ) -> std::collections::HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn list_instances(
        &self,
        _query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        Ok(Vec::new())
    }
}

// ---- validate_new_user_payload_via_gts ----------------------------

#[tokio::test]
async fn user_payload_schema_not_registered_surfaces_service_unavailable() {
    let registry = MockTypesRegistryClient::new();
    let payload = user_payload("alice");
    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err(
            "schema-not-found MUST fail closed: provision_user has no AM-side \
             fallback gate and degrading to length-only checks would silently \
             disable format/pattern rules until the catalog is seeded",
        );
    match err {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert!(
                detail.contains(super::USER_TYPE_ID) && detail.contains("catalog"),
                "ServiceUnavailable.detail must name the missing schema and the \
                 catalog-seed remediation; got: {detail}"
            );
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn user_payload_valid_username_passes_registered_schema() {
    let registry =
        MockTypesRegistryClient::new().with_type_schemas([user_schema_with_username_max(255)]);
    let payload = user_payload("alice");
    super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect("valid username passes the registered schema");
}

#[tokio::test]
async fn user_payload_oversized_username_rejects_with_validation() {
    let registry =
        MockTypesRegistryClient::new().with_type_schemas([user_schema_with_username_max(255)]);
    let payload = user_payload(&"x".repeat(256));
    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err("oversize username must be rejected by the registered schema");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("username"),
                "diagnostic must name the violating field; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn user_payload_registry_transport_error_surfaces_as_service_unavailable() {
    let registry = UnavailableRegistry;
    let payload = user_payload("alice");
    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err("registry transport error must surface");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}

// ---- validate_tenant_name_via_gts ---------------------------------

#[tokio::test]
async fn tenant_name_schema_not_registered_short_circuits_to_ok() {
    let registry = MockTypesRegistryClient::new();
    super::validate_tenant_name_via_gts("acme", &registry)
        .await
        .expect("schema-not-found short-circuits to Ok");
}

#[tokio::test]
async fn tenant_name_valid_passes_registered_schema() {
    let registry =
        MockTypesRegistryClient::new().with_type_schemas([tenant_schema_with_name_max(255)]);
    super::validate_tenant_name_via_gts("acme", &registry)
        .await
        .expect("valid name passes the registered schema");
}

#[tokio::test]
async fn tenant_name_oversized_rejects_with_validation() {
    let registry =
        MockTypesRegistryClient::new().with_type_schemas([tenant_schema_with_name_max(255)]);
    let oversized = "x".repeat(256);
    let err = super::validate_tenant_name_via_gts(&oversized, &registry)
        .await
        .expect_err("oversize name must be rejected");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("name"),
                "diagnostic must name the violating field; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn tenant_name_empty_rejects_with_validation() {
    let registry =
        MockTypesRegistryClient::new().with_type_schemas([tenant_schema_with_name_max(255)]);
    let err = super::validate_tenant_name_via_gts("", &registry)
        .await
        .expect_err("empty name must be rejected");
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn tenant_name_registry_transport_error_surfaces_as_service_unavailable() {
    let registry = UnavailableRegistry;
    let err = super::validate_tenant_name_via_gts("acme", &registry)
        .await
        .expect_err("registry transport error must surface");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}
