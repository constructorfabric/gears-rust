// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for [`GtsSecretTypeResolver`].

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use toolkit_gts::gts_id;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use super::*;
use crate::domain::ports::metrics::NoopMetrics;
use credstore_sdk::SharingMode;

const DRAFT: &str = "http://json-schema.org/draft-07/schema#";

/// Deterministic v5 UUID of a GTS type id, via the SDK helper.
fn uuid_of(gts_id: &str) -> Uuid {
    credstore_sdk::types::type_uuid(gts_id).expect("valid gts id")
}

/// The credstore secret base type as registered by the SDK: abstract (a pure
/// trait/schema carrier, secrets are always typed by a derived id) and
/// supplying no trait values: every non-abstract type declares the required
/// `allow_sharing` and `max_size_bytes` itself. The registry is seeded with it
/// so a derived type chains off it.
fn secret_envelope() -> Value {
    json!({
        "$id": format!("gts://{CREDENTIAL_RESOURCE_TYPE}"),
        "$schema": DRAFT,
        "type": "object",
        "x-gts-abstract": true,
        "x-gts-traits-schema": {
            "type": "object",
            "properties": {
                "allow_sharing": {"type": "array"},
                "max_size_bytes": {"type": "integer"},
                "expirable": {"type": "boolean", "default": false},
                "utf8_only": {"type": "boolean", "default": false},
                "value_schema": {"type": "object"},
            },
        },
    })
}

/// A derived secret-type schema declaring its own `x-gts-traits`.
fn derived(type_id: &str, traits: &Value) -> Value {
    json!({
        "$id": format!("gts://{type_id}"),
        "$schema": DRAFT,
        "allOf": [{"$ref": format!("gts://{CREDENTIAL_RESOURCE_TYPE}")}],
        "x-gts-traits": traits,
    })
}

/// A registry holding the secret base type and `schemas`.
fn registry_with(schemas: &[(&str, Value)]) -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(CREDENTIAL_RESOURCE_TYPE, secret_envelope());
    for (id, schema) in schemas {
        registry.seed(id, schema.clone());
    }
    registry
}

fn resolver(registry: Arc<MockTypesRegistry>) -> GtsSecretTypeResolver {
    GtsSecretTypeResolver::new(registry, Arc::new(NoopMetrics))
}

const CUSTOM_TYPE_ID: &str =
    gts_id!("cf.core.credstore.credential.v1~acme.connectors.creds.db_password.v1~");

#[tokio::test]
async fn resolves_gts_id_and_effective_traits() {
    let schema = derived(
        CUSTOM_TYPE_ID,
        &json!({
            "allow_sharing": ["private"],
            "expirable": true,
            "max_size_bytes": 8192,
            "value_schema": {"type": "object", "required": ["password"]},
        }),
    );
    let registry = registry_with(&[(CUSTOM_TYPE_ID, schema)]);
    let resolved = resolver(registry)
        .resolve(uuid_of(CUSTOM_TYPE_ID))
        .await
        .expect("registered custom type resolves");

    assert_eq!(resolved.gts_id, CUSTOM_TYPE_ID);
    assert!(resolved.traits.allows_sharing(SharingMode::Private));
    assert!(!resolved.traits.allows_sharing(SharingMode::Tenant));
    assert!(resolved.traits.expirable);
    assert_eq!(resolved.traits.max_size_bytes, 8192);
    // Undeclared optional traits take their schema defaults.
    assert!(!resolved.traits.utf8_only);
    assert_eq!(
        resolved.traits.value_schema,
        Some(json!({"type": "object", "required": ["password"]}))
    );
}

/// The built-in types, as the SDK declares them, resolve with the traits of
/// their catalog entries.
#[tokio::test]
async fn the_builtin_types_resolve_from_the_declared_inventory() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    let generic = credstore_sdk::types::SecretType::from_name("generic")
        .expect("catalog type")
        .descriptor();

    let resolved = resolver(registry)
        .resolve(uuid_of(generic.gts_id))
        .await
        .expect("a built-in type resolves");

    assert_eq!(resolved.gts_id, generic.gts_id);
    assert_eq!(resolved.traits, generic.traits());
}

/// The base type is abstract (`x-gts-abstract`): a trait carrier, never a
/// concrete secret type. The registry answer does not surface abstractness,
/// so the resolver rejects the base by its known id, with the same
/// `UNKNOWN_SECRET_TYPE` violation as a foreign type.
#[tokio::test]
async fn the_abstract_envelope_itself_is_rejected_as_a_secret_type() {
    let registry = registry_with(&[]);
    let err = resolver(registry)
        .resolve(uuid_of(CREDENTIAL_RESOURCE_TYPE))
        .await
        .expect_err("abstract base type must not type a secret");
    assert!(
        matches!(
            err,
            DomainError::TypeViolation {
                reason: reasons::UNKNOWN_SECRET_TYPE,
                ..
            }
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn unknown_uuid_is_an_unknown_secret_type_violation() {
    let registry = registry_with(&[]);
    let err = resolver(registry)
        .resolve(Uuid::from_u128(0xDEAD))
        .await
        .expect_err("unregistered uuid must reject");
    assert!(
        matches!(
            err,
            DomainError::TypeViolation {
                reason: reasons::UNKNOWN_SECRET_TYPE,
                ..
            }
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn non_secret_type_is_an_unknown_secret_type_violation() {
    // Registered and resolvable, but rooted outside the credstore secret
    // envelope: must not be admitted as a secret type even though its
    // traits could deserialize by luck.
    let alien_root = gts_id!("acme.core.events.type.v1~");
    let alien_id = gts_id!("acme.core.events.type.v1~acme.commerce.orders.order.v1~");
    let registry = registry_with(&[
        (
            alien_root,
            json!({
                "$id": format!("gts://{alien_root}"),
                "$schema": DRAFT,
                "type": "object",
            }),
        ),
        (
            alien_id,
            json!({
                "$id": format!("gts://{alien_id}"),
                "$schema": DRAFT,
                "allOf": [{"$ref": format!("gts://{alien_root}")}],
            }),
        ),
    ]);
    let err = resolver(registry)
        .resolve(uuid_of(alien_id))
        .await
        .expect_err("non-secret type must reject");
    assert!(
        matches!(
            err,
            DomainError::TypeViolation {
                reason: reasons::UNKNOWN_SECRET_TYPE,
                ..
            }
        ),
        "got: {err:?}"
    );
    assert!(err.to_string().contains("does not descend"), "got: {err}");
}

#[tokio::test]
async fn malformed_effective_traits_fail_closed_as_service_unavailable() {
    // `allow_sharing` declared with a value outside the SharingMode enum:
    // the registry would reject this at registration, so hitting it at
    // resolve time means the registered chain is broken: 503, not 400.
    let schema = derived(
        CUSTOM_TYPE_ID,
        &json!({"allow_sharing": ["everyone"], "max_size_bytes": 1024}),
    );
    let registry = registry_with(&[(CUSTOM_TYPE_ID, schema)]);
    let err = resolver(registry)
        .resolve(uuid_of(CUSTOM_TYPE_ID))
        .await
        .expect_err("malformed traits must fail closed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
    assert!(err.to_string().contains("malformed"), "got: {err}");
}

#[tokio::test]
async fn a_type_missing_a_required_trait_fails_closed_as_service_unavailable() {
    // `max_size_bytes` has no default and the base supplies none: a chain
    // that omits it (or `allow_sharing`) is a broken registration.
    for traits in [
        json!({"allow_sharing": ["private"]}),
        json!({"max_size_bytes": 1024}),
    ] {
        let schema = derived(CUSTOM_TYPE_ID, &traits);
        let registry = registry_with(&[(CUSTOM_TYPE_ID, schema)]);
        let err = resolver(registry)
            .resolve(uuid_of(CUSTOM_TYPE_ID))
            .await
            .expect_err("a missing required trait must fail closed");
        assert!(
            matches!(err, DomainError::ServiceUnavailable { .. }),
            "got: {err:?}"
        );
    }
}

// -------- transport / infra --------

#[tokio::test]
async fn registry_transport_error_maps_to_service_unavailable() {
    let registry = registry_with(&[]);
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let err = resolver(registry)
        .resolve(Uuid::from_u128(0x1))
        .await
        .expect_err("transport error must propagate as 503");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
    // Curated wire detail; the raw registry error stays in the cause chain.
    assert_eq!(
        err.to_string(),
        "service unavailable: types-registry unavailable"
    );
}

#[tokio::test(start_paused = true)]
async fn slow_registry_times_out_as_service_unavailable() {
    let registry = registry_with(&[]);
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_millis(50)));
    let resolver = GtsSecretTypeResolver::with_timeout(registry.clone(), Arc::new(NoopMetrics), 10);
    let err = resolver
        .resolve(Uuid::from_u128(0x1))
        .await
        .expect_err("slow registry must time out");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
    assert!(err.to_string().contains("timeout exceeded"), "got: {err}");
    assert_eq!(registry.calls(Call::BatchGet), 1);
}
