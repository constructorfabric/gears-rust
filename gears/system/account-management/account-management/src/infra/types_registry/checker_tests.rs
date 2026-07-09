//! Tests for [`super::GtsTenantTypeChecker`].
//!
//! Extracted into a companion file per dylint `DE1101` (inline test
//! blocks > 100 lines must move out of the production source file).
//! The fakes here are local to the checker's unit tests; the
//! cross-gear `SlowRegistry` used by service-level integration
//! tests lives in `super::test_helpers`.

use super::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use types_registry_sdk::{GtsInstance, GtsTypeId, InstanceQuery, RegisterResult, TypeSchemaQuery};

/// Build a `GtsTypeSchema` for testing. `traits_schema` carries
/// the `allowed_parent_types` default (empty array per the canonical
/// AM envelope); `traits` is the leaf-declared override layer.
fn schema(
    type_id: &str,
    parent: Option<Arc<GtsTypeSchema>>,
    own_allowed_parents: Option<Vec<&str>>,
) -> Arc<GtsTypeSchema> {
    let mut raw = json!({
        "$id": format!("gts://{type_id}"),
        "type": "object",
    });
    if let Some(parents) = own_allowed_parents {
        raw["x-gts-traits"] = json!({
            ALLOWED_PARENT_TYPES_TRAIT: parents,
        });
    }
    let schema =
        GtsTypeSchema::try_new(GtsTypeId::new(type_id), raw, None, parent).expect("schema");
    Arc::new(schema)
}

/// Build the canonical envelope (`gts.cf.core.am.tenant_type.v1~`)
/// with the `x-gts-traits-schema` defaulting `allowed_parent_types`
/// to `[]`, mirroring `docs/schemas/tenant_type.v1.schema.json`.
fn envelope() -> Arc<GtsTypeSchema> {
    let raw = json!({
        "$id": format!("gts://{TENANT_TYPE_BASE_GTS_ID}"),
        "type": "object",
        "x-gts-traits-schema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                ALLOWED_PARENT_TYPES_TRAIT: {
                    "type": "array",
                    "items": { "type": "string" },
                    "default": [],
                },
            },
        },
    });
    Arc::new(
        GtsTypeSchema::try_new(GtsTypeId::new(TENANT_TYPE_BASE_GTS_ID), raw, None, None)
            .expect("envelope"),
    )
}

/// Test fake `TypesRegistryClient` returning canned schemas keyed
/// by UUID. The only methods the checker exercises are
/// `get_type_schemas_by_uuid` and (transitively, via timeout)
/// nothing else; the rest are `unreachable!()`.
struct FakeRegistry {
    schemas: Mutex<HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>>>,
    delay: Mutex<Option<Duration>>,
    calls: Mutex<u32>,
}

impl FakeRegistry {
    fn new(entries: Vec<(Uuid, Result<GtsTypeSchema, CanonicalError>)>) -> Self {
        Self {
            schemas: Mutex::new(entries.into_iter().collect()),
            delay: Mutex::new(None),
            calls: Mutex::new(0),
        }
    }

    fn with_delay(mut self, delay: Duration) -> Self {
        *self.delay.get_mut().expect("lock") = Some(delay);
        self
    }
}

#[async_trait]
impl TypesRegistryClient for FakeRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!()
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!()
    }
    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        unreachable!()
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        unreachable!("checker uses the batch variant")
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        unreachable!()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        *self.calls.lock().expect("lock") += 1;
        let delay = *self.delay.lock().expect("lock");
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        let map = self.schemas.lock().expect("lock");
        let mut out = HashMap::new();
        for u in type_uuids {
            let entry = map
                .get(&u)
                .cloned()
                .unwrap_or_else(|| Err(types_registry_sdk::testing::not_found(u.to_string())));
            out.insert(u, entry);
        }
        out
    }
    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        unreachable!()
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!()
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        unreachable!()
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        unreachable!()
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        unreachable!()
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        unreachable!()
    }
    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        unreachable!()
    }
}

// -------- happy path --------

#[tokio::test]
async fn admits_when_parent_in_child_allowed_parent_types() {
    let envelope = envelope();
    let parent_schema = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~",
        Some(envelope.clone()),
        None,
    );
    let child_schema = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~",
        Some(envelope),
        Some(vec![
            "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~",
        ]),
    );
    let parent_uuid = parent_schema.type_uuid;
    let child_uuid = child_schema.type_uuid;

    let registry = Arc::new(FakeRegistry::new(vec![
        (
            parent_uuid,
            Ok(Arc::try_unwrap(parent_schema).unwrap_or_else(|a| (*a).clone())),
        ),
        (
            child_uuid,
            Ok(Arc::try_unwrap(child_schema).unwrap_or_else(|a| (*a).clone())),
        ),
    ]));
    let checker = GtsTenantTypeChecker::new(registry.clone());

    checker
        .check_parent_child(parent_uuid, child_uuid)
        .await
        .expect("admit when parent listed");
    // Single batch call — checker MUST NOT issue per-uuid round-trips.
    assert_eq!(*registry.calls.lock().expect("lock"), 1);
}

// -------- same-type nesting --------

#[tokio::test]
async fn admits_same_type_nesting_when_self_listed() {
    let envelope = envelope();
    let nested = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.partner.v1~",
        Some(envelope),
        // Self-reference admits same-type nesting.
        Some(vec![
            "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.partner.v1~",
        ]),
    );
    let nested_uuid = nested.type_uuid;
    let registry = Arc::new(FakeRegistry::new(vec![(
        nested_uuid,
        Ok(Arc::try_unwrap(nested).unwrap_or_else(|a| (*a).clone())),
    )]));
    let checker = GtsTenantTypeChecker::new(registry);
    checker
        .check_parent_child(nested_uuid, nested_uuid)
        .await
        .expect("admit when self-nesting allowed");
}

#[tokio::test]
async fn rejects_same_type_nesting_when_self_not_listed() {
    let envelope = envelope();
    let leaf = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~",
        Some(envelope),
        // Self NOT in the list → same-type nesting disallowed.
        Some(vec![
            "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~",
        ]),
    );
    let leaf_uuid = leaf.type_uuid;
    let registry = Arc::new(FakeRegistry::new(vec![(
        leaf_uuid,
        Ok(Arc::try_unwrap(leaf).unwrap_or_else(|a| (*a).clone())),
    )]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(leaf_uuid, leaf_uuid)
        .await
        .expect_err("self-nesting must reject");
    assert!(matches!(err, DomainError::TypeNotAllowed { .. }));
    assert!(err.to_string().contains("same-type nesting"), "got: {err}");
}

// -------- rejection paths --------

#[tokio::test]
async fn rejects_when_parent_not_in_allowed_list() {
    let envelope = envelope();
    let parent_schema = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.stranger.v1~",
        Some(envelope.clone()),
        None,
    );
    let child_schema = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~",
        Some(envelope),
        Some(vec![
            "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~",
        ]),
    );
    let parent_uuid = parent_schema.type_uuid;
    let child_uuid = child_schema.type_uuid;

    let registry = Arc::new(FakeRegistry::new(vec![
        (parent_uuid, Ok((*parent_schema).clone())),
        (child_uuid, Ok((*child_schema).clone())),
    ]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(parent_uuid, child_uuid)
        .await
        .expect_err("parent not listed must reject");
    assert!(matches!(err, DomainError::TypeNotAllowed { .. }));
    assert_eq!(err.code(), "type_not_allowed");
}

#[tokio::test]
async fn rejects_child_not_under_envelope_as_invalid_tenant_type() {
    // Schema is registered, but its chain root is not the AM
    // tenant_type envelope — every membership check would be against
    // a meaningless trait map.
    let alien_root = schema("gts.acme.core.events.type.v1~", None, None);
    let alien_leaf = schema(
        "gts.acme.core.events.type.v1~acme.commerce.orders.order.v1~",
        Some(alien_root),
        Some(vec!["whatever"]),
    );
    let parent = Uuid::from_u128(0xDEAD);
    let child_uuid = alien_leaf.type_uuid;

    let registry = Arc::new(FakeRegistry::new(vec![
        (
            parent,
            Ok(GtsTypeSchema::try_new(
                GtsTypeId::new("gts.acme.core.events.type.v1~"),
                json!({"type": "object"}),
                None,
                None,
            )
            .expect("parent")),
        ),
        (child_uuid, Ok((*alien_leaf).clone())),
    ]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(parent, child_uuid)
        .await
        .expect_err("alien child must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("does not descend"), "got: {err}");
}

#[tokio::test]
async fn rejects_parent_not_under_envelope_as_invalid_tenant_type() {
    // Child is a proper tenant-type envelope descendant and would
    // accept the parent's `type_id` literally (its
    // `allowed_parent_types` contains the alien id), but the parent
    // resolves to a schema rooted in a different GTS namespace.
    // Without the envelope re-check on the parent the membership
    // would still match and admit a non-tenant-type parent; the
    // envelope re-check turns this into a clear `InvalidTenantType`.
    let envelope = envelope();
    let alien_root = schema("gts.acme.core.events.type.v1~", None, None);
    let alien_parent = schema(
        "gts.acme.core.events.type.v1~acme.commerce.orders.order.v1~",
        Some(alien_root),
        None,
    );
    let child_schema = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~",
        Some(envelope),
        // Deliberately list the alien parent's id — without the
        // envelope re-check the membership would match and admit.
        Some(vec![
            "gts.acme.core.events.type.v1~acme.commerce.orders.order.v1~",
        ]),
    );
    let parent_uuid = alien_parent.type_uuid;
    let child_uuid = child_schema.type_uuid;

    let registry = Arc::new(FakeRegistry::new(vec![
        (
            parent_uuid,
            Ok(Arc::try_unwrap(alien_parent).unwrap_or_else(|a| (*a).clone())),
        ),
        (
            child_uuid,
            Ok(Arc::try_unwrap(child_schema).unwrap_or_else(|a| (*a).clone())),
        ),
    ]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(parent_uuid, child_uuid)
        .await
        .expect_err("alien parent must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(
        err.to_string().contains("parent tenant type")
            && err.to_string().contains("does not descend"),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_child_not_registered_as_invalid_tenant_type() {
    let registry = Arc::new(FakeRegistry::new(vec![]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(Uuid::from_u128(0x1), Uuid::from_u128(0x2))
        .await
        .expect_err("unregistered child must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("child tenant type"), "got: {err}");
}

#[tokio::test]
async fn rejects_parent_not_registered_as_invalid_tenant_type() {
    let envelope = envelope();
    let child_schema = schema(
        "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~",
        Some(envelope),
        Some(vec![
            "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~",
        ]),
    );
    let child_uuid = child_schema.type_uuid;
    let parent_uuid = Uuid::from_u128(0xCAFE);

    let registry = Arc::new(FakeRegistry::new(vec![(
        child_uuid,
        Ok((*child_schema).clone()),
    )]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(parent_uuid, child_uuid)
        .await
        .expect_err("missing parent must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("parent tenant type"), "got: {err}");
}

// -------- malformed trait shapes --------

#[tokio::test]
async fn rejects_when_allowed_parent_types_contains_non_tenant_type_chained_id() {
    // Codex P2: a malformed entry that does not parse as a
    // tenant-type-shaped GTS id (i.e. does not end with `~`) MUST
    // collapse the whole trait list to `InvalidTenantType` rather
    // than silently admitting via a sibling well-formed entry.
    // Without this, an operator who fat-fingers an instance id
    // (e.g. `gts.acme.am.customer.v1~prod-acct-42`) into the array
    // alongside a legit type id would see the malformed entry
    // ignored at extraction time, and a parent whose chained id
    // happens to match the legit entry would still admit — masking
    // the misconfiguration.
    let envelope = envelope();
    // Child declares its own `allowed_parent_types` with a
    // non-type-shape entry (instance, missing trailing `~`) and
    // an otherwise legit type-shape entry. The whole list must
    // collapse to malformed.
    let raw = json!({
        "$id": "gts://gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.child.v1~",
        "type": "object",
        "x-gts-traits": {
            ALLOWED_PARENT_TYPES_TRAIT: [
                "gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.parent.v1~",
                "gts.cf.core.am.tenant_type.v1~acme.am.customer.v1~prod-acct-42",
            ],
        },
    });
    let child = GtsTypeSchema::try_new(
        GtsTypeId::new("gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.child.v1~"),
        raw,
        None,
        Some(envelope),
    )
    .expect("schema");
    let child_uuid = child.type_uuid;
    let registry = Arc::new(FakeRegistry::new(vec![(child_uuid, Ok(child))]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        // Same-type-nesting probe — only the child schema is
        // resolved, so the malformed list is the sole signal under
        // test (no parent-schema lookup confounds the assertion).
        .check_parent_child(child_uuid, child_uuid)
        .await
        .expect_err("malformed entry must collapse the list to InvalidTenantType");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
}

#[tokio::test]
async fn rejects_when_allowed_parent_types_is_not_an_array() {
    // Build a child whose own `x-gts-traits` declares a malformed
    // value (string instead of array). Leaf-declared values win,
    // overriding the envelope's `default: []`.
    let envelope = envelope();
    let raw = json!({
        "$id": "gts://gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.broken.v1~",
        "type": "object",
        "x-gts-traits": {
            ALLOWED_PARENT_TYPES_TRAIT: "not an array",
        },
    });
    let broken = GtsTypeSchema::try_new(
        GtsTypeId::new("gts.cf.core.am.tenant_type.v1~acme.am.tenant_type.broken.v1~"),
        raw,
        None,
        Some(envelope),
    )
    .expect("schema");
    let broken_uuid = broken.type_uuid;
    let registry = Arc::new(FakeRegistry::new(vec![(broken_uuid, Ok(broken))]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(Uuid::from_u128(0xAB), broken_uuid)
        .await
        .expect_err("malformed trait must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
}

// -------- transport / infra --------

#[tokio::test]
async fn rejects_when_registry_returns_unrecognised_error_as_service_unavailable() {
    let registry = Arc::new(FakeRegistry::new(vec![(
        Uuid::from_u128(0x1),
        Err(types_registry_sdk::testing::internal("registry exploded")),
    )]));
    let checker = GtsTenantTypeChecker::new(registry);
    let err = checker
        .check_parent_child(Uuid::from_u128(0x1), Uuid::from_u128(0x1))
        .await
        .expect_err("registry error must propagate");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(err.code(), "service_unavailable");
    assert_eq!(err.http_status(), 503);
}

#[tokio::test(start_paused = true)]
async fn rejects_when_registry_probe_times_out() {
    let registry = Arc::new(
        FakeRegistry::new(vec![(
            Uuid::from_u128(0x1),
            Err(types_registry_sdk::testing::internal("never reaches us")),
        )])
        .with_delay(Duration::from_millis(50)),
    );
    let checker = GtsTenantTypeChecker::with_timeout(registry.clone(), 10);
    let err = checker
        .check_parent_child(Uuid::from_u128(0x1), Uuid::from_u128(0x1))
        .await
        .expect_err("slow registry must time out");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert!(
        err.to_string().contains("types-registry: timeout exceeded"),
        "got: {err}"
    );
}
