//! [`RegistryTypeCatalog`] over `MockTypesRegistry`, which materializes
//! the resolved schema with `gts-rust` as admission does.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use std::sync::Arc;

use async_trait::async_trait;
use resource_group_sdk::gts::GROUP_MEMBERSHIP_RESOURCE_TYPE;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{gts_id, gts_uri};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, EntityLookup, GtsId, ListEntitiesRequest,
    ListEntitiesResponse, TypesRegistryApi,
};
use uuid::Uuid;

use super::RegistryTypeCatalog;
use crate::domain::error::DomainError;
use crate::domain::type_catalog::{RgTypeCatalog, TypeUse};

#[toolkit_canonical_errors::resource_error(gts_id!("cf.core.types_registry.entity.v1~"))]
struct Catalogue;

const USER: &str = gts_id!("cf.test.rg.user.v1~");
const TENANT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~");
const DEPT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~");
const MEMBERSHIP: &str = gts_id!("cf.core.rg.group_membership.v1~cf.test.rg.member.v1~");

fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    for (id, schema) in resource_group_sdk::gts_declarations() {
        registry.seed(
            &id,
            serde_json::from_str(&schema).expect("RG base schema JSON"),
        );
    }
    registry.seed(
        MEMBERSHIP,
        json!({
            "$id": format!("gts://{MEMBERSHIP}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                { "$ref": format!("gts://{GROUP_MEMBERSHIP_RESOURCE_TYPE}") },
                { "properties": { "resource_type": { "const": USER } } }
            ]
        }),
    );
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
            "x-gts-traits": {"membership_type": MEMBERSHIP},
            "x-gts-traits-schema": {"properties": {"membership_type": {"const": MEMBERSHIP}}},
            "$defs": { "tenant_meta": {
                "type": "object",
                "required": ["tier"],
                "properties": { "tier": { "type": "string" } }
            } },
            "allOf": [
                { "$ref": gts_uri!("cf.core.rg.type.v1~") },
                { "properties": {
                    "metadata": { "$ref": "#/$defs/tenant_meta" },
                    "parent_type": { "enum": [null, TENANT] }
                } }
            ]
        }),
    );
    registry.seed(
        DEPT,
        json!({
            "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                { "$ref": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~") },
                { "properties": {
                    "parent_type": { "const": TENANT },
                    "metadata": {
                        "type": "object",
                        "properties": { "code": { "type": "string", "maxLength": 3 } }
                    }
                } }
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

#[tokio::test]
async fn reads_the_inherited_membership_binding_and_validates_placement() {
    let dept = catalog(registry())
        .get_type(&caller(1), DEPT, TypeUse::NewLink)
        .await
        .expect("active group type");
    assert_eq!(dept.code, DEPT);
    assert_eq!(
        dept.membership_type().expect("membership binding"),
        MEMBERSHIP
    );
    dept.validate_group(Some(TENANT), None)
        .expect("tenant parent");
    dept.validate_group(Some(DEPT), None)
        .expect_err("department parent");
    dept.validate_group(None, None)
        .expect_err("department as root");

    let tenant = catalog(registry())
        .get_type(&caller(1), TENANT, TypeUse::NewLink)
        .await
        .expect("active group type");
    tenant.validate_group(None, None).expect("tenant as root");
    tenant
        .validate_group(Some(TENANT), None)
        .expect("self nesting");
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

#[tokio::test]
async fn membership_rule_rejects_an_active_abstract_member() {
    let registry = registry();
    let mut content = registry.content(USER).expect("member schema");
    content["x-gts-abstract"] = json!(true);
    registry.seed(USER, content);
    let error = catalog(registry)
        .get_membership_rule(&caller(1), TENANT, USER)
        .await
        .expect_err("abstract member under an otherwise admitting rule");
    assert!(matches!(error, DomainError::Validation { message } if message.contains("abstract")));
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
    retained
        .validate_group(None, Some(&json!({ "tier": "gold" })))
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
        let problem = |error: CanonicalError| {
            serde_json::to_value(toolkit_canonical_errors::Problem::from(error))
                .expect("problem JSON")
        };
        assert_eq!(
            problem(CanonicalError::from(err)),
            problem(refusal.clone()),
            "401/403 reach the caller unmasked, reason and resource included"
        );
    }
}

#[tokio::test]
async fn a_transient_registry_failure_is_service_unavailable_with_its_retry_hint() {
    for (failure, retry_after) in [
        (
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(7)
                .create(),
            Some(7),
        ),
        (Catalogue::deadline_exceeded("slow read").create(), None),
        (
            Catalogue::resource_exhausted("read quota")
                .with_quota_violation("READS", "too many reads")
                .with_quota_violation_retry_after_seconds(11)
                .create(),
            Some(11),
        ),
        (
            Catalogue::aborted("read conflict")
                .with_reason("CONFLICT")
                .create(),
            None,
        ),
    ] {
        let registry = registry();
        registry.inject(Fault::on(Call::BatchGet).fail(failure.clone()));
        let err = catalog(registry)
            .get_type(&caller(1), TENANT, TypeUse::NewLink)
            .await
            .expect_err("transient failure");
        assert!(
            matches!(
                &err,
                DomainError::ServiceUnavailable { retry_after_seconds, .. }
                    if *retry_after_seconds == retry_after
            ),
            "{failure:?} -> {err:?}"
        );
        let canonical = CanonicalError::from(err);
        assert_eq!(canonical.status_code(), 503, "{failure:?}");
        assert_eq!(canonical.retry_after_seconds(), retry_after, "{failure:?}");
    }
}

#[tokio::test]
async fn any_other_registry_failure_is_internal() {
    let registry = registry();
    registry
        .inject(Fault::on(Call::BatchGet).fail(CanonicalError::internal("registry bug").create()));
    let err = catalog(registry)
        .get_type(&caller(1), TENANT, TypeUse::NewLink)
        .await
        .expect_err("registry failure");
    assert!(matches!(err, DomainError::InternalError), "{err:?}");
}

// -------- payload validation --------

#[tokio::test]
async fn metadata_is_validated_by_the_sdk_against_the_inherited_resolved_schema() {
    let dept = catalog(registry())
        .get_type(&caller(1), DEPT, TypeUse::NewLink)
        .await
        .expect("active type");
    dept.validate_group(Some(TENANT), Some(&json!({ "tier": "gold", "code": "ab" })))
        .expect("conforms");
    dept.validate_group(Some(TENANT), None)
        .expect("metadata is optional");
    for invalid in [
        json!({ "code": "ab" }),
        json!({ "tier": 7 }),
        json!({ "tier": "gold", "code": "abcd" }),
        json!("not an object"),
    ] {
        let err = dept
            .validate_group(Some(TENANT), Some(&invalid))
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
        "properties": {
            "metadata": { "type": "object", "required": ["only_in_resolved"] }
        }
    });
    let dept = catalog(Arc::new(ResolvedOverride {
        inner: registry,
        resolved: stricter,
    }))
    .get_type(&caller(1), DEPT, TypeUse::NewLink)
    .await
    .expect("active type");
    let err = dept
        .validate_group(Some(TENANT), Some(&json!({ "tier": "gold" })))
        .expect_err("the materialized resolved schema is the one enforced");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    dept.validate_group(Some(TENANT), Some(&json!({ "only_in_resolved": 1 })))
        .expect("the authored content's constraints are not re-derived");
}

// -------- membership rule --------

const EXTERNAL: &str = gts_id!("cf.test.lms.course.v1~");

#[tokio::test]
async fn a_membership_rule_reads_the_group_then_batches_the_schema_and_member_for_the_caller() {
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
        .get_membership_rule(&caller(4), TENANT, EXTERNAL)
        .await
        .expect("a non-RG member type can be looked up");
    assert_eq!(rule.code, MEMBERSHIP);
    rule.validate_member(USER).expect("user allowed");
    rule.validate_member(EXTERNAL)
        .expect_err("external type excluded");
    let contexts = registry.caller_contexts(Call::BatchGet);
    assert_eq!(
        contexts.len(),
        2,
        "group type, then membership schema and member"
    );
    for ctx in contexts {
        assert_eq!(ctx.subject_id(), caller(4).subject_id());
        assert_eq!(ctx.subject_tenant_id(), caller(4).subject_tenant_id());
    }
    assert_eq!(
        registry.platform_calls(Call::BatchGet),
        0,
        "never the platform plane"
    );
}

#[tokio::test]
async fn a_membership_rule_uses_the_inherited_group_type_trait() {
    let registry = registry();
    let rule = catalog(registry.clone())
        .get_membership_rule(&caller(1), DEPT, USER)
        .await
        .expect("the binding the department type inherits");
    assert_eq!(rule.code, MEMBERSHIP);
    assert_eq!(
        registry.calls(Call::BatchGet),
        2,
        "group type, then the rule"
    );
}

#[tokio::test]
async fn a_membership_rule_checks_the_group_type_first() {
    for resource_type in [USER, "not an id"] {
        let registry = registry();
        delete(&registry, TENANT);
        delete(&registry, USER);
        let err = catalog(registry)
            .get_membership_rule(&caller(1), TENANT, resource_type)
            .await
            .expect_err("deleted group type");
        assert!(
            matches!(&err, DomainError::TypeNotFound { code } if code == TENANT),
            "{resource_type}: {err:?}"
        );
    }
}

#[tokio::test]
async fn only_a_concrete_membership_schema_is_read() {
    for membership_type in [
        GROUP_MEMBERSHIP_RESOURCE_TYPE,
        gts_id!("cf.test.rg.member.v1~"),
        "gts.cf.core.rg.group_membership.v1~not an id",
    ] {
        let registry = registry();
        let err = catalog(registry.clone())
            .get_membership_schema(&caller(1), membership_type)
            .await
            .expect_err("not a concrete membership schema");
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{membership_type}: {err:?}"
        );
        assert_eq!(registry.calls(Call::BatchGet), 0, "refused before the read");
    }
}

/// A malformed membership schema or member type is refused after the group
/// type is read, and nothing else is read for it.
#[tokio::test]
async fn a_malformed_identifier_reads_only_the_group_type() {
    let registry = registry();
    let err = catalog(registry.clone())
        .get_membership_rule(&caller(1), TENANT, "not an id")
        .await
        .expect_err("malformed identifier");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    let read: Vec<String> = registry
        .reads()
        .iter()
        .map(|key| match key {
            types_registry_sdk::EntityKey::GtsId(id) => id.as_ref().to_owned(),
            other @ types_registry_sdk::EntityKey::GtsUuid(_) => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(read, [TENANT]);
}

#[tokio::test]
async fn a_membership_schema_is_read_for_the_caller() {
    let registry = registry();
    let schema = catalog(registry.clone())
        .get_membership_schema(&caller(5), MEMBERSHIP)
        .await
        .expect("active membership schema");
    assert_eq!(schema.code, MEMBERSHIP);
    let contexts = registry.caller_contexts(Call::BatchGet);
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].subject_id(), caller(5).subject_id());
    assert_eq!(
        contexts[0].subject_tenant_id(),
        caller(5).subject_tenant_id()
    );
    assert_eq!(registry.platform_calls(Call::BatchGet), 0);
}

/// Answers a lookup of `from` with the entity stored under `to`, renamed.
struct Misaddressed {
    inner: Arc<MockTypesRegistry>,
    from: &'static str,
    to: &'static str,
}

#[async_trait]
impl TypesRegistryApi for Misaddressed {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let mut response = self.inner.batch_get_entities(ctx, request).await?;
        for lookup in response.0.values_mut() {
            if let EntityLookup::Found { entity, .. } = lookup
                && entity.gts_id.as_ref() == self.from
            {
                entity.gts_id = GtsId::try_new(self.to).expect("identifier");
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
async fn an_answer_for_another_type_is_internal() {
    let misaddressed = |from, to| -> RegistryTypeCatalog {
        catalog(Arc::new(Misaddressed {
            inner: registry(),
            from,
            to,
        }))
    };
    let err = misaddressed(TENANT, DEPT)
        .get_type(&caller(1), TENANT, TypeUse::NewLink)
        .await
        .expect_err("group type answered as another");
    assert!(matches!(err, DomainError::InternalError), "{err:?}");
    for (from, to) in [(TENANT, DEPT), (MEMBERSHIP, USER), (USER, TENANT)] {
        let err = misaddressed(from, to)
            .get_membership_rule(&caller(1), TENANT, USER)
            .await
            .expect_err("a lookup answered as another type");
        assert!(matches!(err, DomainError::InternalError), "{from}: {err:?}");
    }
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
async fn a_deleted_or_unknown_membership_schema_refuses_a_new_link() {
    let deleted = registry();
    delete(&deleted, MEMBERSHIP);
    let err = catalog(deleted.clone())
        .get_membership_rule(&caller(1), TENANT, USER)
        .await
        .expect_err("deleted membership schema");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
    let err = catalog(deleted)
        .get_membership_schema(&caller(1), MEMBERSHIP)
        .await
        .expect_err("deleted membership schema");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
    let err = catalog(registry())
        .get_membership_schema(
            &caller(1),
            gts_id!("cf.core.rg.group_membership.v1~cf.test.rg.nope.v1~"),
        )
        .await
        .expect_err("unknown membership schema");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
}
