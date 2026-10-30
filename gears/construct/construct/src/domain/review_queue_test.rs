//! The Review Queue over an in-memory profile, writer and review store, with
//! stub policy services.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use serde_json::{Value, json};
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Origin};
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::profile::Category;
use crate::domain::record_intake::DropReason;
use crate::domain::review_queue::{
    Corrections, NotStored, Resolution, ReviewOutcome, ReviewQueueEvent, ReviewQueueService,
    ReviewRequest, ReviewState,
};
use crate::domain::sensitive_data::SensitiveDataChecks;
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    AllowInOwnTenant, AllowResolver, DenyResolver, FixedRules, FixedSubjectSettings, MemoryProfile,
    MemoryReviews, MemoryWriter, RecordingAdmissionEvents, RecordingEvents, RecordingResolver,
    RecordingReviewEvents, context_of, open_rules, stored_fact, subject_control_reviewed,
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

    /// The service with the policy service `resolver`, and corrections whose
    /// Admission reads the subject's settings as `settings`; none when
    /// `settings` is none.
    fn service_with(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        settings: Option<SubjectSettings>,
    ) -> ReviewQueueService {
        let control = subject_control_reviewed(
            Arc::clone(&resolver),
            Arc::clone(&self.profile),
            Arc::clone(&self.writer),
            Arc::new(FixedSubjectSettings(Ok(ON))),
            Arc::default(),
            Some(Uuid::new_v4()),
            Arc::clone(&self.reviews),
            Arc::clone(&self.events),
        )
        .with_budget(std::time::Duration::from_secs(5));
        let corrections = settings.map(|settings| {
            let drops = Arc::new(RecordingEvents::default());
            Corrections {
                checks: SensitiveDataChecks::new(Vec::new(), Arc::clone(&drops) as _),
                admission: Admission::new(AdmissionPorts {
                    settings: Arc::new(FixedSubjectSettings(Ok(settings))),
                    rules: Arc::new(FixedRules(Ok(open_rules()))),
                    events: Arc::new(RecordingAdmissionEvents::default()),
                    drops,
                }),
                catalog: Arc::new(PropertyCatalog::load().unwrap()),
            }
        });
        ReviewQueueService::new(
            Arc::new(control),
            Arc::clone(&self.reviews) as _,
            Arc::clone(&self.events) as _,
            corrections,
            PolicyEnforcer::new(resolver),
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
                Some("wrong".to_owned()),
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
}

#[tokio::test]
async fn a_flag_opens_a_request_with_the_flagged_value_and_records_it() {
    let fixture = Fixture::new(0);
    let skill = fixture.skill();

    let request = fixture.flagged().await;

    assert_eq!(request.fact_id, skill);
    assert_eq!(request.state, ReviewState::Open);
    assert_eq!(request.property, "skills");
    assert_eq!(request.value, Some(json!({ "name": "Rust" })));
    assert_eq!(request.comment.as_deref(), Some("wrong"));
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
        matches!(again, Err(DomainError::ReviewConflict(_))),
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
    assert_eq!(open.items, [request]);
}

#[tokio::test]
async fn a_caller_without_the_permission_or_with_a_narrower_scope_is_no_reviewer() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let denied = fixture
        .service_with(Arc::new(DenyResolver), Some(ON))
        .open_requests(&fixture.reviewer(), &ODataQuery::new())
        .await;
    // AllowResolver adds the resource id when one is named; the reviewers'
    // check names none, so a scope with another property is narrower.
    let narrower = fixture
        .service_with(Arc::new(crate::test_support::OwnerOnlyResolver), Some(ON))
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Rejected {
                reason: "fine".to_owned(),
            },
        )
        .await;

    assert!(
        matches!(denied, Err(DomainError::Forbidden(_))),
        "{denied:?}"
    );
    assert!(
        matches!(narrower, Err(DomainError::Forbidden(_))),
        "{narrower:?}"
    );
    assert_eq!(fixture.reviews.all()[0].state, ReviewState::Open);
}

#[tokio::test]
async fn a_rejection_closes_the_request_with_its_reason_and_keeps_the_fact() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;

    let closed = fixture
        .service_with(Arc::new(AllowInOwnTenant), None)
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Rejected {
                reason: "it is right".to_owned(),
            },
        )
        .await
        .unwrap();

    assert_eq!(closed.state, ReviewState::Closed);
    assert_eq!(closed.outcome, Some(ReviewOutcome::Rejected));
    assert_eq!(closed.reason.as_deref(), Some("it is right"));
    assert_eq!(closed.resolved_by, Some(fixture.reviewer));
    assert_eq!(closed.value, None, "the flagged value is not kept");
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .any(|f| f.node_key == request.fact_id)
    );
    assert!(fixture.writer.plans().is_empty(), "nothing is written");
    assert!(matches!(
        fixture.events.taken().last(),
        Some(ReviewQueueEvent::Resolved {
            outcome: ReviewOutcome::Rejected,
            by,
            ..
        }) if *by == fixture.reviewer
    ));
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

    assert_eq!(closed.outcome, Some(ReviewOutcome::Deleted));
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

    assert_eq!(closed.outcome, Some(ReviewOutcome::Deleted));
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

    let new_key = closed.corrected_fact_id.clone().expect("the new fact's id");
    assert_eq!(closed.outcome, Some(ReviewOutcome::Corrected));
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
            outcome: ReviewOutcome::Corrected,
            corrected_fact_id: Some(key),
            ..
        }) if *key == new_key
    ));
}

#[tokio::test]
async fn a_correction_admission_drops_stores_nothing_and_leaves_the_request_open() {
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
    assert_eq!(fixture.reviews.all()[0].state, ReviewState::Open);
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

    assert_eq!(closed.outcome, Some(ReviewOutcome::Deleted));
    assert_eq!(closed.corrected_fact_id, None);
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
        matches!(other_property, Err(DomainError::InvalidValue(_))),
        "{other_property:?}"
    );
    assert!(
        matches!(bad_value, Err(DomainError::InvalidValue(_))),
        "{bad_value:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert_eq!(fixture.reviews.all()[0].state, ReviewState::Open);
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
        .resolve(
            &fixture.reviewer(),
            request.id,
            Resolution::Rejected {
                reason: "fine".to_owned(),
            },
        )
        .await;

    assert!(
        matches!(corrected, Err(DomainError::NotConfigured(_))),
        "{corrected:?}"
    );
    assert_eq!(rejected.unwrap().outcome, Some(ReviewOutcome::Rejected));
}

#[tokio::test]
async fn a_closed_or_unknown_request_cannot_be_resolved() {
    let fixture = Fixture::new(0);
    let request = fixture.flagged().await;
    let service = fixture.service();
    let reject = || Resolution::Rejected {
        reason: "fine".to_owned(),
    };
    service
        .resolve(&fixture.reviewer(), request.id, reject())
        .await
        .unwrap();

    let again = service
        .resolve(&fixture.reviewer(), request.id, reject())
        .await;
    let unknown = service
        .resolve(&fixture.reviewer(), Uuid::new_v4(), reject())
        .await;
    let elsewhere = service
        .resolve(
            &context_of(fixture.reviewer, Uuid::new_v4()),
            request.id,
            reject(),
        )
        .await;

    assert!(
        matches!(again, Err(DomainError::ReviewConflict(_))),
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
