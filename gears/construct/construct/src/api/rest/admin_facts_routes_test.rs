//! Router-level tests for the administrator's fact routes. The security
//! context the gateway injects in production is injected with an `Extension`
//! layer.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_admin_facts_routes;
use crate::domain::admin_facts::{AdminFactsPorts, AdminFactsService};
use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::one_step::OneStepPath;
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::profile::Category;
use crate::domain::review_queue::{CLAIM_HOLD, ReviewQueueService, ReviewText};
use crate::domain::sensitive_data::SensitiveDataChecks;
use crate::domain::subject_settings::{SubjectKey, SubjectSettings};
use crate::test_support::{
    AllowResolver, DenyResolver, FixedRules, FixedSubjectSettings, MemoryProfile, MemoryReviews,
    MemoryWriter, RecordingAdmissionEvents, RecordingEvents, SECURITY_HEADERS, context_of,
    open_rules, profile_writes, stored_fact,
};

const ON: SubjectSettings = SubjectSettings {
    personalization_enabled: true,
    erasure_in_progress: false,
};

const OFF: SubjectSettings = SubjectSettings {
    personalization_enabled: false,
    erasure_in_progress: false,
};

struct Tenant {
    id: Uuid,
    subject: Uuid,
    profile: Arc<MemoryProfile>,
    reviews: Arc<MemoryReviews>,
}

impl Tenant {
    /// A tenant whose one subject has a skill; any other subject or tenant
    /// reads as one without a profile.
    fn new() -> Self {
        let (id, subject) = (Uuid::new_v4(), Uuid::new_v4());
        Self {
            id,
            subject,
            profile: MemoryProfile::of(
                SubjectKey {
                    tenant_id: id,
                    subject_id: subject,
                },
                vec![stored_fact(
                    Category::Skill,
                    "skills",
                    json!({ "name": "Rust" }),
                    10,
                )],
            ),
            reviews: Arc::default(),
        }
    }

    /// The routes as an administrator of the tenant sees them, with a
    /// one-step path whose Admission reads the settings `settings`.
    fn routes(&self, resolver: Arc<dyn AuthZResolverApi>, settings: SubjectSettings) -> Router {
        let writes = Arc::new(profile_writes(
            Arc::clone(&self.profile),
            MemoryWriter::new(Arc::clone(&self.profile), 0),
            Some(Uuid::new_v4()),
        ));
        let drops = Arc::new(RecordingEvents::default());
        let one_step = Arc::new(OneStepPath {
            checks: SensitiveDataChecks::new(Vec::new(), Arc::clone(&drops) as _),
            admission: Admission::new(AdmissionPorts {
                settings: Arc::new(FixedSubjectSettings(Ok(settings))),
                rules: Arc::new(FixedRules(Ok(open_rules()))),
                events: Arc::new(RecordingAdmissionEvents::default()),
                drops,
            }),
            catalog: Arc::new(PropertyCatalog::load().unwrap()),
        });
        let enforcer = PolicyEnforcer::new(resolver);
        let review_queue = Arc::new(ReviewQueueService::new(
            Arc::clone(&writes),
            Arc::clone(&self.reviews) as _,
            Arc::default() as Arc<crate::test_support::RecordingReviewEvents>,
            Some(Arc::clone(&one_step)),
            enforcer.clone(),
        ));
        let service = AdminFactsService::new(
            AdminFactsPorts {
                writes,
                review_queue,
                one_step: Some(one_step),
                events: Arc::default() as Arc<crate::test_support::RecordingAdminEvents>,
            },
            enforcer,
        );
        register_admin_facts_routes(
            Router::new(),
            &OpenApiRegistryImpl::new(),
            Arc::new(service),
        )
        .layer(Extension(context_of(Uuid::new_v4(), self.id)))
    }

    fn admin(&self) -> Router {
        self.routes(Arc::new(AllowResolver), ON)
    }

    fn skill(&self) -> String {
        self.profile.live()[0].node_key.0.clone()
    }

    fn facts(&self) -> String {
        format!("/construct/v1/admin/subjects/{}/facts", self.subject)
    }

    fn fact(&self, fact_id: &str) -> String {
        format!("{}/{fact_id}", self.facts())
    }

    /// The skill under an open review request, flagged by the subject.
    async fn flagged(&self) -> Uuid {
        ReviewQueueService::new(
            Arc::new(profile_writes(
                Arc::clone(&self.profile),
                MemoryWriter::new(Arc::clone(&self.profile), 0),
                Some(Uuid::new_v4()),
            )),
            Arc::clone(&self.reviews) as _,
            Arc::default() as Arc<crate::test_support::RecordingReviewEvents>,
            None,
            PolicyEnforcer::new(Arc::new(AllowResolver)),
        )
        .flag(
            &context_of(self.subject, self.id),
            self.subject,
            &self.profile.live()[0].node_key,
            Some(ReviewText::new("wrong").unwrap()),
        )
        .await
        .unwrap()
        .id
    }
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn send(router: Router, method: Method, uri: &str, body: Option<Value>) -> Answer {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    };
    let response = router.oneshot(request.unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Answer {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

#[tokio::test]
async fn an_administrator_lists_adds_edits_and_deletes_a_subjects_facts() {
    let tenant = Tenant::new();
    let skill = tenant.skill();

    let listed = send(tenant.admin(), Method::GET, &tenant.facts(), None).await;
    let added = send(
        tenant.admin(),
        Method::POST,
        &tenant.facts(),
        Some(json!({ "property": "headline", "value": "Engineer" })),
    )
    .await;
    let edited = send(
        tenant.admin(),
        Method::PUT,
        &tenant.fact(&skill),
        Some(json!({ "property": "skills", "value": { "name": "Go" } })),
    )
    .await;
    let new_skill = edited.body["fact_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let deleted = send(
        tenant.admin(),
        Method::DELETE,
        &tenant.fact(&new_skill),
        None,
    )
    .await;
    let after = send(tenant.admin(), Method::GET, &tenant.facts(), None).await;

    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"][0]["fact_id"], json!(skill));
    assert_eq!(listed.body["items"][0]["entity_kind"], json!("skill"));
    assert_eq!(listed.body["items"][0]["review_request"], Value::Null);
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    assert_eq!(added.body["entity_kind"], json!("identity"));
    assert_eq!(added.body["property"], json!("headline"));
    assert_eq!(added.body["value"], json!("Engineer"));
    assert_eq!(added.body["origin"]["kind"], json!("administrator"));
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    assert_ne!(new_skill, skill);
    assert_eq!(edited.body["property"], json!("skills"));
    assert_eq!(edited.body["value"], json!({ "name": "Go" }));
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(after.body["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(after.body["items"][0]["fact_id"], added.body["fact_id"]);
}

#[tokio::test]
async fn a_caller_without_the_permission_gets_403_and_a_fact_the_subject_lacks_404() {
    let tenant = Tenant::new();
    let denied = || tenant.routes(Arc::new(DenyResolver), ON);

    let read = send(denied(), Method::GET, &tenant.facts(), None).await;
    let add = send(
        denied(),
        Method::POST,
        &tenant.facts(),
        Some(json!({ "property": "headline", "value": "Engineer" })),
    )
    .await;
    let unknown = send(
        tenant.admin(),
        Method::DELETE,
        &tenant.fact("construct:t:nothing"),
        None,
    )
    .await;
    let malformed = send(
        tenant.admin(),
        Method::PUT,
        &tenant.fact("not%20a%20key"),
        Some(json!({ "property": "skills", "value": { "name": "Go" } })),
    )
    .await;

    assert_eq!(read.status, StatusCode::FORBIDDEN, "{}", read.body);
    assert_eq!(add.status, StatusCode::FORBIDDEN, "{}", add.body);
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    assert_eq!(
        malformed.status,
        StatusCode::NOT_FOUND,
        "{}",
        malformed.body
    );
    assert_eq!(tenant.profile.live().len(), 1);
}

#[tokio::test]
async fn a_refused_value_is_422_naming_the_reason_and_a_held_one_409() {
    let tenant = Tenant::new();

    let refused = send(
        tenant.routes(Arc::new(AllowResolver), OFF),
        Method::POST,
        &tenant.facts(),
        Some(json!({ "property": "headline", "value": "Engineer" })),
    )
    .await;
    let held = send(
        tenant.admin(),
        Method::POST,
        &tenant.facts(),
        Some(json!({ "property": "skills", "value": { "name": "Rust" } })),
    )
    .await;

    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.body
    );
    let violation = &refused.body["context"]["violations"][0];
    assert_eq!(
        violation["type"],
        json!("personalization_off"),
        "{}",
        refused.body
    );
    assert_eq!(violation["subject"], json!("value"));
    assert_eq!(held.status, StatusCode::CONFLICT, "{}", held.body);
    assert_eq!(held.body["context"]["reason"], json!("ALREADY_HELD"));
    assert!(
        held.body.to_string().contains(&tenant.skill()),
        "names the fact that holds it: {}",
        held.body
    );
    assert_eq!(tenant.profile.live().len(), 1);
}

#[tokio::test]
async fn an_unknown_property_or_a_value_its_schema_does_not_take_is_400_naming_the_field() {
    let tenant = Tenant::new();

    for (body, field) in [
        (json!({ "property": "shoe_size", "value": 42 }), "property"),
        (
            json!({ "property": "Skills", "value": { "name": "Go" } }),
            "property",
        ),
        (
            json!({ "property": "x".repeat(65), "value": 42 }),
            "property",
        ),
        (json!({ "property": "", "value": 42 }), "property"),
        (json!({ "property": "skills", "value": 42 }), "value"),
    ] {
        let answer = send(
            tenant.admin(),
            Method::POST,
            &tenant.facts(),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            answer.status,
            StatusCode::BAD_REQUEST,
            "{body}: {}",
            answer.body
        );
        assert_eq!(
            answer.body["context"]["field_violations"][0]["field"],
            json!(field),
            "{body}: {}",
            answer.body
        );
    }
    assert_eq!(tenant.profile.live().len(), 1);
}

#[tokio::test]
async fn a_body_without_a_property_or_a_value_or_with_another_field_stores_nothing() {
    let tenant = Tenant::new();

    for body in [
        json!({ "value": 42 }),
        json!({ "property": "headline" }),
        json!({ "properties": { "headline": "Engineer" } }),
        json!({ "property": "headline", "value": "Engineer", "category": "identity" }),
    ] {
        let answer = send(
            tenant.admin(),
            Method::POST,
            &tenant.facts(),
            Some(body.clone()),
        )
        .await;
        assert!(
            answer.status.is_client_error(),
            "{body}: {} {}",
            answer.status,
            answer.body
        );
    }
    assert_eq!(tenant.profile.live().len(), 1);
}

#[tokio::test]
async fn an_edit_closes_the_facts_review_request_and_while_a_reviewer_holds_it_is_409() {
    let held = Tenant::new();
    held.flagged().await;
    held.reviews.hold_first(
        time::OffsetDateTime::now_utc() - CLAIM_HOLD + std::time::Duration::from_secs(30),
    );
    let free = Tenant::new();
    free.flagged().await;
    let edit = |tenant: &Tenant| {
        let router = tenant.admin();
        let uri = tenant.fact(&tenant.skill());
        async move {
            send(
                router,
                Method::PUT,
                &uri,
                Some(json!({ "property": "skills", "value": { "name": "Go" } })),
            )
            .await
        }
    };

    let refused = edit(&held).await;
    let edited = edit(&free).await;

    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    assert_eq!(refused.body["context"]["reason"], json!("BEING_RESOLVED"));
    assert!(held.reviews.all()[0].is_open());
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    assert!(!free.reviews.all()[0].is_open());
}

#[tokio::test]
async fn every_answer_is_no_store_with_the_security_headers() {
    let tenant = Tenant::new();

    for (router, method, uri) in [
        (tenant.admin(), Method::GET, tenant.facts()),
        (
            tenant.routes(Arc::new(DenyResolver), ON),
            Method::DELETE,
            tenant.fact(&tenant.skill()),
        ),
    ] {
        let answer = send(router, method.clone(), &uri, None).await;
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(
                answer.headers.get(name).map(|v| v.to_str().unwrap()),
                Some(value),
                "{method} {uri}: {name}"
            );
        }
    }
}
