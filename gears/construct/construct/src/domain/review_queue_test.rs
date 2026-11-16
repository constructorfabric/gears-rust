//! The Review Queue over an in-memory profile, writer and review store, with
//! stub policy services.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use serde_json::{Value, json};
use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::error::DomainError;
use crate::domain::one_step::OneStepPath;
use crate::domain::plan::Plan;
use crate::domain::plan::{Change, NodeKey, Origin};
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::planner::processing::{PlanWriter, Written};
use crate::domain::profile::Category;
use crate::domain::profile_writes::ProfileWrites;
use crate::domain::profile_writes::{SubjectFacts, SubjectProfiles};
use crate::domain::record_intake::DropReason;
use crate::domain::review_queue::{
    CLAIM_HOLD, NotStored, OutsideChange, Resolution, ReviewConflict, ReviewOutcome,
    ReviewQueueEvent, ReviewQueueService, ReviewRequest, ReviewStatus, ReviewText, close_on_change,
};
use crate::domain::sensitive_data::{SensitiveDataCheck, SensitiveDataChecks, SensitiveDataKind};
use crate::domain::subject_settings::{SubjectKey, SubjectSettings};
use crate::infra::processing_contexts::ServiceIdentity;
use crate::test_support::{
    AllowInOwnTenant, AllowResolver, DenyResolver, FailingCheck, FixedRules, FixedSubjectSettings,
    MemoryProfile, MemoryReviews, MemoryWriter, OwnerOnlyResolver, RecordingAdmissionEvents,
    RecordingEvents, RecordingResolver, RecordingReviewEvents, SubstringCheck, context_of,
    open_rules, stored_fact,
};

const ON: SubjectSettings = SubjectSettings {
    personalization_enabled: true,
    erasure_in_progress: false,
};

struct Fixture {
    tenant: Uuid,
    subject: Uuid,
    reviewer: Uuid,
    profile: Arc<MemoryProfile>,
    writer: Arc<MemoryWriter>,
    reviews: Arc<MemoryReviews>,
    events: Arc<RecordingReviewEvents>,
}

impl Fixture {
    /// A subject with a skill and a role, and a writer that conflicts
    /// `conflicts` times.
    fn new(conflicts: u32) -> Self {
        let profile = MemoryProfile::with(vec![
            stored_fact(Category::Skill, "skills", json!({ "name": "Rust" }), 10),
            stored_fact(Category::Role, "roles", json!({ "title": "Engineer" }), 20),
        ]);
        let writer = MemoryWriter::new(Arc::clone(&profile), conflicts);
        Self {
            tenant: Uuid::new_v4(),
            subject: Uuid::new_v4(),
            reviewer: Uuid::new_v4(),
            profile,
            writer,
            reviews: Arc::default(),
            events: Arc::default(),
        }
    }

    /// The service with the policy service `resolver`, the profile read
    /// through `profiles`, and corrections whose Admission reads the
    /// subject's settings as `settings` and whose checks are `checks`; none
    /// when `settings` is none.
    fn service_over(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        profiles: Arc<dyn SubjectProfiles>,
        settings: Option<SubjectSettings>,
        checks: Vec<Arc<dyn SensitiveDataCheck>>,
    ) -> ReviewQueueService {
        let writes = ProfileWrites::new(
            profiles,
            Arc::clone(&self.writer) as _,
            Arc::new(ServiceIdentity::new(Uuid::new_v4())),
            2,
        )
        .with_budget(std::time::Duration::from_secs(5));
        let corrections = settings.map(|settings| {
            let drops = Arc::new(RecordingEvents::default());
            Arc::new(OneStepPath {
                checks: SensitiveDataChecks::new(checks, Arc::clone(&drops) as _),
                admission: Admission::new(AdmissionPorts {
                    settings: Arc::new(FixedSubjectSettings(Ok(settings))),
                    rules: Arc::new(FixedRules(Ok(open_rules()))),
                    events: Arc::new(RecordingAdmissionEvents::default()),
                    drops,
                }),
                catalog: Arc::new(PropertyCatalog::load().unwrap()),
            })
        });
        ReviewQueueService::new(
            Arc::new(writes),
            Arc::clone(&self.reviews) as _,
            Arc::clone(&self.events) as _,
            corrections,
            PolicyEnforcer::new(resolver),
        )
    }

    fn service_with(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        settings: Option<SubjectSettings>,
    ) -> ReviewQueueService {
        self.service_over(
            resolver,
            Arc::clone(&self.profile) as _,
            settings,
            Vec::new(),
        )
    }

    fn service(&self) -> ReviewQueueService {
        self.service_with(Arc::new(AllowResolver), Some(ON))
    }

    fn owner(&self) -> SecurityContext {
        context_of(self.subject, self.tenant)
    }

    fn reviewer(&self) -> SecurityContext {
        context_of(self.reviewer, self.tenant)
    }

    fn skill(&self) -> NodeKey {
        self.profile
            .live()
            .into_iter()
            .find(|fact| fact.category == Category::Skill)
            .unwrap()
            .node_key
    }

    /// The skill flagged by the subject.
    async fn flagged(&self) -> ReviewRequest {
        self.service()
            .flag(
                &self.owner(),
                self.subject,
                &self.skill(),
                Some(ReviewText::new("wrong").unwrap()),
            )
            .await
            .unwrap()
    }

    fn corrected(name: &str) -> Resolution {
        Resolution::Corrected {
            property: "skills".to_owned(),
            value: json!({ "name": name }),
            reason: None,
        }
    }

    fn reject() -> Resolution {
        Resolution::Rejected {
            reason: ReviewText::new("it is right").unwrap(),
        }
    }

    /// The only request's status.
    fn status(&self) -> ReviewStatus {
        let [request] = self.reviews.all().try_into().unwrap();
        request.status
    }
}

/// The closed status's outcome, or a panic for an open one.
fn outcome_of(request: &ReviewRequest) -> &ReviewOutcome {
    match &request.status {
        ReviewStatus::Closed(closed) => &closed.outcome,
        ReviewStatus::Open { .. } => panic!("still open: {request:?}"),
    }
}

#[tokio::test]
async fn a_flag_opens_a_request_with_the_flagged_value_and_records_it() {
    let fixture = Fixture::new(0);
    let skill = fixture.skill();

    let request = fixture.flagged().await;

    assert_eq!(request.fact_id, skill);
    assert_eq!(request.property, "skills");
    assert_eq!(
        request.status,
        ReviewStatus::Open {
            value: json!({ "name": "Rust" })
        }
    );
    assert_eq!(
        request.comment.as_ref().map(ReviewText::as_str),
        Some("wrong")
    );
    assert_eq!(fixture.reviews.all(), std::slice::from_ref(&request));
    assert_eq!(
        fixture.events.taken(),
        [ReviewQueueEvent::Requested {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            request_id: request.id,
            fact_id: skill,
            category: Category::Skill,
        }]
    );
}

#[tokio::test]
async fn a_second_flag_on_a_fact_under_review_is_a_conflict() {
    let fixture = Fixture::new(0);
    fixture.flagged().await;

    let again = fixture
        .service()
        .flag(&fixture.owner(), fixture.subject, &fixture.skill(), None)
        .await;

    assert!(
        matches!(
            again,
            Err(DomainError::ReviewConflict(ReviewConflict::AlreadyOpen(_)))
        ),
        "{again:?}"
    );
    assert_eq!(fixture.reviews.all().len(), 1);
}

#[tokio::test]
async fn only_the_subject_flags_and_only_its_live_facts() {
    let fixture = Fixture::new(0);
    let service = fixture.service();
    let other = context_of(Uuid::new_v4(), fixture.tenant);

    let by_another = service
        .flag(&other, fixture.subject, &fixture.skill(), None)
        .await;
    let unknown = service
        .flag(
            &fixture.owner(),
            fixture.subject,
            &NodeKey("construct:t:nothing".to_owned()),
            None,
        )
        .await;
    let refused = fixture
        .service_with(Arc::new(DenyResolver), Some(ON))
        .flag(&fixture.owner(), fixture.subject, &fixture.skill(), None)
        .await;

    assert!(
        matches!(by_another, Err(DomainError::Forbidden(_))),
        "{by_another:?}"
    );
    assert!(
        matches!(unknown, Err(DomainError::NoSuchFact(_))),
        "{unknown:?}"
    );
    assert!(
        matches!(refused, Err(DomainError::Forbidden(_))),
        "{refused:?}"
    );
    assert!(fixture.reviews.all().is_empty());
}

/// Serves the fixture's facts on the first read and none after: the fact
/// is removed while the flag is stored.
struct Vanishing {
    profile: Arc<MemoryProfile>,
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl SubjectProfiles for Vanishing {
    async fn facts(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<SubjectFacts, DomainError> {
        let mut facts = self.profile.facts(ctx, key).await?;
        if self.reads.fetch_add(1, Ordering::SeqCst) > 0 {
            facts.facts.clear();
        }
        Ok(facts)
    }
}

#[tokio::test]
async fn a_flag_on_a_fact_removed_meanwhile_closes_its_request_and_is_no_such_fact() {
    let fixture = Fixture::new(0);
    let vanishing = Arc::new(Vanishing {
        profile: Arc::clone(&fixture.profile),
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    let service = fixture.service_over(Arc::new(AllowResolver), vanishing, Some(ON), Vec::new());

    let flagged = service
        .flag(&fixture.owner(), fixture.subject, &fixture.skill(), None)
        .await;

    assert!(
        matches!(flagged, Err(DomainError::NoSuchFact(_))),
        "{flagged:?}"
    );
    let [request] = fixture.reviews.all().try_into().unwrap();
    assert_eq!(outcome_of(&request), &ReviewOutcome::Deleted);
    let events = fixture.events.taken();
    assert!(
        matches!(
            events.as_slice(),
            [
                ReviewQueueEvent::Requested { request_id: opened, .. },
                ReviewQueueEvent::Resolved { request_id: closed, outcome: ReviewOutcome::Deleted, by, .. },
            ] if *opened == request.id && *closed == request.id && *by == fixture.subject
        ),
        "the request is announced, then closed: {events:?}"
    );
}

/// Serves the fixture's facts on the first read and fails after.
struct ReadOnce {
    profile: Arc<MemoryProfile>,
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl SubjectProfiles for ReadOnce {
    async fn facts(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<SubjectFacts, DomainError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) > 0 {
            return Err(DomainError::Unavailable("graph storage is down".to_owned()));
        }
        self.profile.facts(ctx, key).await
    }
}

#[tokio::test]
async fn a_flag_whose_fact_cannot_be_read_again_is_answered_with_its_stored_request() {
    let fixture = Fixture::new(0);
    let once = Arc::new(ReadOnce {
        profile: Arc::clone(&fixture.profile),
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    let service = fixture.service_over(Arc::new(AllowResolver), once, Some(ON), Vec::new());

    let flagged = service
        .flag(&fixture.owner(), fixture.subject, &fixture.skill(), None)
        .await
        .unwrap();

    let [stored] = fixture.reviews.all().try_into().unwrap();
    assert_eq!(flagged, stored);
    assert!(stored.is_open());
}

#[tokio::test]
async fn each_operation_asks_for_its_own_permission() {
    let fixture = Fixture::new(0);
    let resolver = Arc::new(RecordingResolver::default());
    let service = fixture.service_with(Arc::clone(&resolver) as _, Some(ON));

    service
        .flag(&fixture.owner(), fixture.subject, &fixture.skill(), None)
        .await
        .unwrap();
    service
        .own_requests(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await
        .unwrap();
    service
        .open_requests(&fixture.reviewer(), &ODataQuery::new())
        .await
        .unwrap();

    let asked: Vec<(String, String)> = resolver
        .asked
        .lock()
        .unwrap()
        .iter()
        .map(|(resource, action, _, _)| (resource.clone(), action.clone()))
        .collect();
    let facts = "gts.cf.construct.subject.fact.v1~".to_owned();
    assert_eq!(
        asked,
        [
            (facts.clone(), "flag".to_owned()),
            (facts, "read".to_owned()),
            (
                "gts.cf.construct.review.request.v1~".to_owned(),
                "resolve".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn the_subject_lists_its_requests_and_a_reviewer_the_open_ones() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let service = fixture.service();

    let own = service
        .own_requests(&fixture.owner(), fixture.subject, &ODataQuery::new())
        .await
        .unwrap();
    let open = service
        .open_requests(&fixture.reviewer(), &ODataQuery::new())
        .await
        .unwrap();

    assert_eq!(own.items, std::slice::from_ref(&request));
    assert_eq!(open.items, std::slice::from_ref(&request));
    assert_eq!(
        fixture.events.taken().last(),
        Some(&ReviewQueueEvent::Listed {
            tenant_id: fixture.tenant,
            by: fixture.reviewer,
            request_ids: vec![request.id],
        }),
        "the reviewer's read of flagged values is recorded"
    );
}

#[tokio::test]
async fn a_caller_without_the_permission_or_with_a_narrower_scope_is_no_reviewer() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let denied = fixture
        .service_with(Arc::new(DenyResolver), Some(ON))
        .open_requests(&fixture.reviewer(), &ODataQuery::new())
        .await;
    let narrower = fixture
        .service_with(Arc::new(OwnerOnlyResolver), Some(ON))
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await;

    assert!(
        matches!(denied, Err(DomainError::Forbidden(_))),
        "{denied:?}"
    );
    assert!(
        matches!(narrower, Err(DomainError::Forbidden(_))),
        "{narrower:?}"
    );
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
}

#[tokio::test]
async fn a_reviewer_may_not_resolve_a_request_on_its_own_fact() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let own = fixture
        .service()
        .resolve(&fixture.owner(), request.id, Fixture::corrected("Go"))
        .await;

    assert!(matches!(own, Err(DomainError::Forbidden(_))), "{own:?}");
    assert!(fixture.writer.plans().is_empty(), "nothing is written");
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
}

#[tokio::test]
async fn a_rejection_closes_the_request_with_its_reason_and_keeps_the_fact() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let closed = fixture
        .service_with(Arc::new(AllowInOwnTenant), None)
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await
        .unwrap();

    let ReviewStatus::Closed(resolution) = &closed.status else {
        panic!("closed: {closed:?}");
    };
    assert_eq!(resolution.outcome, ReviewOutcome::Rejected);
    assert_eq!(
        resolution.reason.as_ref().map(ReviewText::as_str),
        Some("it is right")
    );
    assert_eq!(resolution.by, fixture.reviewer);
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .any(|f| f.node_key == request.fact_id)
    );
    assert!(fixture.writer.plans().is_empty(), "nothing is written");
    assert_eq!(
        fixture.events.taken().last(),
        Some(&ReviewQueueEvent::Resolved {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            request_id: request.id,
            fact_id: request.fact_id,
            outcome: ReviewOutcome::Rejected,
            by: fixture.reviewer,
        })
    );
    assert_eq!(fixture.reviews.held(), [false], "no claim is left");
}

#[tokio::test]
async fn a_delete_removes_the_fact_as_the_reviewer_and_closes_the_request() {
    let fixture = Fixture::new(1);
    let request = fixture.flagged().await;

    let closed = fixture
        .service_with(Arc::new(AllowResolver), None)
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Deleted { reason: None },
        )
        .await
        .unwrap();

    assert_eq!(outcome_of(&closed), &ReviewOutcome::Deleted);
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|f| f.node_key != request.fact_id)
    );
    let plans = fixture.writer.plans();
    assert_eq!(plans.len(), 2, "written again after the conflict");
    assert_eq!(
        plans[1].origin,
        Origin::Reviewer {
            reviewer: fixture.reviewer
        }
    );
    assert!(matches!(
        &plans[1].steps[0].change,
        Change::RemoveEntity { entity } if *entity == request.fact_id
    ));
}

#[tokio::test]
async fn a_delete_of_a_fact_already_gone_closes_the_request_as_deleted() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture.profile.facts.lock().unwrap().clear();

    let closed = fixture
        .service()
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Deleted { reason: None },
        )
        .await
        .unwrap();

    assert_eq!(outcome_of(&closed), &ReviewOutcome::Deleted);
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn a_correction_replaces_the_fact_under_a_new_key_as_the_reviewer() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let closed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await
        .unwrap();

    let ReviewOutcome::Corrected(new_key) = outcome_of(&closed).clone() else {
        panic!("corrected: {closed:?}");
    };
    assert_ne!(
        new_key, request.fact_id,
        "a replace gives the fact a new id"
    );
    let [plan] = fixture.writer.plans().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Reviewer {
            reviewer: fixture.reviewer
        }
    );
    assert_eq!(plan.profile_version, Some(1));
    assert!(matches!(
        &plan.steps[..],
        [step] if step.confidence.is_none() && matches!(
            &step.change,
            Change::ReplaceEntity { old, key, properties }
                if *old == request.fact_id && *key == new_key
                    && properties[0].value == json!({ "name": "Go" })
        )
    ));
    let live = fixture.profile.live();
    assert!(live.iter().all(|fact| fact.node_key != request.fact_id));
    assert!(live.iter().any(|fact| fact.node_key == new_key));
    assert!(matches!(
        fixture.events.taken().last(),
        Some(ReviewQueueEvent::Resolved {
            outcome: ReviewOutcome::Corrected(key),
            ..
        }) if *key == new_key
    ));
}

#[tokio::test]
async fn a_correction_rerun_after_a_conflict_keeps_the_key_it_stored() {
    let fixture = Fixture::new(1);
    let request = fixture.flagged().await;

    let closed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await
        .unwrap();

    let ReviewOutcome::Corrected(new_key) = outcome_of(&closed).clone() else {
        panic!("corrected: {closed:?}");
    };
    let plans = fixture.writer.plans();
    assert_eq!(plans.len(), 2, "written again after the conflict");
    assert!(matches!(
        &plans[1].steps[0].change,
        Change::ReplaceEntity { key, .. } if *key == new_key
    ));
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .any(|fact| fact.node_key == new_key)
    );
}

#[tokio::test]
async fn a_correction_admission_drops_stores_nothing_and_gives_the_request_back() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let off = SubjectSettings {
        personalization_enabled: false,
        erasure_in_progress: false,
    };

    let refused = fixture
        .service_with(Arc::new(AllowResolver), Some(off))
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;

    assert!(
        matches!(
            refused,
            Err(DomainError::NotStored(NotStored::Dropped(
                DropReason::PersonalizationOff
            )))
        ),
        "{refused:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
    assert_eq!(fixture.reviews.held(), [false], "the claim is given back");
}

#[tokio::test]
async fn a_correction_whose_value_is_blocked_stores_nothing_and_stays_open() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let blocking: Arc<dyn SensitiveDataCheck> = Arc::new(SubstringCheck {
        kind: SensitiveDataKind::Credentials,
        needle: "secret",
    });

    let refused = fixture
        .service_over(
            Arc::new(AllowResolver),
            Arc::clone(&fixture.profile) as _,
            Some(ON),
            vec![blocking],
        )
        .resolve(
            &fixture.reviewer(),
            request.id,
            Fixture::corrected("a secret"),
        )
        .await;

    assert!(
        matches!(refused, Err(DomainError::NotStored(NotStored::Blocked))),
        "{refused:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
}

#[tokio::test]
async fn a_correction_of_a_fact_already_gone_closes_the_request_as_deleted() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture.profile.facts.lock().unwrap().clear();

    let closed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await
        .unwrap();

    assert_eq!(outcome_of(&closed), &ReviewOutcome::Deleted);
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn a_correction_takes_only_the_facts_property_with_a_value_its_schema_takes() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let service = fixture.service();
    let reviewer = fixture.reviewer();
    let resolve = |property: &str, value: Value| {
        service.resolve(
            &reviewer,
            request.id,
            Resolution::Corrected {
                property: property.to_owned(),
                value,
                reason: None,
            },
        )
    };

    let other_property = resolve("roles", json!({ "title": "Lead" })).await;
    let bad_value = resolve("skills", json!({ "name": "" })).await;

    assert!(
        matches!(
            other_property,
            Err(DomainError::InvalidValue {
                field: "property",
                ..
            })
        ),
        "{other_property:?}"
    );
    assert!(
        matches!(
            bad_value,
            Err(DomainError::InvalidValue { field: "value", .. })
        ),
        "{bad_value:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
}

#[tokio::test]
async fn a_corrections_value_is_checked_before_its_fact_is_read() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture
        .profile
        .facts
        .lock()
        .unwrap()
        .retain(|fact| fact.node_key != request.fact_id);

    let bad_value = fixture
        .service()
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Corrected {
                property: "skills".to_owned(),
                value: json!({ "name": "" }),
                reason: None,
            },
        )
        .await;

    assert!(
        matches!(
            bad_value,
            Err(DomainError::InvalidValue { field: "value", .. })
        ),
        "{bad_value:?}"
    );
    assert!(
        matches!(fixture.status(), ReviewStatus::Open { .. }),
        "a value no schema takes does not close the request"
    );
}

#[tokio::test]
async fn a_correction_of_a_stored_property_no_person_type_has_is_internal() {
    let fixture = Fixture::new(0);
    let odd = stored_fact(Category::Skill, "retired_property", json!("x"), 30);
    fixture.profile.facts.lock().unwrap().push(odd.clone());
    let request = fixture
        .service()
        .flag(&fixture.owner(), fixture.subject, &odd.node_key, None)
        .await
        .unwrap();

    let corrected = fixture
        .service()
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Corrected {
                property: "retired_property".to_owned(),
                value: json!("y"),
                reason: None,
            },
        )
        .await;

    assert!(
        matches!(&corrected, Err(DomainError::Internal(why)) if why.contains(&odd.node_key.0)),
        "{corrected:?}"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn without_the_opt_in_a_correction_is_not_configured_and_the_rest_works() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let service = fixture.service_with(Arc::new(AllowResolver), None);

    let corrected = service
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;
    let rejected = service
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await;

    assert!(
        matches!(corrected, Err(DomainError::NotConfigured(_))),
        "{corrected:?}"
    );
    assert_eq!(outcome_of(&rejected.unwrap()), &ReviewOutcome::Rejected);
}

#[tokio::test]
async fn a_request_another_resolution_holds_is_a_conflict_until_its_hold_ends() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let now = time::OffsetDateTime::now_utc();
    fixture
        .reviews
        .hold_first(now - CLAIM_HOLD + std::time::Duration::from_secs(5));

    let held = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;
    fixture
        .reviews
        .hold_first(now - CLAIM_HOLD - std::time::Duration::from_secs(5));
    let after_the_hold = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await;

    assert!(
        matches!(
            held,
            Err(DomainError::ReviewConflict(ReviewConflict::Held(id))) if id == request.id
        ),
        "{held:?}"
    );
    assert_eq!(
        fixture.writer.plans().len(),
        0,
        "nothing is written while it is held"
    );
    assert_eq!(
        outcome_of(&after_the_hold.unwrap()),
        &ReviewOutcome::Rejected,
        "a hold older than CLAIM_HOLD is taken over"
    );
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_request_closed_while_its_correction_was_written_is_a_conflict_that_says_so() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture
        .reviews
        .deleted_while_written
        .store(true, Ordering::SeqCst);

    let lost = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;

    assert!(
        matches!(
            &lost,
            Err(DomainError::ReviewConflict(
                ReviewConflict::ClosedAfterWrite {
                    outcome: ReviewOutcome::Corrected(_),
                    ..
                }
            ))
        ),
        "{lost:?}"
    );
    assert_eq!(
        fixture.writer.plans().len(),
        1,
        "the correction was written"
    );
    let [closed] = fixture.reviews.all().try_into().unwrap();
    assert_eq!(
        outcome_of(&closed),
        &ReviewOutcome::Deleted,
        "as the subject closed it"
    );
    assert!(logs_contain("was written, but its request was not closed"));
    assert!(
        matches!(
            fixture.events.taken().as_slice(),
            [ReviewQueueEvent::Requested { .. }]
        ),
        "no resolved event from the reviewer"
    );
}

/// A writer that fails every write, after it may have landed.
struct FailingWriter;

#[async_trait]
impl PlanWriter for FailingWriter {
    async fn write_by(
        &self,
        _: &SecurityContext,
        _: &Plan,
        _: tokio::time::Instant,
    ) -> Result<Written, DomainError> {
        Err(DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

/// A writer that stores each plan through `inner`, then fails.
struct LandsThenFails {
    inner: Arc<MemoryWriter>,
}

#[async_trait]
impl PlanWriter for LandsThenFails {
    async fn write_by(
        &self,
        ctx: &SecurityContext,
        plan: &Plan,
        _: tokio::time::Instant,
    ) -> Result<Written, DomainError> {
        self.inner.write(ctx, plan).await?;
        Err(DomainError::Unavailable("the answer was lost".to_owned()))
    }
}

#[tokio::test]
async fn a_failed_write_found_in_the_profile_closes_the_request() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let writes = ProfileWrites::new(
        Arc::clone(&fixture.profile) as _,
        Arc::new(LandsThenFails {
            inner: Arc::clone(&fixture.writer),
        }),
        Arc::new(ServiceIdentity::new(Uuid::new_v4())),
        2,
    );
    let service = ReviewQueueService::new(
        Arc::new(writes),
        Arc::clone(&fixture.reviews) as _,
        Arc::clone(&fixture.events) as _,
        None,
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    );

    let deleted = service
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Deleted { reason: None },
        )
        .await
        .unwrap();

    assert_eq!(outcome_of(&deleted), &ReviewOutcome::Deleted);
    assert!(
        !fixture
            .profile
            .live()
            .iter()
            .any(|fact| fact.node_key == request.fact_id)
    );
}

#[tokio::test]
async fn a_rejection_whose_close_failed_gives_its_claim_back() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture.reviews.close_fails.store(true, Ordering::SeqCst);

    let failed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await;
    let held = fixture.reviews.held();
    fixture.reviews.close_fails.store(false, Ordering::SeqCst);
    let again = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await
        .unwrap();

    // It wrote nothing, so it neither says it was stored nor holds the request.
    assert!(
        matches!(failed, Err(DomainError::Unavailable(_))),
        "{failed:?}"
    );
    assert_eq!(held, [false], "the claim is given back");
    assert_eq!(
        outcome_of(&again),
        &ReviewOutcome::Rejected,
        "resolved at once"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn a_request_closed_before_its_write_was_recorded_answers_closed_not_held() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture
        .reviews
        .deleted_before_pend
        .store(true, Ordering::SeqCst);

    let refused = fixture
        .service()
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Deleted { reason: None },
        )
        .await;

    assert!(
        matches!(
            refused,
            Err(DomainError::ReviewConflict(ReviewConflict::Closed(id))) if id == request.id
        ),
        "{refused:?}"
    );
    assert!(fixture.writer.plans().is_empty(), "nothing written");
}

#[tokio::test]
async fn the_subjects_delete_after_a_landed_correction_closes_the_request_as_the_correction() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture.reviews.close_fails.store(true, Ordering::SeqCst);
    let unclosed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;
    let Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
        outcome: ReviewOutcome::Corrected(new_key),
        ..
    })) = unclosed
    else {
        panic!("{unclosed:?}");
    };
    fixture.reviews.close_fails.store(false, Ordering::SeqCst);
    let writes = ProfileWrites::new(
        Arc::clone(&fixture.profile) as _,
        Arc::clone(&fixture.writer) as _,
        Arc::new(ServiceIdentity::new(Uuid::new_v4())),
        2,
    );

    // The subject deletes the corrected fact, which the correction already
    // replaced: it finds it gone.
    let closed = close_on_change(
        fixture.reviews.as_ref(),
        fixture.events.as_ref(),
        &writes,
        &AccessScope::allow_all(),
        request.subject(),
        &request.fact_id,
        OutsideChange {
            by: fixture.subject,
            outcome: ReviewOutcome::Deleted,
            changed: false,
        },
    )
    .await
    .unwrap();

    assert!(closed);
    let ReviewStatus::Closed(done) = fixture.status() else {
        panic!("closed");
    };
    assert_eq!(
        (done.outcome, done.by),
        (ReviewOutcome::Corrected(new_key), fixture.reviewer),
        "closed as the reviewer's correction, not as the subject's delete"
    );
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_correction_whose_close_failed_is_closed_as_it_by_the_next_resolution() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture.reviews.close_fails.store(true, Ordering::SeqCst);

    let unclosed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;
    let Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
        request_id,
        outcome: ReviewOutcome::Corrected(new_key),
    })) = unclosed
    else {
        panic!("{unclosed:?}");
    };
    let held_while_open = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await;
    fixture.reviews.close_fails.store(false, Ordering::SeqCst);
    fixture
        .reviews
        .claimed_first_at(time::OffsetDateTime::now_utc() - CLAIM_HOLD);
    let next = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await
        .unwrap();

    assert_eq!(request_id, request.id);
    assert!(
        matches!(
            held_while_open,
            Err(DomainError::ReviewConflict(ReviewConflict::Held(_)))
        ),
        "{held_while_open:?}"
    );
    assert_eq!(
        outcome_of(&next),
        &ReviewOutcome::Corrected(new_key.clone()),
        "closed as the correction that is in the profile, not as the rejection"
    );
    assert_eq!(fixture.writer.plans().len(), 1, "written once");
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .any(|fact| fact.node_key == new_key)
    );
    assert!(logs_contain("was written, but its request was not closed"));
    assert!(matches!(
        fixture.events.taken().last(),
        Some(ReviewQueueEvent::Resolved {
            outcome: ReviewOutcome::Corrected(_),
            ..
        })
    ));
}

#[tokio::test]
async fn a_write_that_may_have_landed_keeps_the_claim() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let writes = ProfileWrites::new(
        Arc::clone(&fixture.profile) as _,
        Arc::new(FailingWriter),
        Arc::new(ServiceIdentity::new(Uuid::new_v4())),
        2,
    );
    let service = ReviewQueueService::new(
        Arc::new(writes),
        Arc::clone(&fixture.reviews) as _,
        Arc::clone(&fixture.events) as _,
        None,
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    );

    let failed = service
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Deleted { reason: None },
        )
        .await;

    // Not a plain 503: a retry now would find the request held.
    assert!(
        matches!(
            failed,
            Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
                outcome: ReviewOutcome::Deleted,
                ..
            }))
        ),
        "{failed:?}"
    );
    assert_eq!(
        fixture.reviews.held(),
        [true],
        "held until CLAIM_HOLD, so nobody resolves it on a guess"
    );
    fixture
        .reviews
        .claimed_first_at(time::OffsetDateTime::now_utc() - CLAIM_HOLD);
    let next = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await
        .unwrap();
    assert_eq!(
        outcome_of(&next),
        &ReviewOutcome::Rejected,
        "the delete is not in the profile, so the next resolution goes on as asked"
    );
}

#[tokio::test(start_paused = true)]
async fn a_claim_answered_after_the_budget_writes_nothing_and_holds_until_it_lapses() {
    let budget = std::time::Duration::from_secs(5);
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let flagged = fixture.events.taken();
    *fixture.reviews.claim_delay.lock().unwrap() = budget * 2;
    let started = tokio::time::Instant::now();

    let resolved = fixture
        .service()
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Deleted { reason: None },
        )
        .await;

    assert!(
        matches!(resolved, Err(DomainError::Unavailable(_))),
        "{resolved:?}"
    );
    assert_eq!(started.elapsed(), budget);
    assert!(fixture.writer.plans().is_empty());
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
    assert_eq!(
        fixture.reviews.held(),
        [true],
        "the late claim was taken; it frees itself after its hold"
    );
    assert_eq!(fixture.events.taken(), flagged, "nothing recorded");
}

#[tokio::test(start_paused = true)]
async fn a_record_answered_after_the_budget_writes_nothing_and_gives_the_claim_back() {
    let budget = std::time::Duration::from_secs(5);
    for resolution in [
        Resolution::Deleted { reason: None },
        Fixture::corrected("Go"),
    ] {
        let fixture = Fixture::new(0);
        let request = fixture.flagged().await;
        let flagged = fixture.events.taken();
        *fixture.reviews.pend_delay.lock().unwrap() = budget * 2;
        let started = tokio::time::Instant::now();

        let resolved = fixture
            .service()
            .resolve(&fixture.reviewer(), request.id, resolution)
            .await;

        assert!(
            matches!(resolved, Err(DomainError::Unavailable(_))),
            "{resolved:?}"
        );
        assert_eq!(started.elapsed(), budget);
        assert!(fixture.writer.plans().is_empty(), "nothing was written");
        assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
        assert_eq!(fixture.reviews.held(), [false], "the claim was given back");
        assert_eq!(fixture.reviews.pending_of(request.id), None);
        assert_eq!(fixture.events.taken(), flagged, "nothing recorded");
    }
}

#[test]
fn a_claim_outlasts_the_budget_and_the_writer() {
    use crate::domain::profile_writes::{WRITE_BUDGET, WRITER_LIMIT};

    assert!(CLAIM_HOLD > WRITE_BUDGET + WRITER_LIMIT);
}

#[tokio::test]
async fn a_correction_whose_check_fails_stores_nothing_and_gives_the_request_back() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let refused = fixture
        .service_over(
            Arc::new(AllowResolver),
            Arc::clone(&fixture.profile) as _,
            Some(ON),
            vec![Arc::new(FailingCheck(SensitiveDataKind::Health))],
        )
        .resolve(&fixture.reviewer(), request.id, Fixture::corrected("Go"))
        .await;

    assert!(
        matches!(
            refused,
            Err(DomainError::NotStored(NotStored::Dropped(
                DropReason::CheckFailed
            )))
        ),
        "{refused:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
    assert_eq!(fixture.reviews.held(), [false]);
    assert_eq!(fixture.reviews.pending_of(request.id), None);
    assert_eq!(
        fixture.events.taken().last(),
        Some(&ReviewQueueEvent::CorrectionRefused {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            request_id: request.id,
            fact_id: request.fact_id.clone(),
            reason: NotStored::Dropped(DropReason::CheckFailed),
            by: fixture.reviewer,
        })
    );
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_rejection_of_a_fact_gone_closes_the_request_as_deleted() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    fixture.profile.facts.lock().unwrap().clear();

    let closed = fixture
        .service()
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await
        .unwrap();

    assert_eq!(outcome_of(&closed), &ReviewOutcome::Deleted);
    assert!(logs_contain("a resolution found its fact gone"));
}

#[tokio::test]
async fn a_closed_or_unknown_request_cannot_be_resolved() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let service = fixture.service();
    service
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await
        .unwrap();

    let again = service
        .resolve(&fixture.reviewer(), request.id, Fixture::reject())
        .await;
    let unknown = service
        .resolve(&fixture.reviewer(), Uuid::new_v4(), Fixture::reject())
        .await;
    let elsewhere = service
        .resolve(
            &context_of(fixture.reviewer, Uuid::new_v4()),
            request.id,
            Fixture::reject(),
        )
        .await;

    assert!(
        matches!(
            again,
            Err(DomainError::ReviewConflict(ReviewConflict::Closed(_)))
        ),
        "{again:?}"
    );
    assert!(
        matches!(unknown, Err(DomainError::NoSuchReviewRequest(_))),
        "{unknown:?}"
    );
    assert!(
        matches!(elsewhere, Err(DomainError::NoSuchReviewRequest(_))),
        "a reviewer of another tenant does not find it: {elsewhere:?}"
    );
}
