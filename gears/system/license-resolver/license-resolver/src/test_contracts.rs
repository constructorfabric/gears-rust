//! Licensing contracts standing in for a consuming Gear's published surface.
//!
//! Declared with the real `#[gts_type_schema]` macro rather than hand-written
//! JSON, so tests run against the schema shape a Gear actually registers. These
//! are external GTS types, not domain models, which is why they live outside the
//! domain layer.

use std::sync::Arc;

use gts::{GtsSchema, GtsTypeId};
use license_resolver_sdk::gts::{LicenseResourceV1, LicenseSubjectV1};
use serde_json::Value;
use toolkit_gts::gts_id;
use types_registry_sdk::GtsTypeSchema;

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

fn schema_of(type_id: &str, body: Value, parent: Option<Arc<GtsTypeSchema>>) -> GtsTypeSchema {
    GtsTypeSchema::try_new(GtsTypeId::new(type_id), body, None, parent)
        .expect("test contract type-schema is well formed")
}

/// The abstract Subject licensing base, exactly as the SDK publishes it.
#[must_use]
pub fn subject_base_schema() -> GtsTypeSchema {
    schema_of(
        SUBJECT_BASE,
        <LicenseSubjectV1<()> as GtsSchema>::gts_schema_with_refs(),
        None,
    )
}

/// The abstract Resource licensing base, exactly as the SDK publishes it.
#[must_use]
pub fn resource_base_schema() -> GtsTypeSchema {
    schema_of(
        RESOURCE_BASE,
        <LicenseResourceV1<()> as GtsSchema>::gts_schema_with_refs(),
        None,
    )
}

/// A derived contract with its licensing base linked as parent, so `ancestors`
/// and `effective_*` observe the whole chain the registry would resolve.
fn derived_schema<T: GtsSchema>(base: GtsTypeSchema) -> GtsTypeSchema {
    schema_of(T::TYPE_ID, T::gts_schema_with_refs(), Some(Arc::new(base)))
}

#[must_use]
pub fn user_subject_schema() -> GtsTypeSchema {
    derived_schema::<TestUserSubjectV1>(subject_base_schema())
}

/// A Subject contract derived from [`TestUserSubjectV1`], with the same shape.
pub const ADMIN_SUBJECT: &str =
    gts_id!("cf.core.lic.subj.v1~test.lic.gateway.user.v1~test.lic.gateway.admin.v1~");

#[must_use]
pub fn admin_subject_schema() -> GtsTypeSchema {
    let mut body = TestUserSubjectV1::gts_schema_with_refs();
    body["$id"] = Value::String(format!("gts://{ADMIN_SUBJECT}"));
    body["allOf"] = serde_json::json!([{ "$ref": format!("gts://{USER_SUBJECT}") }]);
    schema_of(ADMIN_SUBJECT, body, Some(Arc::new(user_subject_schema())))
}

#[must_use]
pub fn tenant_subject_schema() -> GtsTypeSchema {
    derived_schema::<TestTenantSubjectV1>(subject_base_schema())
}

#[must_use]
pub fn model_usage_schema() -> GtsTypeSchema {
    derived_schema::<TestModelUsageResourceV1>(resource_base_schema())
}

#[must_use]
pub fn bare_resource_schema() -> GtsTypeSchema {
    derived_schema::<TestBareResourceV1>(resource_base_schema())
}

#[must_use]
pub fn seat_subject_schema() -> GtsTypeSchema {
    derived_schema::<TestSeatSubjectV1>(subject_base_schema())
}

/// Principal types: a base, a user type under it, an operator subtype of user.
pub const PRINCIPAL_BASE: &str = gts_id!("cf.core.identity.principal.v1~");
pub const USER_PRINCIPAL: &str = gts_id!("cf.core.identity.principal.v1~cf.core.identity.user.v1~");
pub const OPERATOR_PRINCIPAL: &str =
    gts_id!("cf.core.identity.principal.v1~cf.core.identity.user.v1~test.op.identity.myuser.v1~");

fn principal_schema(type_id: &str, parent: Option<GtsTypeSchema>) -> GtsTypeSchema {
    let mut body = serde_json::json!({ "$id": format!("gts://{type_id}"), "type": "object" });
    if let Some(parent) = &parent {
        body["allOf"] =
            serde_json::json!([{ "$ref": format!("gts://{}", parent.type_id.as_ref()) }]);
    }
    schema_of(type_id, body, parent.map(Arc::new))
}

/// The principal-type chains as the registry resolves them.
#[must_use]
pub fn principal_schemas() -> [GtsTypeSchema; 3] {
    let base = principal_schema(PRINCIPAL_BASE, None);
    let user = principal_schema(USER_PRINCIPAL, Some(base.clone()));
    let operator = principal_schema(OPERATOR_PRINCIPAL, Some(user.clone()));
    [base, user, operator]
}
