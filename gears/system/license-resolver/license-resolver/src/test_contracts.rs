//! Licensing contracts standing in for a consuming Gear's published surface.
//!
//! Declared with the real `#[gts_type_schema]` macro rather than hand-written
//! JSON, so tests run against the schema shape a Gear actually registers. These
//! are external GTS types, not domain models, which is why they live outside the
//! domain layer.

use gts::{GtsRefValidation, GtsSchema, GtsStore, GtsTypeId};
use license_resolver_sdk::gts::{LicenseResourceV1, LicenseSubjectV1};
use serde_json::Value;
use toolkit_gts::gts_id;
use types_registry_sdk::{LifecycleStatus, TypeSchema};

/// Subject contract: `metadata: { category: string }`.
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = LicenseSubjectV1,
    type_id = gts_id!("cf.core.lic.subj.v1~test.lic.gateway.user.v1~"),
    description = "test Subject licensing contract",
    properties = "category"
)]
#[derive(Default)]
pub struct TestUserSubjectV1 {
    category: String,
}

/// Subject contract without metadata, the target of principal mappings.
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = LicenseSubjectV1,
    type_id = gts_id!("cf.core.lic.subj.v1~test.lic.gateway.seat.v1~"),
    description = "test Subject licensing contract without metadata",
    properties = ""
)]
#[derive(Default)]
pub struct TestSeatSubjectV1;

/// Resource contract admitting [`TestUserSubjectV1`] and [`TestSeatSubjectV1`].
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = LicenseResourceV1,
    type_id = gts_id!("cf.core.lic.res.v1~test.lic.gateway.model_usage.v1~"),
    description = "test Resource licensing contract",
    properties = "model_vendor,model_name",
    traits = serde_json::json!({
        "admitted_subjects": [
            gts_id!("cf.core.lic.subj.v1~test.lic.gateway.user.v1~"),
            gts_id!("cf.core.lic.subj.v1~test.lic.gateway.seat.v1~"),
        ]
    })
)]
#[derive(Default)]
pub struct TestModelUsageResourceV1 {
    model_vendor: String,
    model_name: String,
}

/// A second Subject contract, deliberately *not* admitted by
/// [`TestModelUsageResourceV1`].
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = LicenseSubjectV1,
    type_id = gts_id!("cf.core.lic.subj.v1~test.lic.gateway.tenant.v1~"),
    description = "test Subject licensing contract that no test Resource admits",
    properties = "tier"
)]
#[derive(Default)]
pub struct TestTenantSubjectV1 {
    tier: String,
}

/// Resource contract that declares no `traits` at all, so trait resolution falls
/// back to the abstract base's `admitted_subjects: []`.
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = LicenseResourceV1,
    type_id = gts_id!("cf.core.lic.res.v1~test.lic.gateway.bare.v1~"),
    description = "test Resource contract declaring no admitted subjects",
    properties = "model_name"
)]
#[derive(Default)]
pub struct TestBareResourceV1 {
    model_name: String,
}

pub const SUBJECT_BASE: &str = <LicenseSubjectV1<()> as GtsSchema>::TYPE_ID;
pub const RESOURCE_BASE: &str = <LicenseResourceV1<()> as GtsSchema>::TYPE_ID;
pub const USER_SUBJECT: &str = <TestUserSubjectV1 as GtsSchema>::TYPE_ID;
pub const TENANT_SUBJECT: &str = <TestTenantSubjectV1 as GtsSchema>::TYPE_ID;
pub const SEAT_SUBJECT: &str = <TestSeatSubjectV1 as GtsSchema>::TYPE_ID;
pub const MODEL_USAGE_RESOURCE: &str = <TestModelUsageResourceV1 as GtsSchema>::TYPE_ID;
pub const BARE_RESOURCE: &str = <TestBareResourceV1 as GtsSchema>::TYPE_ID;

/// A contract as the registry serves it: its own document, the chain
/// resolved (`resolved_schema`) and the merged traits (`effective_traits`),
/// materialized with `gts-rust` over `chain` (parents first, `type_id` last).
fn materialize(type_id: &str, chain: &[(&str, Value)]) -> TypeSchema {
    let mut store = GtsStore::new();
    for (id, body) in chain {
        store
            .register_schema(id, body)
            .expect("test contract registers");
    }
    let resolved = store
        .validate_schema_with(type_id, GtsRefValidation::None)
        .expect("test contract resolves");
    let own = chain
        .iter()
        .find(|(id, _)| *id == type_id)
        .map(|(_, body)| body.clone())
        .expect("the contract is in its chain");
    TypeSchema {
        type_id: GtsTypeId::new(type_id),
        type_uuid: uuid::Uuid::nil(),
        lifecycle_status: LifecycleStatus::Active,
        ownership: types_registry_sdk::Ownership::Global,
        origin: None,
        content: Some(own),
        resolved_schema: Some(resolved.schema),
        effective_traits: Some(resolved.effective_traits),
        effective_traits_schema: None,
        provenance: None,
    }
}

fn subject_base() -> (&'static str, Value) {
    (
        SUBJECT_BASE,
        <LicenseSubjectV1<()> as GtsSchema>::gts_schema_with_refs(),
    )
}

fn resource_base() -> (&'static str, Value) {
    (
        RESOURCE_BASE,
        <LicenseResourceV1<()> as GtsSchema>::gts_schema_with_refs(),
    )
}

#[must_use]
pub fn subject_base_schema() -> TypeSchema {
    materialize(SUBJECT_BASE, &[subject_base()])
}

#[must_use]
pub fn resource_base_schema() -> TypeSchema {
    materialize(RESOURCE_BASE, &[resource_base()])
}

/// The resource contract `body` (registered as [`MODEL_USAGE_RESOURCE`]) over
/// the resource base.
#[must_use]
pub fn model_usage_schema_with(body: Value) -> TypeSchema {
    materialize(
        MODEL_USAGE_RESOURCE,
        &[resource_base(), (MODEL_USAGE_RESOURCE, body)],
    )
}

#[must_use]
pub fn user_subject_schema() -> TypeSchema {
    materialize(
        USER_SUBJECT,
        &[
            subject_base(),
            (USER_SUBJECT, TestUserSubjectV1::gts_schema_with_refs()),
        ],
    )
}

pub const ADMIN_SUBJECT: &str =
    gts_id!("cf.core.lic.subj.v1~test.lic.gateway.user.v1~test.lic.gateway.admin.v1~");

#[must_use]
pub fn admin_subject_schema() -> TypeSchema {
    let mut body = TestUserSubjectV1::gts_schema_with_refs();
    body["$id"] = Value::String(format!("gts://{ADMIN_SUBJECT}"));
    body["allOf"] = serde_json::json!([{ "$ref": format!("gts://{USER_SUBJECT}") }]);
    materialize(
        ADMIN_SUBJECT,
        &[
            subject_base(),
            (USER_SUBJECT, TestUserSubjectV1::gts_schema_with_refs()),
            (ADMIN_SUBJECT, body),
        ],
    )
}

#[must_use]
pub fn tenant_subject_schema() -> TypeSchema {
    materialize(
        TENANT_SUBJECT,
        &[
            subject_base(),
            (TENANT_SUBJECT, TestTenantSubjectV1::gts_schema_with_refs()),
        ],
    )
}

#[must_use]
pub fn model_usage_schema() -> TypeSchema {
    model_usage_schema_with(TestModelUsageResourceV1::gts_schema_with_refs())
}

#[must_use]
pub fn bare_resource_schema() -> TypeSchema {
    materialize(
        BARE_RESOURCE,
        &[
            resource_base(),
            (BARE_RESOURCE, TestBareResourceV1::gts_schema_with_refs()),
        ],
    )
}

#[must_use]
pub fn seat_subject_schema() -> TypeSchema {
    materialize(
        SEAT_SUBJECT,
        &[
            subject_base(),
            (SEAT_SUBJECT, TestSeatSubjectV1::gts_schema_with_refs()),
        ],
    )
}

/// Principal types: a base, a user type under it, an operator subtype of user.
pub const PRINCIPAL_BASE: &str = gts_id!("cf.core.identity.principal.v1~");
pub const USER_PRINCIPAL: &str = gts_id!("cf.core.identity.principal.v1~cf.core.identity.user.v1~");
pub const OPERATOR_PRINCIPAL: &str =
    gts_id!("cf.core.identity.principal.v1~cf.core.identity.user.v1~test.op.identity.myuser.v1~");

fn principal_document(type_id: &'static str, parent: Option<&str>) -> (&'static str, Value) {
    let mut body = serde_json::json!({
        "$id": format!("gts://{type_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object"
    });
    if let Some(parent) = parent {
        body["allOf"] = serde_json::json!([{ "$ref": format!("gts://{parent}") }]);
    }
    (type_id, body)
}

/// The principal-type chains as the registry resolves them.
#[must_use]
pub fn principal_schemas() -> [TypeSchema; 3] {
    let chain = [
        principal_document(PRINCIPAL_BASE, None),
        principal_document(USER_PRINCIPAL, Some(PRINCIPAL_BASE)),
        principal_document(OPERATOR_PRINCIPAL, Some(USER_PRINCIPAL)),
    ];
    [
        materialize(PRINCIPAL_BASE, &chain[..1]),
        materialize(USER_PRINCIPAL, &chain[..2]),
        materialize(OPERATOR_PRINCIPAL, &chain),
    ]
}
