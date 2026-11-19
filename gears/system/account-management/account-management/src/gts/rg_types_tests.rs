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

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE};

use super::USER_MEMBERSHIP_SCHEMA;

const RG_BASE: &str = toolkit_gts::gts_id!("cf.core.rg.type.v1~");
const DERIVED_USER_GROUP: &str =
    toolkit_gts::gts_id!("cf.core.rg.type.v1~cf.core.am.user_group.v1~cf.test.am.custom_group.v1~");
const DERIVED_USER: &str =
    toolkit_gts::gts_id!("cf.core.rg.type.v1~cf.core.am.user.v1~cf.test.am.custom_user.v1~");

fn declaration(id: &str) -> serde_json::Value {
    let (_, text) = crate::gts_declarations()
        .into_iter()
        .find(|(declared, _)| declared == id)
        .unwrap_or_else(|| panic!("AM declares {id}"));
    serde_json::from_str(&text).expect("declared JSON")
}

fn rg_base(code: &str) -> serde_json::Value {
    let (_, text) = resource_group_sdk::gts_declarations()
        .into_iter()
        .find(|(declared, _)| declared == code)
        .expect("RG declares its base");
    serde_json::from_str(&text).expect("declared JSON")
}

#[test]
fn am_declares_its_user_group_container_and_member_handle() {
    let ids: Vec<String> = crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        ids.iter().any(|id| id == USER_GROUP_RG_TYPE_CODE),
        "{ids:?}"
    );
    assert!(ids.iter().any(|id| id == USER_RG_TYPE_CODE), "{ids:?}");
}

async fn admitted_registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    let api: &dyn PlatformTypesRegistryApi = registry.as_ref();
    let platform = PlatformSecurityContext::outbound_marker();
    let desired: Vec<(GtsId, serde_json::Value)> = [
        (RG_BASE, rg_base(RG_BASE)),
        (MEMBERSHIP_TYPE_V1, rg_base(MEMBERSHIP_TYPE_V1)),
        (USER_RG_TYPE_CODE, declaration(USER_RG_TYPE_CODE)),
        (
            USER_GROUP_RG_TYPE_CODE,
            declaration(USER_GROUP_RG_TYPE_CODE),
        ),
        (USER_MEMBERSHIP_SCHEMA, declaration(USER_MEMBERSHIP_SCHEMA)),
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
        .validate(&json!({"parent_type":null,"membership_type":USER_MEMBERSHIP_SCHEMA}))
        .expect("root and absent metadata");
    group
        .validate(&json!({
            "parent_type": USER_GROUP_RG_TYPE_CODE,
            "membership_type": USER_MEMBERSHIP_SCHEMA,
            "metadata": {}
        }))
        .expect("same-type parent and empty metadata");
    group
        .validate(&json!({
            "parent_type": DERIVED_USER_GROUP,
            "membership_type": USER_MEMBERSHIP_SCHEMA
        }))
        .expect("a derived user-group type can be a parent");
    group
        .validate(
            &json!({"parent_type":USER_RG_TYPE_CODE,"membership_type":USER_MEMBERSHIP_SCHEMA}),
        )
        .expect_err("users cannot parent groups");

    let membership = validator(registry.as_ref(), USER_MEMBERSHIP_SCHEMA).await;
    membership
        .validate(&json!({"resource_type": USER_RG_TYPE_CODE}))
        .expect("user resource type");
    membership
        .validate(&json!({"resource_type": DERIVED_USER}))
        .expect("derived user resource type");
    membership
        .validate(&json!({"resource_type": USER_GROUP_RG_TYPE_CODE}))
        .expect_err("groups cannot be members");
    membership
        .validate(&json!({"resource_type": USER_RG_TYPE_CODE, "resource_id": "user-42"}))
        .expect_err("membership schema accepts only resource_type");
}

#[tokio::test]
async fn user_group_metadata_rejects_unknown_fields() {
    let registry = admitted_registry().await;
    let group = validator(registry.as_ref(), USER_GROUP_RG_TYPE_CODE).await;
    group
        .validate(&json!({
            "parent_type": null,
            "membership_type": USER_MEMBERSHIP_SCHEMA,
            "metadata": {"extra": 1}
        }))
        .expect_err("v1 user groups declare no metadata fields");
}
