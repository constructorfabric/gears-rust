//! [`RegistryTypeCatalog`] over `MockTypesRegistry`, which materializes
//! `resolved_schema` and `effective_traits` with `gts-rust` as admission does.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{gts_id, gts_uri};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, EntityLookup, ListEntitiesRequest,
    ListEntitiesResponse, TypesRegistryApi,
};
use uuid::Uuid;

use super::RegistryTypeCatalog;
use crate::domain::error::DomainError;
use crate::domain::type_catalog::{RgTypeCatalog, TypeUse};

#[toolkit_canonical_errors::resource_error(gts_id!("cf.core.types_registry.entity.v1~"))]
struct Catalogue;

const BASE: &str = gts_id!("cf.core.rg.type.v1~");
const USER: &str = gts_id!("cf.test.rg.user.v1~");
const TENANT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~");
const DEPT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~");

fn rg_base() -> Value {
    let (_, text) = resource_group_sdk::gts_declarations()
        .into_iter()
        .find(|(id, _)| id == BASE)
        .expect("the RG SDK declares the base");
    serde_json::from_str(&text).expect("declared JSON")
}

fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(BASE, rg_base());
    registry.seed(
        USER,
        json!({
            "$id": gts_uri!("cf.test.rg.user.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object"
        }),
    );
    registry.seed(
        TENANT,
        json!({
            "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "x-gts-traits": {
                "can_be_root": true,
                "allowed_parent_types": [TENANT],
                "allowed_membership_types": [USER]
            },
            "$defs": { "tenant_meta": {
                "type": "object",
                "required": ["tier"],
                "properties": { "tier": { "type": "string" } }
            } },
            "allOf": [
                { "$ref": gts_uri!("cf.core.rg.type.v1~") },
                { "properties": { "metadata": { "$ref": "#/$defs/tenant_meta" } } }
            ]
        }),
    );
    registry.seed(
        DEPT,
        json!({
            "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "x-gts-traits": {
                "can_be_root": false,
                "allowed_parent_types": [TENANT],
                "allowed_membership_types": []
            },
            "allOf": [
                { "$ref": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~") },
                { "properties": { "metadata": {
                    "type": "object",
                    "properties": { "code": { "type": "string", "maxLength": 3 } }
                } } }
            ]
        }),
    );
    registry
}

fn caller(n: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xC0FF_EE00_0000 + n))
        .subject_type("user")
        .subject_tenant_id(Uuid::from_u128(0x7E4A_4700_0000 + n))
        .bearer_token(format!("bearer-{n}"))
        .build()
        .expect("caller context")
}

fn catalog(registry: Arc<dyn TypesRegistryApi>) -> RegistryTypeCatalog {
    RegistryTypeCatalog::new(registry)
}

fn delete(registry: &MockTypesRegistry, id: &str) {
    let content = registry.content(id).expect("seeded");
    registry.seed_entity(Seed::new(id, content).deleted());
}

// -------- traits --------

#[tokio::test]
async fn reads_the_materialized_placement_traits() {
    let t = catalog(registry())
        .get_type(&caller(1), DEPT, TypeUse::NewLink)
        .await
        .expect("active type");
    assert_eq!(t.code, DEPT);
    assert!(!t.traits.can_be_root);
    assert!(t.allows_parent(TENANT));
    assert!(!t.allows_parent(DEPT));
    assert!(!t.allows_member(USER));

    let tenant = catalog(registry())
        .get_type(&caller(1), TENANT, TypeUse::NewLink)
        .await
        .expect("active type");
    assert!(tenant.traits.can_be_root);
    assert!(tenant.allows_parent(TENANT), "self nesting");
    assert!(tenant.allows_member(USER));
}

#[tokio::test]
async fn reads_are_made_for_the_caller() {
    let registry = registry();
    catalog(registry.clone())
        .get_type(&caller(7), TENANT, TypeUse::NewLink)
        .await
        .expect("active type");
    let contexts = registry.caller_contexts(Call::BatchGet);
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].subject_id(), caller(7).subject_id());
    assert_eq!(
        registry.platform_calls(Call::BatchGet),
        0,
        "never the platform plane"
    );
}

// -------- lifecycle --------

#[tokio::test]
async fn an_unknown_type_is_type_not_found_for_every_use() {
    for usage in [TypeUse::NewLink, TypeUse::ExistingGroup] {
        let err = catalog(registry())
            .get_type(
                &caller(1),
                gts_id!("cf.core.rg.type.v1~cf.test.rg.nope.v1~"),
                usage,
            )
            .await
            .expect_err("unknown");
        assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
    }
}

#[tokio::test]
async fn a_deleted_type_takes_no_new_link_but_still_defines_its_groups() {
    let registry = registry();
    delete(&registry, TENANT);
    let err = catalog(registry.clone())
        .get_type(&caller(1), TENANT, TypeUse::NewLink)
        .await
        .expect_err("deleted");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");

    let retained = catalog(registry)
        .get_type(&caller(1), TENANT, TypeUse::ExistingGroup)
        .await
        .expect("a deleted type's definition is retained");
    assert!(retained.traits.can_be_root);
    retained
        .validate_metadata(Some(&json!({ "tier": "gold" })))
        .expect("its schema still applies");
}

// -------- failures --------

#[tokio::test]
async fn a_refused_caller_keeps_the_registry_refusal() {
    for refusal in [
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_EXPIRED")
            .create(),
        Catalogue::permission_denied()
            .with_reason("READ_NOT_GRANTED")
            .create(),
    ] {
        let registry = registry();
        registry.inject(Fault::on(Call::BatchGet).fail(refusal.clone()));
        let err = catalog(registry)
            .get_type(&caller(1), TENANT, TypeUse::NewLink)
            .await
            .expect_err("refused");
        let DomainError::RegistryRefusedCaller(kept) = &err else {
            panic!("{err:?}");
        };
        assert_eq!(kept.status_code(), refusal.status_code());
        assert_eq!(
            CanonicalError::from(err).status_code(),
            refusal.status_code(),
            "401/403 reach the caller unmasked"
        );
    }
}

#[tokio::test]
async fn an_unavailable_registry_is_service_unavailable() {
    let registry = registry();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let err = catalog(registry)
        .get_type(&caller(1), TENANT, TypeUse::NewLink)
        .await
        .expect_err("outage");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(CanonicalError::from(err).status_code(), 503);
}

// -------- payload validation --------

#[tokio::test]
async fn metadata_is_validated_by_the_sdk_against_the_inherited_resolved_schema() {
    let dept = catalog(registry())
        .get_type(&caller(1), DEPT, TypeUse::NewLink)
        .await
        .expect("active type");
    dept.validate_metadata(Some(&json!({ "tier": "gold", "code": "ab" })))
        .expect("conforms");
    dept.validate_metadata(None).expect("metadata is optional");
    for invalid in [
        json!({ "code": "ab" }),
        json!({ "tier": 7 }),
        json!({ "tier": "gold", "code": "abcd" }),
        json!("not an object"),
    ] {
        let err = dept
            .validate_metadata(Some(&invalid))
            .expect_err("the inherited ($defs-referenced) and own constraints apply");
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{invalid}: {err:?}"
        );
    }
}

/// A registry whose snapshots answer an edited `resolved_schema`: resource-group
/// must enforce the registry's materialization, not a schema it derives itself
/// from the authored content.
struct ResolvedOverride {
    inner: Arc<MockTypesRegistry>,
    resolved: Value,
}

#[async_trait]
impl TypesRegistryApi for ResolvedOverride {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let mut response = self.inner.batch_get_entities(ctx, request).await?;
        for lookup in response.0.values_mut() {
            if let EntityLookup::Found { entity, .. } = lookup {
                entity.resolved_schema = Some(self.resolved.clone());
            }
        }
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
    }
}

#[tokio::test]
async fn validation_follows_the_registry_resolved_schema_not_the_authored_content() {
    let registry = registry();
    let stricter = json!({
        "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "metadata": { "type": "object", "required": ["only_in_resolved"] } }
    });
    let dept = catalog(Arc::new(ResolvedOverride {
        inner: registry,
        resolved: stricter,
    }))
    .get_type(&caller(1), DEPT, TypeUse::NewLink)
    .await
    .expect("active type");
    let err = dept
        .validate_metadata(Some(&json!({ "tier": "gold" })))
        .expect_err("the materialized resolved schema is the one enforced");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    dept.validate_metadata(Some(&json!({ "only_in_resolved": 1 })))
        .expect("the authored content's constraints are not re-derived");
}

// -------- membership rule --------

const EXTERNAL: &str = gts_id!("cf.test.lms.course.v1~");

#[tokio::test]
async fn a_membership_rule_reads_the_group_traits_and_checks_any_member_type() {
    let registry = registry();
    registry.seed(
        EXTERNAL,
        json!({
            "$id": gts_uri!("cf.test.lms.course.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object"
        }),
    );
    let rule = catalog(registry.clone())
        .get_membership_rule(&caller(1), TENANT, USER)
        .await
        .expect("both active");
    assert!(rule.allows_member(USER));
    assert!(!rule.allows_member(EXTERNAL));
    catalog(registry.clone())
        .get_membership_rule(&caller(1), TENANT, EXTERNAL)
        .await
        .expect("a non-RG member type needs no RG traits");
    assert_eq!(registry.calls(Call::BatchGet), 2, "one read per rule");
}

#[tokio::test]
async fn an_unknown_or_deleted_member_type_refuses_a_new_membership() {
    let unknown = catalog(registry())
        .get_membership_rule(&caller(1), TENANT, gts_id!("cf.test.rg.ghost.v1~"))
        .await
        .expect_err("unknown member type");
    assert!(
        matches!(unknown, DomainError::Validation { .. }),
        "{unknown:?}"
    );

    let registry = registry();
    delete(&registry, USER);
    let deleted = catalog(registry)
        .get_membership_rule(&caller(1), TENANT, USER)
        .await
        .expect_err("deleted member type");
    assert!(
        matches!(deleted, DomainError::Validation { .. }),
        "{deleted:?}"
    );
}

#[tokio::test]
async fn a_deleted_or_unknown_group_type_refuses_a_new_membership() {
    let deleted = registry();
    delete(&deleted, TENANT);
    let err = catalog(deleted)
        .get_membership_rule(&caller(1), TENANT, USER)
        .await
        .expect_err("deleted group type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
    let err = catalog(registry())
        .get_membership_rule(
            &caller(1),
            gts_id!("cf.core.rg.type.v1~cf.test.rg.nope.v1~"),
            USER,
        )
        .await
        .expect_err("unknown group type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
}
