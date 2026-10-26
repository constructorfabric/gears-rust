//! The profile read over stub policy services, stored facts a test gives, and
//! subject settings in an in-memory database.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use serde_json::json;
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::profile_read::{
    CATEGORY_PROPERTY, CategoryCheck, OriginKind, ProfileView, StoredFact,
};
use crate::domain::subject_settings::{SubjectKey, SubjectSettings};
use crate::test_support::{
    AllowResolver, DenyResolver, FixedFacts, OwnTenantOnlyResolver, RecordingProfileEvents,
    RecordingResolver, inmem_db, profile_service, profile_service_over, seed_subject_settings,
};

fn fact(category: Category, property: &str, value: serde_json::Value) -> StoredFact {
    StoredFact {
        node_key: NodeKey(format!("construct:{}", Uuid::new_v4())),
        category,
        property: property.to_owned(),
        value,
        origin: OriginKind::Connector,
        stored_at: OffsetDateTime::UNIX_EPOCH,
        produced_at: None,
    }
}

fn facts() -> Vec<StoredFact> {
    vec![
        fact(Category::Skill, "skills", json!({ "name": "Rust" })),
        fact(Category::Identity, "full_name", json!("Ada Lovelace")),
        fact(Category::Skill, "skills", json!({ "name": "Go" })),
    ]
}

fn caller_in(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

fn kinds(view: &ProfileView) -> Vec<(Category, usize)> {
    view.categories
        .iter()
        .map(|group| (group.category, group.facts.len()))
        .collect()
}

/// The tenant a request is about: the context tenant, else the caller's.
fn tenant_of(request: &EvaluationRequest) -> Uuid {
    request
        .context
        .tenant_context
        .as_ref()
        .and_then(|tc| tc.root_id)
        .or_else(|| {
            request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .unwrap_or_else(Uuid::nil)
}

fn allowed(predicates: Vec<Predicate>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint { predicates }],
            ..Default::default()
        },
    }
}

/// Allows the read, naming the readable categories in its scope.
struct CategoryResolver(&'static [&'static str]);

#[async_trait]
impl AuthZResolverApi for CategoryResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(allowed(vec![
            Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [tenant_of(&request)],
            )),
            Predicate::In(InPredicate::new(
                CATEGORY_PROPERTY,
                self.0.iter().map(|c| (*c).to_owned()),
            )),
        ]))
    }
}

/// Decides each request on its own: allows a request without a category, and
/// one for a category it lists; denies the rest.
struct PerCategoryResolver(&'static [&'static str]);

#[async_trait]
impl AuthZResolverApi for PerCategoryResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let category = request
            .resource
            .properties
            .get(CATEGORY_PROPERTY)
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        if let Some(category) = category
            && !self.0.contains(&category.as_str())
        {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        }
        Ok(allowed(vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            [tenant_of(&request)],
        ))]))
    }
}

#[tokio::test]
async fn a_caller_the_platform_lets_act_for_the_subject_reads_its_facts_by_category() {
    let db = inmem_db().await;
    let events = Arc::new(RecordingProfileEvents::default());
    let service = profile_service(
        &db,
        Arc::new(AllowResolver),
        facts(),
        Arc::clone(&events),
        CategoryCheck::OneCall,
    );
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let caller = caller_in(tenant);

    let view = service.read(&caller, None, subject).await.unwrap();

    assert_eq!(view.subject_id, subject);
    assert_eq!(
        kinds(&view),
        [(Category::Identity, 1), (Category::Skill, 2)],
        "in category order, only categories with facts"
    );
    let event = &events.taken()[0];
    assert_eq!(
        (event.tenant_id, event.subject_id, event.caller),
        (tenant, subject, caller.subject_id())
    );
    assert_eq!(event.categories, [Category::Identity, Category::Skill]);
}

#[tokio::test]
async fn a_caller_the_platform_refuses_does_not_see_the_subject_and_nothing_is_read() {
    let db = inmem_db().await;
    let events = Arc::new(RecordingProfileEvents::default());
    let service = profile_service(
        &db,
        Arc::new(DenyResolver),
        facts(),
        Arc::clone(&events),
        CategoryCheck::OneCall,
    );

    let result = service
        .read(&caller_in(Uuid::new_v4()), None, Uuid::new_v4())
        .await;

    assert!(
        matches!(result, Err(DomainError::NotVisible(_))),
        "{result:?}"
    );
    assert!(
        events.taken().is_empty(),
        "no read event for a refused read"
    );
}

#[tokio::test]
async fn a_named_tenant_the_scope_does_not_reach_is_not_visible() {
    let db = inmem_db().await;
    let service = profile_service(
        &db,
        Arc::new(OwnTenantOnlyResolver),
        facts(),
        Arc::default(),
        CategoryCheck::OneCall,
    );
    let caller = caller_in(Uuid::new_v4());

    let result = service
        .read(&caller, Some(Uuid::new_v4()), Uuid::new_v4())
        .await;

    assert!(
        matches!(result, Err(DomainError::NotVisible(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn the_policy_service_is_asked_about_the_named_tenant_or_the_callers_own() {
    let db = inmem_db().await;
    let resolver = Arc::new(RecordingResolver::default());
    let service = profile_service(
        &db,
        Arc::clone(&resolver) as Arc<dyn AuthZResolverApi>,
        Vec::new(),
        Arc::default(),
        CategoryCheck::OneCall,
    );
    let (own, named) = (Uuid::new_v4(), Uuid::new_v4());
    let caller = caller_in(own);

    service.read(&caller, None, Uuid::new_v4()).await.unwrap();
    service
        .read(&caller, Some(named), Uuid::new_v4())
        .await
        .unwrap();

    let asked: Vec<_> = resolver
        .asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(resource, ..)| resource == "construct.profile")
        .map(|(_, action, context, owner)| (action.clone(), *context, owner.clone()))
        .collect();
    assert_eq!(
        asked,
        [
            ("read".to_owned(), Some(own), Some(own.to_string())),
            ("read".to_owned(), Some(named), Some(named.to_string())),
        ]
    );
}

#[tokio::test]
async fn a_category_the_scope_leaves_out_is_not_listed() {
    let db = inmem_db().await;
    let events = Arc::new(RecordingProfileEvents::default());
    let service = profile_service(
        &db,
        Arc::new(CategoryResolver(&["skill"])),
        facts(),
        Arc::clone(&events),
        CategoryCheck::OneCall,
    );

    let view = service
        .read(&caller_in(Uuid::new_v4()), None, Uuid::new_v4())
        .await
        .unwrap();

    assert_eq!(kinds(&view), [(Category::Skill, 2)]);
    assert_eq!(events.taken()[0].categories, [Category::Skill]);
}

#[tokio::test]
async fn per_category_the_policy_service_is_asked_for_each_category() {
    let db = inmem_db().await;
    let service = profile_service(
        &db,
        Arc::new(PerCategoryResolver(&["identity"])),
        facts(),
        Arc::default(),
        CategoryCheck::PerCategory,
    );

    let view = service
        .read(&caller_in(Uuid::new_v4()), None, Uuid::new_v4())
        .await
        .unwrap();

    assert_eq!(kinds(&view), [(Category::Identity, 1)]);
}

#[tokio::test]
async fn nothing_is_served_while_personalization_is_off_or_an_erasure_is_under_way() {
    for settings in [
        SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: false,
        },
        SubjectSettings {
            personalization_enabled: true,
            erasure_in_progress: true,
        },
    ] {
        let db = inmem_db().await;
        let events = Arc::new(RecordingProfileEvents::default());
        let service = profile_service(
            &db,
            Arc::new(AllowResolver),
            facts(),
            Arc::clone(&events),
            CategoryCheck::OneCall,
        );
        let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
        seed_subject_settings(
            &db,
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
            settings,
        )
        .await;

        let view = service
            .read(&caller_in(tenant), None, subject)
            .await
            .unwrap();

        assert!(view.categories.is_empty(), "{settings:?}");
        assert_eq!(events.taken().len(), 1, "the read is still an event");
        assert!(events.taken()[0].categories.is_empty());
    }
}

/// Allows the read with exactly the constraints a test gives.
struct FixedScope(Vec<Vec<Predicate>>);

#[async_trait]
impl AuthZResolverApi for FixedScope {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: self
                    .0
                    .iter()
                    .map(|predicates| Constraint {
                        predicates: predicates.clone(),
                    })
                    .collect(),
                ..Default::default()
            },
        })
    }
}

fn tenant_is(tenant: Uuid) -> Predicate {
    Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [tenant]))
}

fn subject_is(subject: Uuid) -> Predicate {
    Predicate::In(InPredicate::new(pep_properties::RESOURCE_ID, [subject]))
}

fn category_is(category: &str) -> Predicate {
    Predicate::In(InPredicate::new(CATEGORY_PROPERTY, [category.to_owned()]))
}

#[tokio::test]
async fn a_tenant_and_a_subject_from_two_different_constraints_do_not_make_a_grant() {
    let db = inmem_db().await;
    let (tenant, other_tenant) = (Uuid::new_v4(), Uuid::new_v4());
    let (subject_in_tenant, subject_elsewhere) = (Uuid::new_v4(), Uuid::new_v4());
    // In `tenant` only `subject_in_tenant`; `subject_elsewhere` only in `other_tenant`.
    let resolver = FixedScope(vec![
        vec![tenant_is(tenant), subject_is(subject_in_tenant)],
        vec![tenant_is(other_tenant), subject_is(subject_elsewhere)],
    ]);
    let service = profile_service(
        &db,
        Arc::new(resolver),
        facts(),
        Arc::default(),
        CategoryCheck::OneCall,
    );
    let caller = caller_in(tenant);

    let mixed = service.read(&caller, Some(tenant), subject_elsewhere).await;
    let granted = service.read(&caller, Some(tenant), subject_in_tenant).await;

    assert!(
        matches!(mixed, Err(DomainError::NotVisible(_))),
        "{mixed:?}"
    );
    assert!(granted.is_ok(), "{granted:?}");
}

#[tokio::test]
async fn categories_come_only_from_the_constraints_that_reach_the_subject() {
    let db = inmem_db().await;
    let (tenant, other_tenant, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = FixedScope(vec![
        vec![tenant_is(tenant), category_is("skill")],
        vec![tenant_is(other_tenant), category_is("identity")],
    ]);
    let service = profile_service(
        &db,
        Arc::new(resolver),
        facts(),
        Arc::default(),
        CategoryCheck::OneCall,
    );

    let view = service
        .read(&caller_in(tenant), Some(tenant), subject)
        .await
        .unwrap();

    assert_eq!(
        kinds(&view),
        [(Category::Skill, 2)],
        "identity is granted only elsewhere"
    );
}

#[tokio::test]
async fn a_scope_that_names_another_subject_does_not_reach_this_one() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let resolver = FixedScope(vec![vec![tenant_is(tenant), subject_is(Uuid::new_v4())]]);
    let events = Arc::new(RecordingProfileEvents::default());
    let service = profile_service(
        &db,
        Arc::new(resolver),
        facts(),
        Arc::clone(&events),
        CategoryCheck::OneCall,
    );

    let result = service.read(&caller_in(tenant), None, Uuid::new_v4()).await;

    assert!(
        matches!(result, Err(DomainError::NotVisible(_))),
        "{result:?}"
    );
    assert!(events.taken().is_empty());
}

/// Allows the read, but fails the policy call for one category.
struct FailsForCategory(&'static str);

#[async_trait]
impl AuthZResolverApi for FailsForCategory {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let category = request
            .resource
            .properties
            .get(CATEGORY_PROPERTY)
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        if category.as_deref() == Some(self.0) {
            return Err(CanonicalError::service_unavailable().create());
        }
        PerCategoryResolver(&["identity", "role", "skill", "preference"])
            .evaluate(ctx, request)
            .await
    }
}

#[tokio::test]
async fn per_category_a_failed_policy_call_fails_the_read_instead_of_hiding_the_category() {
    let db = inmem_db().await;
    let service = profile_service(
        &db,
        Arc::new(FailsForCategory("role")),
        facts(),
        Arc::default(),
        CategoryCheck::PerCategory,
    );

    let result = service
        .read(&caller_in(Uuid::new_v4()), None, Uuid::new_v4())
        .await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "a partial profile must not be served: {result:?}"
    );
}

#[tokio::test]
async fn the_facts_are_read_as_the_processing_identity_for_the_subject_and_tenant_asked() {
    let db = inmem_db().await;
    let profiles = Arc::new(FixedFacts::new(facts()));
    let identity = Uuid::new_v4();
    let service = profile_service_over(
        &db,
        Arc::new(AllowResolver),
        Arc::clone(&profiles) as _,
        identity,
        Arc::default(),
        CategoryCheck::OneCall,
    );
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    service
        .read(&caller_in(Uuid::new_v4()), Some(tenant), subject)
        .await
        .unwrap();

    assert_eq!(
        *profiles.calls.lock().unwrap(),
        [(
            identity,
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject
            }
        )]
    );
}
