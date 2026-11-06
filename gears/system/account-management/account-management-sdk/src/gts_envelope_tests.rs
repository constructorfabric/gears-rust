//! Behavioural guards for the envelope trait contracts, exercised through
//! `gts::GtsStore` the way the types-registry admits derived schemas.

use gts::{GtsRefValidation, GtsSchema, GtsStore};
use serde_json::{Value, json};

use super::{TenantMetadataEnvelopeV1, TenantTypeEnvelopeV1};

const TYPE_BASE: &str = "gts.cf.core.am.tenant_type.v1~";
const TYPE_CHILD: &str = "gts.cf.core.am.tenant_type.v1~cf.test.am.child.v1~";
const META_BASE: &str = "gts.cf.core.am.tenant_metadata.v1~";
const META_CHILD: &str = "gts.cf.core.am.tenant_metadata.v1~cf.test.am.meta.v1~";

/// Derived schema shaped like `desired_root_schema`: `$ref` to the base
/// plus `x-gts-traits` (omitted when `traits` is `None`).
fn derived(type_id: &str, base: &str, traits: Option<Value>) -> Value {
    let mut schema = json!({
        "$id": format!("gts://{type_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{base}") }],
    });
    if let Some(traits) = traits {
        schema["x-gts-traits"] = traits;
    }
    schema
}

/// Register `base_schema` and a derived schema, then validate the derived
/// one under `mode`.
fn admit(
    base_schema: &Value,
    base: &str,
    type_id: &str,
    traits: Option<Value>,
    mode: GtsRefValidation,
) -> Result<(), String> {
    let mut store = GtsStore::new();
    store
        .register_schema(base, base_schema)
        .expect("register base");
    store
        .register_schema(type_id, &derived(type_id, base, traits))
        .expect("register derived");
    store
        .validate_schema_with(type_id, mode)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn admit_tenant_type(traits: Option<Value>, mode: GtsRefValidation) -> Result<(), String> {
    admit(
        &TenantTypeEnvelopeV1::<()>::gts_schema_with_refs(),
        TYPE_BASE,
        TYPE_CHILD,
        traits,
        mode,
    )
}

fn admit_metadata(traits: Option<Value>) -> Result<(), String> {
    admit(
        &TenantMetadataEnvelopeV1::<()>::gts_schema_with_refs(),
        META_BASE,
        META_CHILD,
        traits,
        GtsRefValidation::None,
    )
}

#[test]
fn abstract_envelopes_validate_on_their_own() {
    for (base, schema) in [
        (
            TYPE_BASE,
            TenantTypeEnvelopeV1::<()>::gts_schema_with_refs(),
        ),
        (
            META_BASE,
            TenantMetadataEnvelopeV1::<()>::gts_schema_with_refs(),
        ),
    ] {
        let mut store = GtsStore::new();
        store.register_schema(base, &schema).expect("register");
        store
            .validate_schema_with(base, GtsRefValidation::None)
            .unwrap_or_else(|e| panic!("{base}: {e}"));
    }
}

/// Every tenant type and metadata schema is a type derived from its envelope;
/// the envelopes themselves never carry instances, so the registry must refuse
/// one validated directly against them.
#[test]
fn the_envelopes_take_no_direct_instances() {
    for (base, schema) in [
        (
            TYPE_BASE,
            TenantTypeEnvelopeV1::<()>::gts_schema_with_refs(),
        ),
        (
            META_BASE,
            TenantMetadataEnvelopeV1::<()>::gts_schema_with_refs(),
        ),
    ] {
        let mut store = GtsStore::new();
        store.register_schema(base, &schema).expect("register");
        let instance = json!({ "id": format!("{base}cf.test.am.direct.v1"), "payload": {} });
        let err = store
            .validate_payload_with(base, &instance, GtsRefValidation::None)
            .expect_err("an envelope takes no direct instances");
        assert!(err.to_string().contains("abstract"), "{base}: {err}");
    }
}

#[test]
fn tenant_type_accepts_valid_traits() {
    for mode in [GtsRefValidation::None, GtsRefValidation::AnyPresent] {
        admit_tenant_type(
            Some(json!({
                "allowed_parent_types": [TYPE_CHILD],
                "idp_provisioning": true,
            })),
            mode,
        )
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
    }
}

#[test]
fn tenant_type_rejects_foreign_parent_type() {
    admit_tenant_type(
        Some(json!({
            "allowed_parent_types": ["gts.cf.core.other.thing.v1~"],
            "idp_provisioning": false,
        })),
        GtsRefValidation::None,
    )
    .expect_err("foreign type id must be rejected");
}

#[test]
fn tenant_type_rejects_unknown_trait_key() {
    admit_tenant_type(
        Some(json!({ "allowed_parent_types": [], "bogus": 1 })),
        GtsRefValidation::None,
    )
    .expect_err("unknown trait must be rejected");
}

#[test]
fn tenant_type_rejects_non_boolean_idp_provisioning() {
    admit_tenant_type(
        Some(json!({ "allowed_parent_types": [], "idp_provisioning": "yes" })),
        GtsRefValidation::None,
    )
    .expect_err("string idp_provisioning must be rejected");
}

#[test]
fn tenant_type_omitted_traits_fall_back_to_defaults() {
    admit_tenant_type(None, GtsRefValidation::None).expect("no traits");
    admit_tenant_type(
        Some(json!({ "idp_provisioning": true })),
        GtsRefValidation::None,
    )
    .expect("partial traits");
}

#[test]
fn tenant_metadata_inheritance_policy_is_enforced() {
    for ok in ["override_only", "inherit"] {
        admit_metadata(Some(json!({ "inheritance_policy": ok }))).expect(ok);
    }
    admit_metadata(Some(json!({ "inheritance_policy": "bogus" })))
        .expect_err("bogus policy must be rejected");
}

#[test]
fn tenant_metadata_omitted_traits_fall_back_to_defaults() {
    admit_metadata(None).expect("no traits");
}
