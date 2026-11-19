//! AM's RG types are AM-owned declarations admitted against the RG base.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::sync::Arc;

use resource_group_sdk::gts::{MembershipTypeRef, ResourceGroupTypeTraitsV1, RgTypeRef};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, GtsTypeId, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, PublisherContext, ReadOptions, ReconcileOptions,
};

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE};

const RG_BASE: &str = toolkit_gts::gts_id!("cf.core.rg.type.v1~");

fn declaration(id: &str) -> serde_json::Value {
    let (_, text) = crate::gts_declarations()
        .into_iter()
        .find(|(declared, _)| declared == id)
        .unwrap_or_else(|| panic!("AM declares {id}"));
    serde_json::from_str(&text).expect("declared JSON")
}

fn rg_base() -> serde_json::Value {
    let (_, text) = resource_group_sdk::gts_declarations()
        .into_iter()
        .find(|(declared, _)| declared == RG_BASE)
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

async fn admitted_traits(id: &str) -> ResourceGroupTypeTraitsV1 {
    let registry = Arc::new(MockTypesRegistry::new());
    let api: &dyn PlatformTypesRegistryApi = registry.as_ref();
    let platform = PlatformSecurityContext::outbound_marker();
    let desired: Vec<(GtsId, serde_json::Value)> = [
        (RG_BASE, rg_base()),
        (USER_RG_TYPE_CODE, declaration(USER_RG_TYPE_CODE)),
        (
            USER_GROUP_RG_TYPE_CODE,
            declaration(USER_GROUP_RG_TYPE_CODE),
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
    let schema = api
        .get_type_schema(
            &platform,
            &GtsTypeId::new(id),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::EffectiveTraits,
            ]))),
        )
        .await
        .expect("admitted");
    serde_json::from_value(schema.effective_traits.expect("selected")).expect("RG traits")
}

#[tokio::test]
async fn the_container_nests_under_itself_and_admits_am_users() {
    assert_eq!(
        admitted_traits(USER_GROUP_RG_TYPE_CODE).await,
        ResourceGroupTypeTraitsV1 {
            can_be_root: true,
            allowed_parent_types: vec![RgTypeRef(USER_GROUP_RG_TYPE_CODE.to_owned())],
            allowed_membership_types: vec![MembershipTypeRef(USER_RG_TYPE_CODE.to_owned())],
        }
    );
}

#[tokio::test]
async fn the_member_handle_keeps_its_previous_placement() {
    assert_eq!(
        admitted_traits(USER_RG_TYPE_CODE).await,
        ResourceGroupTypeTraitsV1 {
            can_be_root: true,
            allowed_parent_types: Vec::new(),
            allowed_membership_types: Vec::new(),
        }
    );
}

#[tokio::test]
async fn user_group_metadata_stays_unconstrained() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(RG_BASE, rg_base());
    registry.seed(USER_RG_TYPE_CODE, declaration(USER_RG_TYPE_CODE));
    registry.seed(
        USER_GROUP_RG_TYPE_CODE,
        declaration(USER_GROUP_RG_TYPE_CODE),
    );
    let api: &dyn PlatformTypesRegistryApi = registry.as_ref();
    let validator = api
        .get_type_schema(
            &PlatformSecurityContext::outbound_marker(),
            &GtsTypeId::new(USER_GROUP_RG_TYPE_CODE),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
            ]))),
        )
        .await
        .expect("seeded")
        .compile_payload_validator()
        .expect("validator");
    validator
        .validate(&serde_json::json!({ "metadata": { "anything": [1, "two"] } }))
        .expect("the container never constrained metadata");
    validator
        .validate(&serde_json::json!({}))
        .expect("metadata is optional");
}
