//! Administrator Fact Management over an in-memory profile, writer and
//! review store, with stub policy services.

use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::Ordering;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use serde_json::json;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::admin_facts::{AdminFactsEvent, AdminFactsPorts, AdminFactsService};
use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::error::DomainError;
use crate::domain::one_step::{NotStored, OneStepPath};
use crate::domain::plan::{Change, NodeKey, Origin, Plan};
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::planner::processing::{PlanWriter, Written};
use crate::domain::profile::Category;
use crate::domain::profile_read::OriginKind;
use crate::domain::profile_writes::ProfileWrites;
use crate::domain::record_intake::DropReason;
use crate::domain::review_queue::{
    CLAIM_HOLD, ReviewConflict, ReviewOutcome, ReviewQueueEvent, ReviewQueueService, ReviewRequest,
    ReviewStatus, ReviewText,
};
use crate::domain::sensitive_data::{SensitiveDataCheck, SensitiveDataChecks, SensitiveDataKind};
use crate::domain::subject_settings::{SubjectKey, SubjectSettings};
use crate::infra::processing_contexts::ServiceIdentity;
use crate::test_support::{
    AllowResolver, DenyResolver, FixedRules, FixedSubjectSettings, MemoryProfile, MemoryReviews,
    MemoryWriter, OwnerOnlyResolver, RecordingAdminEvents, RecordingAdmissionEvents,
    RecordingEvents, RecordingResolver, RecordingReviewEvents, SlowResolver, SubstringCheck,
    context_of, open_rules, stored_fact,
};

const ON: SubjectSettings = SubjectSettings {
    personalization_enabled: true,
    erasure_in_progress: false,
};

/// The budget of one write in these tests.
const BUDGET: Duration = Duration::from_secs(5);

const OFF: SubjectSettings = SubjectSettings {
    personalization_enabled: false,
    erasure_in_progress: false,
};

/// The one-step path with Admission reading the subject's settings as
/// `settings`, and the checks `checks`.
fn one_step_path(
    settings: SubjectSettings,
    checks: Vec<Arc<dyn SensitiveDataCheck>>,
) -> Arc<OneStepPath> {
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
}

struct Fixture {
    tenant: Uuid,
    subject: Uuid,
    admin: Uuid,
    profile: Arc<MemoryProfile>,
    writer: Arc<MemoryWriter>,
    reviews: Arc<MemoryReviews>,
    events: Arc<RecordingAdminEvents>,
    review_events: Arc<RecordingReviewEvents>,
}

impl Fixture {
    /// A subject of the tenant with a skill and a headline.
    fn new() -> Self {
        let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
        Self::over(
            tenant,
            subject,
            MemoryProfile::of(
                SubjectKey {
                    tenant_id: tenant,
                    subject_id: subject,
                },
                vec![
                    stored_fact(Category::Skill, "skills", json!({ "name": "Rust" }), 10),
                    stored_fact(Category::Identity, "headline", json!("Engineer"), 20),
                ],
            ),
        )
    }

    fn over(tenant: Uuid, subject: Uuid, profile: Arc<MemoryProfile>) -> Self {
        let writer = MemoryWriter::new(Arc::clone(&profile), 0);
        Self {
            tenant,
            subject,
            admin: Uuid::new_v4(),
            profile,
            writer,
            reviews: Arc::default(),
            events: Arc::default(),
            review_events: Arc::default(),
        }
    }

    /// The service with the policy service `resolver` and a one-step path
    /// whose Admission reads the subject's settings as `settings` and whose
    /// checks are `checks`; none when `settings` is none.
    fn service_over(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        settings: Option<SubjectSettings>,
        checks: Vec<Arc<dyn SensitiveDataCheck>>,
    ) -> AdminFactsService {
        self.service_writing(resolver, settings, checks, Arc::clone(&self.writer) as _)
    }

    /// The service as [`Self::service_over`] builds it, writing through
    /// `writer`.
    fn service_writing(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        settings: Option<SubjectSettings>,
        checks: Vec<Arc<dyn SensitiveDataCheck>>,
        plan_writer: Arc<dyn PlanWriter>,
    ) -> AdminFactsService {
        let writes = Arc::new(
            ProfileWrites::new(
                Arc::clone(&self.profile) as _,
                plan_writer,
                Arc::new(ServiceIdentity::new(Uuid::new_v4())),
                2,
            )
            .with_budget(BUDGET),
        );
        let one_step = settings.map(|settings| one_step_path(settings, checks));
        let enforcer = PolicyEnforcer::new(resolver);
        let review_queue = Arc::new(ReviewQueueService::new(
            Arc::clone(&writes),
            Arc::clone(&self.reviews) as _,
            Arc::clone(&self.review_events) as _,
            one_step.clone(),
            enforcer.clone(),
        ));
        AdminFactsService::new(
            AdminFactsPorts {
                writes,
                reviews: Arc::clone(&self.reviews) as _,
                review_queue,
                one_step,
                events: Arc::clone(&self.events) as _,
            },
            enforcer,
        )
    }

    fn service(&self) -> AdminFactsService {
        self.service_over(Arc::new(AllowResolver), Some(ON), Vec::new())
    }

    fn admin(&self) -> SecurityContext {
        context_of(self.admin, self.tenant)
    }

    fn fact(&self, property: &str) -> NodeKey {
        self.profile
            .live()
            .into_iter()
            .find(|fact| fact.property == property)
            .unwrap()
            .node_key
    }

    /// The subject's skill under an open review request.
    async fn flagged(&self) -> ReviewRequest {
        ReviewQueueService::new(
            Arc::new(ProfileWrites::new(
                Arc::clone(&self.profile) as _,
                Arc::clone(&self.writer) as _,
                Arc::new(ServiceIdentity::new(Uuid::new_v4())),
                2,
            )),
            Arc::clone(&self.reviews) as _,
            Arc::clone(&self.review_events) as _,
            None,
            PolicyEnforcer::new(Arc::new(AllowResolver)),
        )
        .flag(
            &context_of(self.subject, self.tenant),
            self.subject,
            &self.fact("skills"),
            Some(ReviewText::new("wrong").unwrap()),
        )
        .await
        .unwrap()
    }

    /// The only request's status.
    fn status(&self) -> ReviewStatus {
        let [request] = self.reviews.all().try_into().unwrap();
        request.status
    }
}

#[tokio::test]
async fn an_administrator_reads_all_facts_with_their_requests_and_the_read_is_recorded() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;

    let page = fixture
        .service()
        .read(&fixture.admin(), fixture.subject, &ODataQuery::default())
        .await
        .unwrap();

    assert_eq!(page.version, Some(1));
    assert_eq!(page.page.items.len(), 2);
    let skill = page
        .page
        .items
        .iter()
        .find(|item| item.fact.property == "skills")
        .unwrap();
    assert_eq!(
        skill.review_request.as_ref().map(|r| r.id),
        Some(request.id)
    );
    assert_eq!(
        fixture.events.taken(),
        [AdminFactsEvent::Read {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            by: fixture.admin,
            categories: [Category::Identity, Category::Skill].into(),
        }]
    );
}

#[tokio::test]
async fn a_caller_without_the_permission_or_with_a_narrower_scope_is_no_administrator() {
    let fixture = Fixture::new();
    let skill = fixture.fact("skills");

    for resolver in [
        Arc::new(DenyResolver) as Arc<dyn AuthZResolverApi>,
        Arc::new(OwnerOnlyResolver),
    ] {
        let service = fixture.service_over(resolver, Some(ON), Vec::new());
        let admin = fixture.admin();
        let read = service
            .read(&admin, fixture.subject, &ODataQuery::default())
            .await;
        let add = service
            .add(&admin, fixture.subject, "skills", &json!({ "name": "Go" }))
            .await;
        let edit = service
            .edit(
                &admin,
                fixture.subject,
                &skill,
                "skills",
                &json!({ "name": "Go" }),
            )
            .await;
        let delete = service.delete(&admin, fixture.subject, &skill).await;

        assert!(matches!(read, Err(DomainError::Forbidden(_))), "{read:?}");
        assert!(matches!(add, Err(DomainError::Forbidden(_))), "{add:?}");
        assert!(matches!(edit, Err(DomainError::Forbidden(_))), "{edit:?}");
        assert!(
            matches!(delete, Err(DomainError::Forbidden(_))),
            "{delete:?}"
        );
    }
    assert!(fixture.writer.plans().is_empty());
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn each_operation_asks_for_the_permission_to_administer_facts_in_the_callers_tenant() {
    let fixture = Fixture::new();
    let resolver = Arc::new(RecordingResolver::default());
    let service = fixture.service_over(Arc::clone(&resolver) as _, Some(ON), Vec::new());
    let admin = fixture.admin();

    let skill = fixture.fact("skills");
    service
        .read(&admin, fixture.subject, &ODataQuery::default())
        .await
        .unwrap();
    service
        .add(&admin, fixture.subject, "skills", &json!({ "name": "Go" }))
        .await
        .unwrap();
    let edited = service
        .edit(
            &admin,
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Zig" }),
        )
        .await
        .unwrap();
    service
        .delete(&admin, fixture.subject, &edited.node_key)
        .await
        .unwrap();

    let asked = resolver.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 4);
    for (resource, action, _, owner_tenant) in asked {
        assert_eq!(resource, "gts.cf.construct.subject.fact.v1~");
        assert_eq!(action, "administer");
        assert_eq!(owner_tenant, Some(fixture.tenant.to_string()));
    }
}

#[tokio::test]
async fn a_subject_of_another_tenant_answers_as_one_without_a_profile() {
    let fixture = Fixture::new();
    let skill = fixture.fact("skills");
    let service = fixture.service();
    let elsewhere = context_of(fixture.admin, Uuid::new_v4());

    let read = service
        .read(&elsewhere, fixture.subject, &ODataQuery::default())
        .await
        .unwrap();
    let edit = service
        .edit(
            &elsewhere,
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let delete = service.delete(&elsewhere, fixture.subject, &skill).await;

    assert!(read.page.items.is_empty());
    assert_eq!(read.version, None);
    assert!(matches!(edit, Err(DomainError::NoSuchFact(_))), "{edit:?}");
    assert!(
        matches!(delete, Err(DomainError::NoSuchFact(_))),
        "{delete:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert_eq!(fixture.profile.live().len(), 2);
}

#[tokio::test]
async fn an_add_stores_a_new_fact_as_the_administrator() {
    let fixture = Fixture::new();

    let added = fixture
        .service()
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await
        .unwrap();

    assert_eq!(added.category, Category::Skill);
    assert_eq!(added.property.name, "skills");
    assert_eq!(added.property.value, json!({ "name": "Go" }));
    let [plan] = fixture.writer.plans().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Administrator {
            administrator: fixture.admin
        }
    );
    assert_eq!(plan.profile_version, Some(1));
    assert!(
        matches!(&plan.steps[0].change, Change::AddEntity { key, .. } if key == &added.node_key)
    );
    let stored = fixture
        .profile
        .live()
        .into_iter()
        .find(|fact| fact.node_key == added.node_key)
        .unwrap();
    assert_eq!(stored.origin, OriginKind::Administrator);
    assert_eq!(
        fixture.events.taken(),
        [AdminFactsEvent::Added {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            by: fixture.admin,
            fact_id: added.node_key,
            category: Category::Skill,
        }]
    );
}

#[tokio::test]
async fn an_add_for_a_subject_without_a_profile_creates_it() {
    let fixture = Fixture::over(Uuid::new_v4(), Uuid::new_v4(), MemoryProfile::empty());

    let added = fixture
        .service()
        .add(
            &fixture.admin(),
            fixture.subject,
            "headline",
            &json!("Engineer"),
        )
        .await
        .unwrap();

    let [plan] = fixture.writer.plans().try_into().unwrap();
    assert_eq!(plan.profile_version, None);
    let version = *fixture.profile.version.lock().unwrap();
    assert_eq!(version, Some(1));
    assert_eq!(fixture.profile.live()[0].node_key, added.node_key);
}

#[tokio::test]
async fn an_add_of_a_held_value_or_a_second_one_valued_value_names_the_held_fact() {
    let fixture = Fixture::new();
    let service = fixture.service();

    let same = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "Rust" }),
        )
        .await;
    let second = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "headline",
            &json!("Manager"),
        )
        .await;

    assert!(
        matches!(&same, Err(DomainError::FactHeld(key)) if key == &fixture.fact("skills")),
        "{same:?}"
    );
    assert!(
        matches!(&second, Err(DomainError::FactHeld(key)) if key == &fixture.fact("headline")),
        "{second:?}"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn an_unknown_property_or_a_value_its_schema_does_not_take_is_invalid() {
    let fixture = Fixture::new();
    let service = fixture.service();

    let unknown = service
        .add(&fixture.admin(), fixture.subject, "shoe_size", &json!(42))
        .await;
    let misfit = service
        .add(&fixture.admin(), fixture.subject, "skills", &json!(42))
        .await;

    assert!(
        matches!(&unknown, Err(DomainError::InvalidValue { field: "property", rule }) if !rule.contains("shoe_size")),
        "{unknown:?}"
    );
    assert!(
        matches!(
            misfit,
            Err(DomainError::InvalidValue { field: "value", .. })
        ),
        "{misfit:?}"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn an_add_or_edit_while_personalization_is_off_stores_nothing_and_says_why() {
    let fixture = Fixture::new();
    let service = fixture.service_over(Arc::new(AllowResolver), Some(OFF), Vec::new());
    let skill = fixture.fact("skills");

    let add = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let edit = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;

    let off = NotStored::Dropped(DropReason::PersonalizationOff);
    assert!(
        matches!(add, Err(DomainError::NotStored(reason)) if reason == off),
        "{add:?}"
    );
    assert!(
        matches!(edit, Err(DomainError::NotStored(reason)) if reason == off),
        "{edit:?}"
    );
    assert!(fixture.writer.plans().is_empty());
    assert_eq!(
        fixture.events.taken(),
        [
            AdminFactsEvent::Refused {
                tenant_id: fixture.tenant,
                subject_id: fixture.subject,
                by: fixture.admin,
                fact_id: None,
                reason: off,
            },
            AdminFactsEvent::Refused {
                tenant_id: fixture.tenant,
                subject_id: fixture.subject,
                by: fixture.admin,
                fact_id: Some(skill),
                reason: off,
            },
        ]
    );
}

#[tokio::test]
async fn a_blocked_value_stores_nothing() {
    let fixture = Fixture::new();
    let service = fixture.service_over(
        Arc::new(AllowResolver),
        Some(ON),
        vec![Arc::new(SubstringCheck {
            kind: SensitiveDataKind::Credentials,
            needle: "secret",
        })],
    );

    let added = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "secret" }),
        )
        .await;

    assert!(
        matches!(added, Err(DomainError::NotStored(NotStored::Blocked))),
        "{added:?}"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn without_the_opt_in_an_add_or_edit_is_not_configured_and_read_and_delete_work() {
    let fixture = Fixture::new();
    let service = fixture.service_over(Arc::new(AllowResolver), None, Vec::new());
    let skill = fixture.fact("skills");

    let add = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let edit = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let read = service
        .read(&fixture.admin(), fixture.subject, &ODataQuery::default())
        .await;
    let delete = service
        .delete(&fixture.admin(), fixture.subject, &skill)
        .await;

    assert!(matches!(add, Err(DomainError::NotConfigured(_))), "{add:?}");
    assert!(
        matches!(edit, Err(DomainError::NotConfigured(_))),
        "{edit:?}"
    );
    assert!(read.is_ok(), "{read:?}");
    assert!(delete.is_ok(), "{delete:?}");
}

#[tokio::test]
async fn an_edit_replaces_the_fact_under_a_new_key_as_the_administrator() {
    let fixture = Fixture::new();
    let skill = fixture.fact("skills");

    let edited = fixture
        .service()
        .edit(
            &fixture.admin(),
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await
        .unwrap();

    assert_ne!(edited.node_key, skill);
    let [plan] = fixture.writer.plans().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Administrator {
            administrator: fixture.admin
        }
    );
    assert!(matches!(
        &plan.steps[0].change,
        Change::ReplaceEntity { old, key, .. } if old == &skill && key == &edited.node_key
    ));
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|fact| fact.node_key != skill)
    );
    assert_eq!(
        fixture.events.taken(),
        [AdminFactsEvent::Edited {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            by: fixture.admin,
            fact_id: skill,
            new_fact_id: edited.node_key,
            category: Category::Skill,
        }]
    );
}

#[tokio::test]
async fn an_edit_of_another_property_or_a_missing_fact_stores_nothing() {
    let fixture = Fixture::new();
    let service = fixture.service();

    let other = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &fixture.fact("skills"),
            "headline",
            &json!("Manager"),
        )
        .await;
    let missing = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &NodeKey("construct:t:nothing".to_owned()),
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;

    assert!(
        matches!(
            other,
            Err(DomainError::InvalidValue {
                field: "property",
                ..
            })
        ),
        "{other:?}"
    );
    assert!(
        matches!(missing, Err(DomainError::NoSuchFact(_))),
        "{missing:?}"
    );
    assert!(fixture.writer.plans().is_empty());
}

#[tokio::test]
async fn an_edit_of_a_fact_under_review_closes_the_request_as_corrected_by_the_administrator() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;

    let edited = fixture
        .service()
        .edit(
            &fixture.admin(),
            fixture.subject,
            &request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await
        .unwrap();

    let ReviewStatus::Closed(closed) = fixture.status() else {
        panic!("still open");
    };
    assert_eq!(
        closed.outcome,
        ReviewOutcome::Corrected(edited.node_key.clone())
    );
    assert_eq!(closed.by, fixture.admin);
    let [plan] = fixture.writer.plans().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Administrator {
            administrator: fixture.admin
        }
    );
    assert!(fixture.review_events.taken().iter().any(|event| matches!(
        event,
        ReviewQueueEvent::Resolved { by, .. } if *by == fixture.admin
    )));
    assert!(fixture.events.taken().iter().any(|event| matches!(
        event,
        AdminFactsEvent::Edited { new_fact_id, .. } if new_fact_id == &edited.node_key
    )));
}

#[tokio::test]
async fn a_refused_edit_of_a_fact_under_review_leaves_the_request_open() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;
    let service = fixture.service_over(Arc::new(AllowResolver), Some(OFF), Vec::new());

    let edit = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;

    assert!(matches!(edit, Err(DomainError::NotStored(_))), "{edit:?}");
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
    assert!(fixture.writer.plans().is_empty());
    assert!(fixture.events.taken().iter().any(|event| matches!(
        event,
        AdminFactsEvent::Refused { fact_id: Some(fact), .. } if fact == &request.fact_id
    )));
}

#[tokio::test]
async fn an_edit_or_delete_while_a_reviewer_holds_the_request_is_a_conflict() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;
    fixture.reviews.hold_first(
        time::OffsetDateTime::now_utc() - CLAIM_HOLD + std::time::Duration::from_secs(30),
    );
    let service = fixture.service();

    let edit = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let delete = service
        .delete(&fixture.admin(), fixture.subject, &request.fact_id)
        .await;

    for answer in [edit.map(|_| ()), delete] {
        assert!(
            matches!(
                answer,
                Err(DomainError::ReviewConflict(ReviewConflict::Held(id))) if id == request.id
            ),
            "{answer:?}"
        );
    }
    assert!(fixture.writer.plans().is_empty());
    assert!(matches!(fixture.status(), ReviewStatus::Open { .. }));
}

#[tokio::test]
async fn a_delete_removes_the_fact_as_the_administrator_also_while_personalization_is_off() {
    let fixture = Fixture::new();
    let skill = fixture.fact("skills");
    let service = fixture.service_over(Arc::new(AllowResolver), Some(OFF), Vec::new());

    service
        .delete(&fixture.admin(), fixture.subject, &skill)
        .await
        .unwrap();
    let again = service
        .delete(&fixture.admin(), fixture.subject, &skill)
        .await;

    let [plan] = fixture.writer.plans().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Administrator {
            administrator: fixture.admin
        }
    );
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|fact| fact.node_key != skill)
    );
    assert!(
        matches!(again, Err(DomainError::NoSuchFact(_))),
        "{again:?}"
    );
    assert_eq!(
        fixture.events.taken(),
        [AdminFactsEvent::Deleted {
            tenant_id: fixture.tenant,
            subject_id: fixture.subject,
            by: fixture.admin,
            fact_id: skill,
            category: Category::Skill,
        }]
    );
}

#[tokio::test]
async fn a_delete_of_a_fact_under_review_closes_the_request_as_deleted_by_the_administrator() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;

    fixture
        .service()
        .delete(&fixture.admin(), fixture.subject, &request.fact_id)
        .await
        .unwrap();

    let ReviewStatus::Closed(closed) = fixture.status() else {
        panic!("still open");
    };
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    assert_eq!(closed.by, fixture.admin);
    assert!(
        fixture
            .profile
            .live()
            .iter()
            .all(|fact| fact.node_key != request.fact_id)
    );
    assert!(fixture.events.taken().iter().any(|event| matches!(
        event,
        AdminFactsEvent::Deleted { fact_id, .. } if fact_id == &request.fact_id
    )));
}

#[tokio::test]
async fn a_delete_of_a_fact_already_gone_closes_its_open_request() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;
    fixture
        .profile
        .facts
        .lock()
        .unwrap()
        .retain(|fact| fact.node_key != request.fact_id);

    fixture
        .service()
        .delete(&fixture.admin(), fixture.subject, &request.fact_id)
        .await
        .unwrap();

    let ReviewStatus::Closed(closed) = fixture.status() else {
        panic!("still open");
    };
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    assert_eq!(closed.by, fixture.admin);
    assert!(fixture.writer.plans().is_empty());
    assert!(
        fixture.events.taken().is_empty(),
        "nothing was deleted by the administrator"
    );
}

/// A writer that fails every write.
struct FailingWriter;

#[async_trait]
impl PlanWriter for FailingWriter {
    async fn write(&self, _: &SecurityContext, _: &Plan) -> Result<Written, DomainError> {
        Err(DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

/// A writer whose first write conflicts because someone stored the plan's
/// own value just before, as another administrator adding it would.
struct AddedMeanwhile {
    inner: Arc<MemoryWriter>,
    profile: Arc<MemoryProfile>,
    raced: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl PlanWriter for AddedMeanwhile {
    async fn write(&self, ctx: &SecurityContext, plan: &Plan) -> Result<Written, DomainError> {
        if !self.raced.swap(true, Ordering::SeqCst) {
            let Change::AddEntity { properties, .. } = &plan.steps[0].change else {
                panic!("not an add: {plan:?}");
            };
            let property = &properties[0];
            self.profile.facts.lock().unwrap().push(stored_fact(
                Category::Skill,
                &property.name,
                property.value.clone(),
                30,
            ));
            let mut version = self.profile.version.lock().unwrap();
            *version = Some(version.unwrap_or(0) + 1);
            return Ok(Written::Conflict);
        }
        self.inner.write(ctx, plan).await
    }
}

#[tokio::test]
async fn a_writer_failure_is_unavailable_and_records_nothing() {
    let fixture = Fixture::new();
    let skill = fixture.fact("skills");
    let service = fixture.service_writing(
        Arc::new(AllowResolver),
        Some(ON),
        Vec::new(),
        Arc::new(FailingWriter),
    );
    let admin = fixture.admin();

    let add = service
        .add(&admin, fixture.subject, "skills", &json!({ "name": "Go" }))
        .await;
    let edit = service
        .edit(
            &admin,
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let delete = service.delete(&admin, fixture.subject, &skill).await;

    for answer in [add.map(|_| ()), edit.map(|_| ()), delete] {
        assert!(
            matches!(&answer, Err(DomainError::Unavailable(why)) if why.contains("may have landed")),
            "{answer:?}"
        );
    }
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn an_add_after_a_conflict_is_built_again_on_the_new_version() {
    let fixture = Fixture::new();
    let writer = MemoryWriter::new(Arc::clone(&fixture.profile), 1);
    let service = fixture.service_writing(
        Arc::new(AllowResolver),
        Some(ON),
        Vec::new(),
        Arc::clone(&writer) as _,
    );

    service
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await
        .unwrap();

    let [first, second] = writer.plans().try_into().unwrap();
    assert_eq!(first.profile_version, Some(1));
    assert_eq!(second.profile_version, Some(2));
}

#[tokio::test]
async fn an_add_whose_value_lands_meanwhile_is_held_on_the_new_version() {
    let fixture = Fixture::new();
    let writer = Arc::new(AddedMeanwhile {
        inner: MemoryWriter::new(Arc::clone(&fixture.profile), 0),
        profile: Arc::clone(&fixture.profile),
        raced: std::sync::atomic::AtomicBool::new(false),
    });
    let service = fixture.service_writing(Arc::new(AllowResolver), Some(ON), Vec::new(), writer);

    let added = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;

    assert!(matches!(added, Err(DomainError::FactHeld(_))), "{added:?}");
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test]
async fn an_edit_whose_fact_goes_away_before_the_write_stores_nothing() {
    let fixture = Fixture::new();
    let skill = fixture.fact("skills");
    let writer = MemoryWriter::racing(Arc::clone(&fixture.profile));
    let service = fixture.service_writing(
        Arc::new(AllowResolver),
        Some(ON),
        Vec::new(),
        Arc::clone(&writer) as _,
    );

    let edit = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;

    assert!(matches!(edit, Err(DomainError::NoSuchFact(_))), "{edit:?}");
    assert_eq!(writer.plans().len(), 1, "the rerun found the fact gone");
    assert!(fixture.events.taken().is_empty());
}

/// The fixture's skill with a reviewer's correction to "Go" in the profile
/// whose request did not close, its claim lapsed: what the next resolution
/// settles.
async fn corrected_but_not_closed(fixture: &Fixture) -> (ReviewRequest, NodeKey) {
    let request = fixture.flagged().await;
    fixture.reviews.close_fails.store(true, Ordering::SeqCst);
    let unclosed = ReviewQueueService::new(
        Arc::new(ProfileWrites::new(
            Arc::clone(&fixture.profile) as _,
            Arc::clone(&fixture.writer) as _,
            Arc::new(ServiceIdentity::new(Uuid::new_v4())),
            2,
        )),
        Arc::clone(&fixture.reviews) as _,
        Arc::clone(&fixture.review_events) as _,
        Some(one_step_path(ON, Vec::new())),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    )
    .resolve(
        &context_of(Uuid::new_v4(), fixture.tenant),
        request.id,
        crate::domain::review_queue::Resolution::Corrected {
            property: "skills".to_owned(),
            value: json!({ "name": "Go" }),
            reason: None,
        },
    )
    .await;
    let Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
        outcome: ReviewOutcome::Corrected(new_key),
        ..
    })) = unclosed
    else {
        panic!("{unclosed:?}");
    };
    fixture.reviews.close_fails.store(false, Ordering::SeqCst);
    fixture
        .reviews
        .claimed_first_at(time::OffsetDateTime::now_utc() - CLAIM_HOLD);
    (request, new_key)
}

#[tokio::test]
async fn an_edit_or_delete_of_a_fact_an_earlier_resolution_replaced_writes_nothing() {
    let edit_fixture = Fixture::new();
    let (edit_request, _) = corrected_but_not_closed(&edit_fixture).await;
    let delete_fixture = Fixture::new();
    let (delete_request, corrected) = corrected_but_not_closed(&delete_fixture).await;

    let edit = edit_fixture
        .service()
        .edit(
            &edit_fixture.admin(),
            edit_fixture.subject,
            &edit_request.fact_id,
            "skills",
            &json!({ "name": "Zig" }),
        )
        .await;
    let delete = delete_fixture
        .service()
        .delete(
            &delete_fixture.admin(),
            delete_fixture.subject,
            &delete_request.fact_id,
        )
        .await;

    // The edit reads the fact first and finds it replaced; the delete
    // settles the request as the correction in the profile.
    assert!(matches!(edit, Err(DomainError::NoSuchFact(_))), "{edit:?}");
    assert!(
        matches!(delete, Err(DomainError::ReviewConflict(ReviewConflict::Closed(id))) if id == delete_request.id),
        "{delete:?}"
    );
    for fixture in [&edit_fixture, &delete_fixture] {
        assert_eq!(fixture.writer.plans().len(), 1, "only the reviewer's write");
        assert!(fixture.events.taken().is_empty());
    }
    assert!(
        delete_fixture
            .profile
            .live()
            .iter()
            .any(|fact| fact.node_key == corrected)
    );
}

#[tokio::test]
async fn an_edit_or_delete_stored_but_not_closed_is_recorded_and_says_so() {
    let edit_fixture = Fixture::new();
    let edit_request = edit_fixture.flagged().await;
    edit_fixture
        .reviews
        .close_fails
        .store(true, Ordering::SeqCst);
    let delete_fixture = Fixture::new();
    let delete_request = delete_fixture.flagged().await;
    delete_fixture
        .reviews
        .close_fails
        .store(true, Ordering::SeqCst);

    let edit = edit_fixture
        .service()
        .edit(
            &edit_fixture.admin(),
            edit_fixture.subject,
            &edit_request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    let delete = delete_fixture
        .service()
        .delete(
            &delete_fixture.admin(),
            delete_fixture.subject,
            &delete_request.fact_id,
        )
        .await;

    for answer in [edit.map(|_| ()), delete] {
        assert!(
            matches!(
                answer,
                Err(DomainError::ReviewConflict(
                    ReviewConflict::NotClosed { .. }
                ))
            ),
            "{answer:?}"
        );
    }
    assert!(matches!(
        edit_fixture.events.taken().as_slice(),
        [AdminFactsEvent::Edited { fact_id, .. }] if fact_id == &edit_request.fact_id
    ));
    assert!(matches!(
        delete_fixture.events.taken().as_slice(),
        [AdminFactsEvent::Deleted { fact_id, .. }] if fact_id == &delete_request.fact_id
    ));
}

#[tokio::test]
async fn a_delete_closes_a_request_flagged_after_its_check_and_a_failed_close_is_repeated() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;
    fixture.reviews.hide_next.store(1, Ordering::SeqCst);
    fixture
        .reviews
        .close_open_fails
        .store(true, Ordering::SeqCst);
    let service = fixture.service();

    let failed = service
        .delete(&fixture.admin(), fixture.subject, &request.fact_id)
        .await;
    fixture
        .reviews
        .close_open_fails
        .store(false, Ordering::SeqCst);
    service
        .delete(&fixture.admin(), fixture.subject, &request.fact_id)
        .await
        .unwrap();

    assert!(
        matches!(&failed, Err(DomainError::Unavailable(why)) if why.contains("send the delete again")),
        "{failed:?}"
    );
    let ReviewStatus::Closed(closed) = fixture.status() else {
        panic!("still open");
    };
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    assert_eq!(closed.by, fixture.admin);
    assert_eq!(fixture.events.taken().len(), 1, "one delete, recorded once");
}

#[tokio::test]
async fn an_edit_closes_a_request_flagged_after_its_check_unless_a_reviewer_holds_it() {
    let free = Fixture::new();
    let free_request = free.flagged().await;
    let held = Fixture::new();
    let held_request = held.flagged().await;
    held.reviews.hold_first(
        time::OffsetDateTime::now_utc() - CLAIM_HOLD + std::time::Duration::from_secs(30),
    );
    // The edit's own check sees no request; the close after it finds it.
    for fixture in [&free, &held] {
        fixture.reviews.hide_next.store(1, Ordering::SeqCst);
    }

    let freed = free
        .service()
        .edit(
            &free.admin(),
            free.subject,
            &free_request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await
        .unwrap();
    held.service()
        .edit(
            &held.admin(),
            held.subject,
            &held_request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await
        .unwrap();

    let ReviewStatus::Closed(closed) = free.status() else {
        panic!("still open");
    };
    assert_eq!(closed.outcome, ReviewOutcome::Corrected(freed.node_key));
    assert_eq!(closed.by, free.admin);
    assert!(
        matches!(held.status(), ReviewStatus::Open { .. }),
        "left to the reviewer who holds it"
    );
}

#[tokio::test(start_paused = true)]
async fn a_policy_service_that_outlasts_the_budget_cuts_off_an_add_edit_or_delete() {
    let fixture = Fixture::new();
    let service = fixture.service_over(Arc::new(SlowResolver(BUDGET * 2)), Some(ON), Vec::new());
    let skill = fixture.fact("skills");

    let started = tokio::time::Instant::now();
    let added = service
        .add(
            &fixture.admin(),
            fixture.subject,
            "headline",
            &json!("Lead"),
        )
        .await;
    assert!(
        matches!(added, Err(DomainError::Unavailable(_))),
        "{added:?}"
    );
    assert_eq!(
        started.elapsed(),
        BUDGET,
        "the add answered with the budget"
    );

    let started = tokio::time::Instant::now();
    let edited = service
        .edit(
            &fixture.admin(),
            fixture.subject,
            &skill,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;
    assert!(
        matches!(edited, Err(DomainError::Unavailable(_))),
        "{edited:?}"
    );
    assert_eq!(
        started.elapsed(),
        BUDGET,
        "the edit answered with the budget"
    );

    let started = tokio::time::Instant::now();
    let deleted = service
        .delete(&fixture.admin(), fixture.subject, &skill)
        .await;
    assert!(
        matches!(deleted, Err(DomainError::Unavailable(_))),
        "{deleted:?}"
    );
    assert_eq!(
        started.elapsed(),
        BUDGET,
        "the delete answered with the budget"
    );

    assert!(fixture.profile.reads.lock().unwrap().is_empty());
    assert!(fixture.writer.plans().is_empty());
    assert_eq!(fixture.profile.live().len(), 2);
    assert!(fixture.events.taken().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_edit_of_a_fact_under_review_resolves_it_within_the_edits_budget() {
    let fixture = Fixture::new();
    let request = fixture.flagged().await;
    // The edit's own read ends at 3 s; the resolution's read would end at
    // 6 s, past the edit's budget though within a budget of its own.
    *fixture.profile.delay.lock().unwrap() = Duration::from_secs(3);
    let started = tokio::time::Instant::now();

    let edited = fixture
        .service()
        .edit(
            &fixture.admin(),
            fixture.subject,
            &request.fact_id,
            "skills",
            &json!({ "name": "Go" }),
        )
        .await;

    assert!(
        matches!(edited, Err(DomainError::Unavailable(_))),
        "{edited:?}"
    );
    assert_eq!(started.elapsed(), BUDGET);
    assert!(fixture.writer.plans().is_empty());
    assert!(
        matches!(fixture.status(), ReviewStatus::Open { .. }),
        "the claim was given back"
    );
    assert!(fixture.events.taken().is_empty());
}
