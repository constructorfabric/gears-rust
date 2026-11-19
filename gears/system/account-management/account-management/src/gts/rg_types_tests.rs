//! AM's RG types are AM-owned declarations admitted against the RG base.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::sync::Arc;

use resource_group_sdk::gts::MEMBERSHIP_TYPE_V1;
use serde_json::json;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, GtsTypeId, PayloadValidator, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, PublisherContext, ReadOptions, ReconcileOptions,
};

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RESOURCE_TYPE};

use super::USER_MEMBERSHIP_SCHEMA;

const RG_BASE: &str = toolkit_gts::gts_id!("cf.core.rg.type.v1~");
const DERIVED_USER_GROUP: &str =
    toolkit_gts::gts_id!("cf.core.rg.type.v1~cf.core.am.user_group.v1~cf.test.am.custom_group.v1~");
const DERIVED_USER: &str = toolkit_gts::gts_id!("cf.core.am.user.v1~cf.test.am.custom_user.v1~");

fn declaration(declarations: Vec<(String, String)>, id: &str) -> serde_json::Value {
    let (_, text) = declarations
        .into_iter()
        .find(|(declared, _)| declared == id)
        .unwrap_or_else(|| panic!("missing declaration for {id}"));
    serde_json::from_str(&text).expect("declared JSON")
}

async fn admitted_registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    let api: &dyn PlatformTypesRegistryApi = registry.as_ref();
    let platform = PlatformSecurityContext::outbound_marker();
    let desired: Vec<(GtsId, serde_json::Value)> = [
        (
            RG_BASE,
            declaration(resource_group_sdk::gts_declarations(), RG_BASE),
        ),
        (
            MEMBERSHIP_TYPE_V1,
            declaration(resource_group_sdk::gts_declarations(), MEMBERSHIP_TYPE_V1),
        ),
        (
            USER_RESOURCE_TYPE,
            declaration(
                account_management_sdk::gts_declarations(),
                USER_RESOURCE_TYPE,
            ),
        ),
        (
            USER_GROUP_RG_TYPE_CODE,
            declaration(crate::gts_declarations(), USER_GROUP_RG_TYPE_CODE),
        ),
        (
            USER_MEMBERSHIP_SCHEMA,
            declaration(crate::gts_declarations(), USER_MEMBERSHIP_SCHEMA),
        ),
    ]
    .into_iter()
    .map(|(id, doc)| (GtsId::try_new(id).expect("id"), doc))
    .collect();
    api.reconcile_entities_and_await(
        &platform,
        &PublisherContext {
            name: "account-management".to_owned(),
            version: "0.1.0".parse().expect("version"),
        },
        &desired,
        &ReconcileOptions::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("AM's RG types are admitted");
    registry
}

async fn validator(registry: &dyn PlatformTypesRegistryApi, id: &str) -> PayloadValidator {
    registry
        .get_type_schema(
            &PlatformSecurityContext::outbound_marker(),
            &GtsTypeId::new(id),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
            ]))),
        )
        .await
        .expect("admitted")
        .compile_payload_validator()
        .expect("validator")
}

#[tokio::test]
async fn user_groups_nest_and_accept_only_user_memberships() {
    let registry = admitted_registry().await;
    let group = validator(registry.as_ref(), USER_GROUP_RG_TYPE_CODE).await;
    group
        .validate(&json!({"parent_type":null}))
        .expect("root and absent metadata");
    group
        .validate(&json!({
            "parent_type": USER_GROUP_RG_TYPE_CODE,
            "metadata": {}
        }))
        .expect("same-type parent and empty metadata");
    group
        .validate(&json!({
            "parent_type": DERIVED_USER_GROUP
        }))
        .expect_err("a parent must have the exact AM user-group type");
    group
        .validate(&json!({"parent_type":USER_RESOURCE_TYPE}))
        .expect_err("users cannot parent groups");
    group
        .validate(&json!({
            "parent_type": null,
            "membership_type": "gts.cf.core.rg.membership.v1~cf.test.am.other_member.v1~"
        }))
        .expect_err("membership_type is not a group payload field");
    group
        .validate(&json!({}))
        .expect_err("parent_type is required");

    let membership = validator(registry.as_ref(), USER_MEMBERSHIP_SCHEMA).await;
    membership
        .validate(&json!({"resource_type": USER_RESOURCE_TYPE}))
        .expect("user resource type");
    membership
        .validate(&json!({"resource_type": DERIVED_USER}))
        .expect("derived user resource type");
    membership
        .validate(&json!({"resource_type": USER_GROUP_RG_TYPE_CODE}))
        .expect_err("groups cannot be members");
    membership
        .validate(&json!({"resource_type": USER_RESOURCE_TYPE, "resource_id": "user-42"}))
        .expect_err("membership schema accepts only resource_type");
}

#[tokio::test]
async fn user_group_metadata_rejects_unknown_fields() {
    let registry = admitted_registry().await;
    let group = validator(registry.as_ref(), USER_GROUP_RG_TYPE_CODE).await;
    group
        .validate(&json!({
            "parent_type": null,
            "metadata": {"extra": 1}
        }))
        .expect_err("v1 user groups declare no metadata fields");
}

/// Resource Group reads the binding out of AM's published user-group
/// declaration: the membership rule for a user group is AM's schema.
#[tokio::test]
async fn resource_group_resolves_the_user_group_binding() {
    use resource_group::domain::type_catalog::{RgTypeCatalog, TypeUse};
    use resource_group::infra::types_registry::RegistryTypeCatalog;

    let group_schema = declaration(crate::gts_declarations(), USER_GROUP_RG_TYPE_CODE);
    assert_eq!(
        group_schema["x-gts-traits-schema"]["properties"]["membership_type"]["const"],
        USER_MEMBERSHIP_SCHEMA,
        "descendants must keep AM's membership schema"
    );
    let registry = admitted_registry().await;
    let catalog = RegistryTypeCatalog::new(registry);
    let caller = toolkit_security::SecurityContext::builder()
        .subject_id(uuid::Uuid::now_v7())
        .subject_tenant_id(uuid::Uuid::now_v7())
        .build()
        .expect("caller");
    catalog
        .get_type(&caller, USER_GROUP_RG_TYPE_CODE, TypeUse::NewLink)
        .await
        .expect("an Active RG type");
    let rule = catalog
        .get_membership_rule(&caller, USER_GROUP_RG_TYPE_CODE, USER_RESOURCE_TYPE)
        .await
        .expect("the user-group membership rule");
    assert_eq!(rule.code, USER_MEMBERSHIP_SCHEMA);
}
