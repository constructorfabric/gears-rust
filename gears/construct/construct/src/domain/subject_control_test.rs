//! Subject Control over an in-memory profile and writer, with stub policy
//! services.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use serde_json::json;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Origin};
use crate::domain::profile::Category;
use crate::domain::subject_control::{SubjectControlEvent, SubjectControlService};
use crate::domain::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsReader};
use crate::test_support::{
    AllowResolver, DenyResolver, FixedSubjectSettings, MemoryProfile, MemoryWriter,
    RecordingResolver, RecordingSubjectControlEvents, context_of, stored_fact, subject_control,
};

const OFF_AND_ERASING: SubjectSettings = SubjectSettings {
    personalization_enabled: false,
    erasure_in_progress: true,
};

struct Fixture {
    tenant: Uuid,
    subject: Uuid,
    identity: Uuid,
    profile: Arc<MemoryProfile>,
    writer: Arc<MemoryWriter>,
    events: Arc<RecordingSubjectControlEvents>,
}

impl Fixture {
    /// A subject with a skill stored at second 10 and a role at second 20,
    /// and a writer that conflicts `conflicts` times.
    fn new(conflicts: u32) -> Self {
        let profile = MemoryProfile::with(vec![
            stored_fact(Category::Skill, "skills", json!({ "name": "Rust" }), 10),
            stored_fact(Category::Role, "roles", json!({ "title": "Engineer" }), 20),
        ]);
        let writer = MemoryWriter::new(Arc::clone(&profile), conflicts);
        Self {
            tenant: Uuid::new_v4(),
            subject: Uuid::new_v4(),
            identity: Uuid::new_v4(),
            profile,
            writer,
            events: Arc::default(),
        }
    }

    /// The service over the fixture, with the settings `settings`.
    fn service_with(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        settings: Arc<dyn SubjectSettingsReader>,
    ) -> SubjectControlService {
        subject_control(
            resolver,
            Arc::clone(&self.profile),
            Arc::clone(&self.writer),
            settings,
            Arc::clone(&self.events),
            Some(self.identity),
        )
    }

    /// The service over the fixture, whose settings are off and under
    /// erasure.
    fn service(&self, resolver: Arc<dyn AuthZResolverApi>) -> SubjectControlService {
        self.service_with(
            resolver,
            Arc::new(FixedSubjectSettings(Ok(OFF_AND_ERASING))),
        )
    }

    fn owner(&self) -> SecurityContext {
        context_of(self.subject, self.tenant)
    }

    fn key(&self) -> SubjectKey {
        SubjectKey {
            tenant_id: self.tenant,
            subject_id: self.subject,
        }
    }

    fn fact_id(&self, category: Category) -> NodeKey {
        self.profile
            .live()
            .into_iter()
            .find(|fact| fact.category == category)
            .unwrap()
            .node_key
    }

    fn reads(&self) -> Vec<(Uuid, SubjectKey)> {
        self.profile.reads.lock().unwrap().clone()
    }
}

/// Allows every request, but its scope names another subject: a platform
/// that lets the caller act, only not for this subject.
struct ElsewhereResolver;

#[async_trait]
impl AuthZResolverApi for ElsewhereResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        mut request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        request.resource.id = Some(Uuid::new_v4());
        AllowResolver.evaluate(ctx, request).await
    }
}

/// Settings that keep the key and the scope each read asked with.
#[derive(Default)]
struct RecordingSettings(Mutex<Vec<(SubjectKey, AccessScope)>>);

#[async_trait]
impl SubjectSettingsReader for RecordingSettings {
    async fn read(
        &self,
        _ctx: &SecurityContext,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SubjectSettings, DomainError> {
        self.0.lock().unwrap().push((key, scope.clone()));
        Ok(OFF_AND_ERASING)
    }
}

fn filter(raw: &str) -> ODataQuery {
    ODataQuery::new().with_filter(toolkit_odata::parse_filter_string(raw).unwrap().into_expr())
}

#[tokio::test]
async fn the_owner_sees_every_fact_newest_first_with_the_version_also_while_off_and_erasing() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(AllowResolver));

    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await
        .unwrap();

    let categories: Vec<Category> = view.page.items.iter().map(|fact| fact.category).collect();
    assert_eq!(categories, [Category::Role, Category::Skill]);
    assert_eq!(view.version, Some(1));
    assert_eq!(
        fixture.reads(),
        [(fixture.identity, fixture.key())],
        "the facts are read as Construct, for the subject in the caller's tenant"
    );
    assert_eq!(
        fixture.events.taken(),
        [SubjectControlEvent::Viewed {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            categories: BTreeSet::from([Category::Skill, Category::Role]),
        }]
    );
}

#[tokio::test]
async fn a_view_is_recorded_with_the_categories_served_and_not_at_all_for_a_bad_query() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(AllowResolver));

    let skills = service
        .view(
            &fixture.owner(),
            fixture.subject,
            &filter("entity_kind eq 'skill'"),
        )
        .await
        .unwrap();
    let refused = service
        .view(&fixture.owner(), fixture.subject, &filter("value eq 'x'"))
        .await;

    assert_eq!(skills.page.items.len(), 1);
    assert!(
        matches!(refused, Err(DomainError::InvalidQuery(_))),
        "{refused:?}"
    );
    assert_eq!(
        fixture.events.taken(),
        [SubjectControlEvent::Viewed {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            categories: BTreeSet::from([Category::Skill]),
        }],
        "only the served page is recorded, with only what it served"
    );
}

#[tokio::test]
async fn another_subject_is_refused_before_the_policy_service_is_asked_or_anything_is_read() {
    let fixture = Fixture::new(0);
    let resolver = Arc::new(RecordingResolver::default());
    let service = fixture.service(Arc::clone(&resolver) as _);
    let fact_id = fixture.fact_id(Category::Skill);
    let colleague = context_of(Uuid::new_v4(), fixture.tenant);

    let view = service
        .view(&colleague, fixture.subject, &ODataQuery::new())
        .await;
    let fact = service.fact(&colleague, fixture.subject, &fact_id).await;
    let deleted = service.delete(&colleague, fixture.subject, &fact_id).await;

    assert!(matches!(view, Err(DomainError::Forbidden(_))), "{view:?}");
    assert!(matches!(fact, Err(DomainError::Forbidden(_))), "{fact:?}");
    assert!(
        matches!(deleted, Err(DomainError::Forbidden(_))),
        "{deleted:?}"
    );
    assert!(resolver.asked.lock().unwrap().is_empty());
    assert!(fixture.reads().is_empty());
    assert!(fixture.writer.plans().is_empty());
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn the_owner_the_platform_refuses_gets_nothing() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(DenyResolver));

    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await;

    assert!(matches!(view, Err(DomainError::Forbidden(_))), "{view:?}");
    assert!(fixture.reads().is_empty());
}

#[tokio::test]
async fn a_scope_that_does_not_reach_the_subject_is_a_refusal_and_nothing_is_read() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(ElsewhereResolver));
    let officer = context_of(Uuid::new_v4(), fixture.tenant);

    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await;
    let export = service.export(&officer, fixture.subject).await;

    assert!(matches!(view, Err(DomainError::Forbidden(_))), "{view:?}");
    assert!(
        matches!(export, Err(DomainError::Forbidden(_))),
        "{export:?}"
    );
    assert!(fixture.reads().is_empty());
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn each_operation_asks_for_its_own_action_on_the_subject_facts() {
    let fixture = Fixture::new(0);
    let resolver = Arc::new(RecordingResolver::default());
    let service = fixture.service(Arc::clone(&resolver) as _);
    let fact_id = fixture.fact_id(Category::Skill);

    service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await
        .unwrap();
    service
        .delete(&fixture.owner(), fixture.subject, &fact_id)
        .await
        .unwrap();
    service
        .export(&context_of(Uuid::new_v4(), fixture.tenant), fixture.subject)
        .await
        .unwrap();

    let asked: Vec<(String, String)> = resolver
        .asked
        .lock()
        .unwrap()
        .iter()
        .map(|(resource, action, ..)| (resource.clone(), action.clone()))
        .collect();
    let facts = toolkit_gts::gts_id!("cf.construct.subject.facts.v1~").to_owned();
    assert_eq!(
        asked,
        [
            (facts.clone(), "read".to_owned()),
            (facts.clone(), "delete".to_owned()),
            (facts, "export".to_owned()),
        ]
    );
}

#[tokio::test]
async fn one_fact_is_served_and_a_fact_the_subject_does_not_have_is_no_such_fact() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(AllowResolver));
    let skill = fixture.profile.live()[0].clone();

    let found = service
        .fact(&fixture.owner(), fixture.subject, &skill.node_key)
        .await
        .unwrap();
    let missing = service
        .fact(
            &fixture.owner(),
            fixture.subject,
            &NodeKey("construct:t:nothing".to_owned()),
        )
        .await;

    assert_eq!(found, skill);
    assert!(
        matches!(missing, Err(DomainError::NoSuchFact(_))),
        "{missing:?}"
    );
    assert_eq!(
        fixture.events.taken(),
        [SubjectControlEvent::Viewed {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            categories: BTreeSet::from([Category::Skill]),
        }],
        "only the read that served a fact is an event"
    );
}

#[tokio::test]
async fn a_delete_writes_one_removal_as_the_subject_against_the_version_and_the_fact_is_gone() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(AllowResolver));
    let skill = fixture.fact_id(Category::Skill);

    service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await
        .unwrap();

    let written = fixture.writer.plans.lock().unwrap().clone();
    let [(writer_ctx, plan)] = written.as_slice() else {
        panic!("one plan: {written:?}");
    };
    assert_eq!(*writer_ctx, fixture.identity, "written as Construct");
    assert_eq!(
        plan.origin,
        Origin::Subject {
            subject: fixture.subject
        }
    );
    assert!(
        !plan.origin.written_by_a_person(),
        "it never reaches Admission"
    );
    assert_eq!(plan.profile_version, Some(1));
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].entity_kind, "skill");
    assert_eq!(
        plan.steps[0].change,
        Change::RemoveEntity {
            entity: skill.clone()
        }
    );
    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await
        .unwrap();
    assert!(
        view.page.items.iter().all(|fact| fact.node_key != skill),
        "not served from the next read"
    );
    assert_eq!(
        fixture.events.taken()[0],
        SubjectControlEvent::Deleted {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            fact_id: skill,
            category: Category::Skill,
        }
    );
}

#[tokio::test]
async fn a_delete_reads_and_writes_again_on_a_conflict() {
    let fixture = Fixture::new(2);
    let service = fixture.service(Arc::new(AllowResolver));

    service
        .delete(
            &fixture.owner(),
            fixture.subject,
            &fixture.fact_id(Category::Skill),
        )
        .await
        .unwrap();

    let versions: Vec<Option<u64>> = fixture
        .writer
        .plans()
        .iter()
        .map(|plan| plan.profile_version)
        .collect();
    assert_eq!(
        versions,
        [Some(1), Some(2), Some(3)],
        "each try on the version just read"
    );
    assert_eq!(fixture.profile.live().len(), 1);
}

#[tokio::test]
async fn a_delete_gives_up_as_unavailable_once_the_reruns_are_spent() {
    let fixture = Fixture::new(10);
    let service = fixture.service(Arc::new(AllowResolver));

    let deleted = service
        .delete(
            &fixture.owner(),
            fixture.subject,
            &fixture.fact_id(Category::Skill),
        )
        .await;

    assert!(
        matches!(deleted, Err(DomainError::Unavailable(_))),
        "{deleted:?}"
    );
    assert_eq!(
        fixture.writer.plans().len(),
        3,
        "the first try and two reruns"
    );
    assert_eq!(fixture.profile.live().len(), 2, "nothing was deleted");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn a_delete_gives_up_as_unavailable_once_its_budget_is_spent() {
    let fixture = Fixture::new(10);
    let service = fixture
        .service(Arc::new(AllowResolver))
        .with_budget(Duration::ZERO);

    let deleted = service
        .delete(
            &fixture.owner(),
            fixture.subject,
            &fixture.fact_id(Category::Skill),
        )
        .await;

    assert!(
        matches!(deleted, Err(DomainError::Unavailable(_))),
        "{deleted:?}"
    );
    assert_eq!(fixture.writer.plans().len(), 1, "no rerun past the budget");
}

#[tokio::test]
async fn a_fact_another_write_removed_before_the_rerun_is_no_such_fact() {
    let mut fixture = Fixture::new(0);
    fixture.writer = MemoryWriter::racing(Arc::clone(&fixture.profile));
    let service = fixture.service(Arc::new(AllowResolver));
    let skill = fixture.fact_id(Category::Skill);

    let deleted = service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await;

    assert!(
        matches!(deleted, Err(DomainError::NoSuchFact(_))),
        "{deleted:?}"
    );
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|fact| fact.node_key != skill)
    );
    assert!(
        fixture.events.taken().is_empty(),
        "the subject's own delete did not land"
    );
}

#[tokio::test]
async fn deleting_a_fact_the_subject_does_not_have_writes_nothing() {
    let fixture = Fixture::new(0);
    let service = fixture.service(Arc::new(AllowResolver));

    let deleted = service
        .delete(
            &fixture.owner(),
            fixture.subject,
            &NodeKey("construct:t:nothing".to_owned()),
        )
        .await;

    assert!(
        matches!(deleted, Err(DomainError::NoSuchFact(_))),
        "{deleted:?}"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn the_export_needs_its_permission_not_the_owner_and_holds_the_settings_and_every_fact() {
    let fixture = Fixture::new(0);
    let officer = context_of(Uuid::new_v4(), fixture.tenant);
    let settings = Arc::new(RecordingSettings::default());

    let export = fixture
        .service_with(Arc::new(AllowResolver), Arc::clone(&settings) as _)
        .export(&officer, fixture.subject)
        .await
        .unwrap();
    let refused = fixture
        .service(Arc::new(DenyResolver))
        .export(&officer, fixture.subject)
        .await;

    assert_eq!(export.tenant_id, fixture.tenant);
    assert_eq!(export.subject_id, fixture.subject);
    assert_eq!(export.settings, OFF_AND_ERASING);
    assert_eq!(export.facts, fixture.profile.live());
    assert_eq!(
        fixture.reads(),
        [(fixture.identity, fixture.key())],
        "the subject's facts, read as Construct, not the officer's"
    );
    let asked = settings.0.lock().unwrap().clone();
    let [(key, scope)] = asked.as_slice() else {
        panic!("one settings read: {asked:?}");
    };
    assert_eq!(
        *key,
        fixture.key(),
        "the subject's settings, not the officer's"
    );
    assert!(
        scope.constraints().iter().all(|constraint| constraint
            .filters()
            .iter()
            .all(|filter| filter.property() == pep_properties::OWNER_TENANT_ID)),
        "the settings are read under the tenant filters only: {scope:?}"
    );
    assert_eq!(
        fixture.events.taken(),
        [SubjectControlEvent::Exported {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            caller: officer.subject_id(),
            categories: BTreeSet::from([Category::Skill, Category::Role]),
        }]
    );
    assert!(
        matches!(refused, Err(DomainError::Forbidden(_))),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_refused_or_failed_settings_read_fails_the_export_as_it_failed() {
    let fixture = Fixture::new(0);
    let officer = context_of(Uuid::new_v4(), fixture.tenant);

    let refused = fixture
        .service_with(
            Arc::new(AllowResolver),
            Arc::new(FixedSubjectSettings(Err(|| {
                DomainError::forbidden("the settings scope refuses")
            }))),
        )
        .export(&officer, fixture.subject)
        .await;
    let down = fixture
        .service_with(
            Arc::new(AllowResolver),
            Arc::new(FixedSubjectSettings(Err(|| {
                DomainError::Unavailable("settings are down".to_owned())
            }))),
        )
        .export(&officer, fixture.subject)
        .await;

    assert!(
        matches!(refused, Err(DomainError::Forbidden(_))),
        "a refusal stays a refusal, not a missing subject: {refused:?}"
    );
    assert!(matches!(down, Err(DomainError::Unavailable(_))), "{down:?}");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn without_a_processing_identity_every_operation_is_not_configured() {
    let fixture = Fixture::new(0);
    let service = subject_control(
        Arc::new(AllowResolver),
        Arc::clone(&fixture.profile),
        Arc::clone(&fixture.writer),
        Arc::new(FixedSubjectSettings(Ok(OFF_AND_ERASING))),
        Arc::clone(&fixture.events),
        None,
    );
    let fact_id = fixture.fact_id(Category::Skill);

    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await;
    let deleted = service
        .delete(&fixture.owner(), fixture.subject, &fact_id)
        .await;
    let export = service.export(&fixture.owner(), fixture.subject).await;

    for result in [view.map(|_| ()), deleted, export.map(|_| ())] {
        assert!(
            matches!(result, Err(DomainError::NotConfigured(_))),
            "{result:?}"
        );
    }
}
