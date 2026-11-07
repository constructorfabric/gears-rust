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
use crate::domain::profile_writes::{ProfileWrites, WRITE_BUDGET};
use crate::domain::review_queue::{Closed, ReviewOutcome, ReviewQueueEvent, ReviewStatus};
use crate::domain::subject_control::{SubjectControlEvent, SubjectControlService};
use crate::domain::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsReader};
use crate::test_support::{
    AllowResolver, DenyResolver, FixedSubjectSettings, MemoryProfile, MemoryReviews, MemoryWriter,
    RecordingResolver, RecordingReviewEvents, RecordingSubjectControlEvents, SlowResolver,
    context_of, stored_fact, subject_control, subject_control_reviewed,
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
    reviews: Arc<MemoryReviews>,
    review_events: Arc<RecordingReviewEvents>,
    budget: Duration,
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
            reviews: Arc::default(),
            review_events: Arc::default(),
            budget: WRITE_BUDGET,
        }
    }

    /// The service over the fixture, with the settings `settings`.
    fn service_with(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        settings: Arc<dyn SubjectSettingsReader>,
    ) -> SubjectControlService {
        let writes = ProfileWrites::new(
            Arc::clone(&self.profile) as _,
            Arc::clone(&self.writer) as _,
            Arc::new(crate::infra::processing_contexts::ServiceIdentity::new(
                self.identity,
            )),
            2,
        )
        .with_budget(self.budget);
        subject_control_reviewed(
            resolver,
            Arc::new(writes),
            settings,
            Arc::clone(&self.events),
            Arc::clone(&self.reviews),
            Arc::clone(&self.review_events),
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

    let categories: Vec<Category> = view
        .page
        .items
        .iter()
        .map(|item| item.fact.category)
        .collect();
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
            caller: fixture.subject,
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
            caller: fixture.subject,
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
    let facts = toolkit_gts::gts_id!("cf.construct.subject.fact.v1~").to_owned();
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

    assert_eq!(found.fact, skill);
    assert_eq!(found.review_request, None);
    assert!(
        matches!(missing, Err(DomainError::NoSuchFact(_))),
        "{missing:?}"
    );
    assert_eq!(
        fixture.events.taken(),
        [SubjectControlEvent::Viewed {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            caller: fixture.subject,
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
        view.page
            .items
            .iter()
            .all(|item| item.fact.node_key != skill),
        "not served from the next read"
    );
    assert_eq!(
        fixture.events.taken()[0],
        SubjectControlEvent::Deleted {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            caller: fixture.subject,
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

#[tokio::test(start_paused = true)]
async fn a_delete_gives_up_as_unavailable_once_the_reruns_are_spent() {
    let fixture = Fixture::new(10);
    let service = fixture.service(Arc::new(AllowResolver));
    let started = tokio::time::Instant::now();

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
    // Pauses of 20 to 40 and 40 to 80 ms before the reruns, and none after
    // the last write, which would add at least 80 ms more.
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(60) && waited <= Duration::from_millis(120),
        "{waited:?}"
    );
    assert_eq!(fixture.profile.live().len(), 2, "nothing was deleted");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn a_delete_gives_up_as_unavailable_once_its_budget_is_spent() {
    let mut fixture = Fixture::new(10);
    fixture.budget = Duration::ZERO;
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
    assert!(
        fixture.writer.plans().is_empty(),
        "no write past the budget"
    );
    assert_eq!(fixture.profile.live().len(), 2, "nothing was deleted");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_read_past_the_budget_is_cut_off_and_nothing_is_written() {
    let mut fixture = Fixture::new(0);
    *fixture.profile.delay.lock().unwrap() = Duration::from_secs(30);
    fixture.budget = Duration::from_secs(20);
    let service = fixture.service(Arc::new(AllowResolver));
    let started = tokio::time::Instant::now();

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
        started.elapsed(),
        Duration::from_secs(20),
        "answered when the budget ran out, not when the read did"
    );
    assert!(fixture.writer.plans().is_empty());
    assert_eq!(fixture.profile.live().len(), 2, "nothing was deleted");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_policy_service_that_outlasts_the_budget_is_cut_off_and_nothing_is_read_or_written() {
    let mut fixture = Fixture::new(0);
    fixture.budget = Duration::from_secs(20);
    let service = fixture.service(Arc::new(SlowResolver(Duration::from_secs(30))));
    let started = tokio::time::Instant::now();

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
        started.elapsed(),
        Duration::from_secs(20),
        "answered when the budget ran out, not when the policy service did"
    );
    assert!(fixture.profile.reads.lock().unwrap().is_empty());
    assert!(fixture.writer.plans().is_empty());
    assert_eq!(fixture.profile.live().len(), 2, "nothing was deleted");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_rerun_whose_read_outlasts_the_budget_writes_nothing_more() {
    let mut fixture = Fixture::new(10);
    // The first read ends at 15 s, inside the budget, so the first write
    // runs; the rerun's read would end at 30 s.
    *fixture.profile.delay.lock().unwrap() = Duration::from_secs(15);
    fixture.budget = Duration::from_secs(20);
    let service = fixture.service(Arc::new(AllowResolver));
    let started = tokio::time::Instant::now();

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
    assert_eq!(started.elapsed(), Duration::from_secs(20));
    assert_eq!(fixture.writer.plans().len(), 1, "only the first write");
    assert_eq!(fixture.profile.live().len(), 2, "nothing was deleted");
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

#[tokio::test]
async fn a_profile_too_large_to_read_is_the_gears_own_failure_for_the_view_the_export_and_the_delete()
 {
    let fixture = Fixture::new(0);
    fixture
        .profile
        .too_large
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let service = fixture.service(Arc::new(AllowResolver));
    let skill = fixture.fact_id(Category::Skill);

    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::default())
        .await;
    let export = service.export(&fixture.owner(), fixture.subject).await;
    let delete = service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await;

    // Not the subject's state: removing facts is what it cannot do then.
    assert!(matches!(view, Err(DomainError::Internal(_))), "{view:?}");
    assert!(
        matches!(export, Err(DomainError::Internal(_))),
        "{export:?}"
    );
    assert!(
        matches!(delete, Err(DomainError::Internal(_))),
        "{delete:?}"
    );
    assert!(fixture.writer.plans().is_empty(), "nothing written");
}

#[tokio::test]
async fn each_fact_carries_its_latest_review_request_and_the_export_holds_them_all() {
    let fixture = Fixture::new(0);
    let skill = fixture.profile.live()[0].clone();
    let older = fixture.reviews.open_on_at(fixture.key(), &skill, 100);
    fixture.reviews.requests.lock().unwrap()[0].0.status = ReviewStatus::Closed(Closed {
        outcome: ReviewOutcome::Rejected,
        by: Uuid::new_v4(),
        at: time::OffsetDateTime::from_unix_timestamp(150).unwrap(),
        reason: None,
    });
    let newer = fixture.reviews.open_on_at(fixture.key(), &skill, 200);
    let service = fixture.service(Arc::new(AllowResolver));

    let view = service
        .view(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await
        .unwrap();
    let one = service
        .fact(&fixture.owner(), fixture.subject, &skill.node_key)
        .await
        .unwrap();
    let export = service
        .export(&fixture.owner(), fixture.subject)
        .await
        .unwrap();

    let requests: Vec<Option<Uuid>> = view
        .page
        .items
        .iter()
        .map(|item| item.review_request.as_ref().map(|request| request.id))
        .collect();
    assert_eq!(
        requests,
        [None, Some(newer.id)],
        "the role has none; the skill shows its newer, open request"
    );
    assert_eq!(one.review_request.map(|request| request.id), Some(newer.id));
    let exported: Vec<Uuid> = export.review_requests.iter().map(|r| r.id).collect();
    assert_eq!(exported, [newer.id, older.id]);
}

#[tokio::test]
async fn the_subjects_delete_closes_an_open_request_on_the_fact_as_deleted() {
    let fixture = Fixture::new(0);
    let skill = fixture.fact_id(Category::Skill);
    let stored = fixture.profile.live();
    fixture.reviews.open_on(
        fixture.key(),
        stored.iter().find(|fact| fact.node_key == skill).unwrap(),
    );
    let service = fixture.service(Arc::new(AllowResolver));

    service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await
        .unwrap();

    let [closed] = fixture.reviews.all().try_into().unwrap();
    let ReviewStatus::Closed(resolution) = closed.status else {
        panic!("closed");
    };
    assert_eq!(resolution.outcome, ReviewOutcome::Deleted);
    assert_eq!(resolution.by, fixture.subject);
    assert!(matches!(
        fixture.review_events.taken().as_slice(),
        [ReviewQueueEvent::Resolved {
            outcome: ReviewOutcome::Deleted,
            ..
        }]
    ));
}

#[tokio::test]
async fn a_delete_whose_request_cannot_be_closed_is_unavailable_and_its_repeat_closes_it() {
    let fixture = Fixture::new(0);
    let skill = fixture.fact_id(Category::Skill);
    let stored = fixture.profile.live();
    fixture.reviews.open_on(
        fixture.key(),
        stored.iter().find(|fact| fact.node_key == skill).unwrap(),
    );
    fixture
        .reviews
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let service = fixture.service(Arc::new(AllowResolver));

    let first = service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await;
    fixture
        .reviews
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let repeat = service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await;
    let once_more = service
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await;

    assert!(
        matches!(first, Err(DomainError::Unavailable(_))),
        "{first:?}"
    );
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|fact| fact.node_key != skill)
    );
    assert!(repeat.is_ok(), "the repeat closes the request: {repeat:?}");
    assert!(!fixture.reviews.all()[0].is_open());
    assert!(
        matches!(once_more, Err(DomainError::NoSuchFact(_))),
        "{once_more:?}"
    );
}

/// A writer that takes `delay`, then writes; or writes, then fails, when
/// `fails`.
struct OddWriter {
    inner: Arc<MemoryWriter>,
    delay: Duration,
    fails: bool,
}

#[async_trait]
impl crate::domain::planner::processing::PlanWriter for OddWriter {
    async fn write(
        &self,
        ctx: &SecurityContext,
        plan: &crate::domain::plan::Plan,
    ) -> Result<crate::domain::planner::processing::Written, DomainError> {
        tokio::time::sleep(self.delay).await;
        let written = self.inner.write(ctx, plan).await?;
        if self.fails {
            return Err(DomainError::Unavailable("the answer was lost".to_owned()));
        }
        Ok(written)
    }
}

impl Fixture {
    /// The service over a writer that takes `delay` and fails after it
    /// wrote when `fails`, with a budget of 20 ms.
    fn service_over_odd_writer(&self, delay: Duration, fails: bool) -> SubjectControlService {
        let writes = ProfileWrites::new(
            Arc::clone(&self.profile) as _,
            Arc::new(OddWriter {
                inner: Arc::clone(&self.writer),
                delay,
                fails,
            }),
            Arc::new(crate::infra::processing_contexts::ServiceIdentity::new(
                self.identity,
            )),
            2,
        )
        .with_budget(Duration::from_millis(20));
        subject_control_reviewed(
            Arc::new(AllowResolver),
            Arc::new(writes),
            Arc::new(FixedSubjectSettings(Ok(OFF_AND_ERASING))),
            Arc::clone(&self.events),
            Arc::clone(&self.reviews),
            Arc::clone(&self.review_events),
        )
    }
}

#[tokio::test(start_paused = true)]
async fn a_write_that_started_runs_past_the_budget_to_its_end() {
    let fixture = Fixture::new(0);
    let skill = fixture.fact_id(Category::Skill);

    fixture
        .service_over_odd_writer(Duration::from_millis(100), false)
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await
        .expect("a started write is not cut off by the budget");

    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|fact| fact.node_key != skill)
    );
}

#[tokio::test(start_paused = true)]
async fn a_writer_past_its_limit_is_a_delete_to_send_again_with_the_request_open() {
    use crate::domain::profile_writes::WRITER_LIMIT;

    let fixture = Fixture::new(0);
    let skill = fixture.fact_id(Category::Skill);
    let fact = fixture
        .profile
        .live()
        .into_iter()
        .find(|f| f.node_key == skill);
    fixture.reviews.open_on(fixture.key(), &fact.unwrap());

    let deleted = fixture
        .service_over_odd_writer(WRITER_LIMIT + Duration::from_secs(1), false)
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await;

    assert!(
        matches!(&deleted, Err(DomainError::Unavailable(msg)) if msg.contains("send it again")),
        "{deleted:?}"
    );
    assert!(
        fixture.reviews.all()[0].is_open(),
        "the fact is still there"
    );
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn a_delete_whose_writer_failed_after_it_landed_is_done() {
    let fixture = Fixture::new(0);
    let skill = fixture.fact_id(Category::Skill);
    let fact = fixture
        .profile
        .live()
        .into_iter()
        .find(|f| f.node_key == skill);
    fixture.reviews.open_on(fixture.key(), &fact.unwrap());

    fixture
        .service_over_odd_writer(Duration::ZERO, true)
        .delete(&fixture.owner(), fixture.subject, &skill)
        .await
        .expect("the delete is in the profile");

    assert!(!fixture.reviews.all()[0].is_open(), "its request is closed");
    assert!(matches!(
        fixture.events.taken().as_slice(),
        [SubjectControlEvent::Deleted { .. }]
    ));
}
