//! Router-level tests for the Review Queue's routes. The security context the
//! gateway injects in production is injected with an `Extension` layer.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_review_queue_routes;
use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::profile::Category;
use crate::domain::review_queue::{CLAIM_HOLD, Corrections, ReviewQueueService};
use crate::domain::sensitive_data::SensitiveDataChecks;
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    AllowResolver, DenyResolver, FixedRules, FixedSubjectSettings, MemoryProfile, MemoryReviews,
    MemoryWriter, RecordingAdmissionEvents, RecordingEvents, SECURITY_HEADERS, context_of,
    open_rules, profile_writes, stored_fact,
};

const ON: SubjectSettings = SubjectSettings {
    personalization_enabled: true,
    erasure_in_progress: false,
};

struct Tenant {
    id: Uuid,
    subject: Uuid,
    profile: Arc<MemoryProfile>,
    reviews: Arc<MemoryReviews>,
}

impl Tenant {
    fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            subject: Uuid::new_v4(),
            profile: MemoryProfile::with(vec![stored_fact(
                Category::Skill,
                "skills",
                json!({ "name": "Rust" }),
                10,
            )]),
            reviews: Arc::default(),
        }
    }

    /// The routes as `caller` of the tenant sees them, with corrections
    /// whose Admission reads the settings `settings`.
    fn routes(
        &self,
        resolver: Arc<dyn AuthZResolverApi>,
        caller: Uuid,
        settings: SubjectSettings,
    ) -> Router {
        let writes = profile_writes(
            Arc::clone(&self.profile),
            MemoryWriter::new(Arc::clone(&self.profile), 0),
            Some(Uuid::new_v4()),
        );
        let drops = Arc::new(RecordingEvents::default());
        let corrections = Corrections {
            checks: SensitiveDataChecks::new(Vec::new(), Arc::clone(&drops) as _),
            admission: Admission::new(AdmissionPorts {
                settings: Arc::new(FixedSubjectSettings(Ok(settings))),
                rules: Arc::new(FixedRules(Ok(open_rules()))),
                events: Arc::new(RecordingAdmissionEvents::default()),
                drops,
            }),
            catalog: Arc::new(PropertyCatalog::load().unwrap()),
        };
        let service = ReviewQueueService::new(
            Arc::new(writes),
            Arc::clone(&self.reviews) as _,
            Arc::default() as Arc<crate::test_support::RecordingReviewEvents>,
            Some(corrections),
            PolicyEnforcer::new(resolver),
        );
        register_review_queue_routes(
            Router::new(),
            &OpenApiRegistryImpl::new(),
            Arc::new(service),
        )
        .layer(Extension(context_of(caller, self.id)))
    }

    fn owner(&self) -> Router {
        self.routes(Arc::new(AllowResolver), self.subject, ON)
    }

    fn reviewer(&self) -> Router {
        self.routes(Arc::new(AllowResolver), Uuid::new_v4(), ON)
    }

    fn skill(&self) -> String {
        self.profile.live()[0].node_key.0.clone()
    }

    fn requests(&self) -> String {
        format!("/construct/v1/subjects/{}/review-requests", self.subject)
    }

    /// The skill flagged by the subject; the request's id.
    async fn flagged(&self) -> String {
        let answer = send(
            self.owner(),
            Method::POST,
            &self.requests(),
            Some(json!({ "fact_id": self.skill(), "comment": "wrong" })),
        )
        .await;
        assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
        answer.body["id"].as_str().unwrap().to_owned()
    }
}

fn resolution(request_id: &str) -> String {
    format!("/construct/v1/admin/review-requests/{request_id}/resolution")
}

const OPEN: &str = "/construct/v1/admin/review-requests";

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
async fn the_subject_flags_a_fact_and_sees_its_request() {
    let tenant = Tenant::new();
    let skill = tenant.skill();

    let flagged = send(
        tenant.owner(),
        Method::POST,
        &tenant.requests(),
        Some(json!({ "fact_id": skill, "comment": "wrong" })),
    )
    .await;
    let listed = send(tenant.owner(), Method::GET, &tenant.requests(), None).await;

    assert_eq!(flagged.status, StatusCode::CREATED, "{}", flagged.body);
    assert_eq!(flagged.body["fact_id"], json!(skill));
    assert_eq!(flagged.body["state"], json!("open"));
    assert_eq!(flagged.body["entity_kind"], json!("skill"));
    assert_eq!(flagged.body["value"], json!({ "name": "Rust" }));
    assert_eq!(flagged.body["comment"], json!("wrong"));
    assert!(flagged.body["created_at"].is_string());
    assert!(flagged.body.get("outcome").is_none(), "{}", flagged.body);
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"][0]["id"], flagged.body["id"]);
}

#[tokio::test]
async fn a_second_flag_is_409_and_another_subject_or_an_unknown_fact_404() {
    let tenant = Tenant::new();
    tenant.flagged().await;
    let body = |fact: &str| Some(json!({ "fact_id": fact }));

    let again = send(
        tenant.owner(),
        Method::POST,
        &tenant.requests(),
        body(&tenant.skill()),
    )
    .await;
    let unknown = send(
        tenant.owner(),
        Method::POST,
        &tenant.requests(),
        body("construct:t:nothing"),
    )
    .await;
    let another = send(
        tenant.routes(Arc::new(AllowResolver), Uuid::new_v4(), ON),
        Method::POST,
        &tenant.requests(),
        body(&tenant.skill()),
    )
    .await;

    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert_eq!(again.body["context"]["reason"], json!("ALREADY_OPEN"));
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    assert_eq!(another.status, StatusCode::NOT_FOUND, "{}", another.body);
}

#[tokio::test]
async fn a_comment_past_its_limit_is_400_and_nothing_is_flagged() {
    let tenant = Tenant::new();

    let answer = send(
        tenant.owner(),
        Method::POST,
        &tenant.requests(),
        Some(json!({ "fact_id": tenant.skill(), "comment": "x".repeat(1001) })),
    )
    .await;

    assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    assert_eq!(
        answer.body["context"]["field_violations"][0]["field"],
        json!("comment")
    );
    assert!(tenant.reviews.all().is_empty());
}

#[tokio::test]
async fn a_comment_and_a_reason_of_1000_characters_are_taken() {
    let tenant = Tenant::new();
    let at_limit = "\u{e9}".repeat(1000);

    let flagged = send(
        tenant.owner(),
        Method::POST,
        &tenant.requests(),
        Some(json!({ "fact_id": tenant.skill(), "comment": at_limit })),
    )
    .await;
    let id = flagged.body["id"].as_str().unwrap().to_owned();
    let rejected = send(
        tenant.reviewer(),
        Method::POST,
        &resolution(&id),
        Some(json!({ "outcome": "rejected", "reason": at_limit })),
    )
    .await;

    assert_eq!(flagged.status, StatusCode::CREATED, "{}", flagged.body);
    assert_eq!(flagged.body["comment"], json!(at_limit));
    assert_eq!(rejected.status, StatusCode::OK, "{}", rejected.body);
    assert_eq!(rejected.body["reason"], json!(at_limit));
}

#[tokio::test]
async fn a_reviewer_lists_the_open_requests_and_a_caller_without_the_permission_gets_403() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;

    let listed = send(tenant.reviewer(), Method::GET, OPEN, None).await;
    let refused = send(
        tenant.routes(Arc::new(DenyResolver), Uuid::new_v4(), ON),
        Method::GET,
        OPEN,
        None,
    )
    .await;

    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"][0]["id"], json!(id));
    assert_eq!(listed.body["items"][0]["value"], json!({ "name": "Rust" }));
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}

#[tokio::test]
async fn a_correction_answers_the_closed_request_with_the_new_facts_id() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;

    let answer = send(
        tenant.reviewer(),
        Method::POST,
        &resolution(&id),
        Some(json!({ "outcome": "corrected", "properties": { "skills": { "name": "Go" } } })),
    )
    .await;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.body["state"], json!("closed"));
    assert_eq!(answer.body["outcome"], json!("corrected"));
    assert!(answer.body.get("value").is_none(), "{}", answer.body);
    let new_fact = answer.body["corrected_fact_id"].as_str().unwrap();
    assert!(
        tenant
            .profile
            .live()
            .iter()
            .any(|fact| fact.node_key.0 == new_fact)
    );
}

#[tokio::test]
async fn a_correction_admission_refuses_is_422_and_the_request_stays_open() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;
    let off = SubjectSettings {
        personalization_enabled: false,
        erasure_in_progress: false,
    };

    let answer = send(
        tenant.routes(Arc::new(AllowResolver), Uuid::new_v4(), off),
        Method::POST,
        &resolution(&id),
        Some(json!({ "outcome": "corrected", "properties": { "skills": { "name": "Go" } } })),
    )
    .await;

    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        answer.body
    );
    let violation = &answer.body["context"]["violations"][0];
    assert_eq!(
        violation["type"],
        json!("personalization_off"),
        "{}",
        answer.body
    );
    assert_eq!(violation["subject"], json!("properties"));
    assert!(tenant.reviews.all()[0].is_open());
}

#[tokio::test]
async fn a_body_that_does_not_fit_its_outcome_is_400_naming_the_field() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;

    for (body, field) in [
        (json!({ "outcome": "rejected" }), "reason"),
        (json!({ "outcome": "rejected", "reason": "  " }), "reason"),
        (json!({ "outcome": "corrected" }), "properties"),
        (
            json!({ "outcome": "deleted", "properties": { "skills": { "name": "Go" } } }),
            "properties",
        ),
        (
            json!({ "outcome": "corrected", "properties": { "skills": { "name": "" } } }),
            "properties",
        ),
        (
            json!({ "outcome": "rejected", "reason": "x".repeat(1001) }),
            "reason",
        ),
        (
            json!({ "outcome": "corrected", "properties": {} }),
            "properties",
        ),
        (
            json!({
                "outcome": "corrected",
                "properties": { "skills": { "name": "Go" }, "roles": { "title": "Lead" } }
            }),
            "properties",
        ),
    ] {
        let answer = send(
            tenant.reviewer(),
            Method::POST,
            &resolution(&id),
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
    assert!(tenant.reviews.all()[0].is_open());
}

#[tokio::test]
async fn a_closed_request_is_409_and_an_unknown_one_404() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;
    let reject = || Some(json!({ "outcome": "rejected", "reason": "it is right" }));

    let first = send(tenant.reviewer(), Method::POST, &resolution(&id), reject()).await;
    let again = send(tenant.reviewer(), Method::POST, &resolution(&id), reject()).await;
    let unknown = send(
        tenant.reviewer(),
        Method::POST,
        &resolution(&Uuid::new_v4().to_string()),
        reject(),
    )
    .await;

    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["reason"], json!("it is right"));
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert_eq!(again.body["context"]["reason"], json!("CLOSED"));
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
}

#[tokio::test]
async fn a_held_or_unclosed_request_is_409_with_a_reason_a_client_tells_apart() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;
    let reject = || Some(json!({ "outcome": "rejected", "reason": "it is right" }));
    let correct =
        Some(json!({ "outcome": "corrected", "properties": { "skills": { "name": "Go" } } }));

    tenant.reviews.hold_first(time::OffsetDateTime::now_utc());
    let held = send(tenant.reviewer(), Method::POST, &resolution(&id), reject()).await;
    tenant
        .reviews
        .hold_first(time::OffsetDateTime::now_utc() - CLAIM_HOLD);
    tenant
        .reviews
        .close_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let unclosed = send(tenant.reviewer(), Method::POST, &resolution(&id), correct).await;

    assert_eq!(held.status, StatusCode::CONFLICT, "{}", held.body);
    assert_eq!(held.body["context"]["reason"], json!("BEING_RESOLVED"));
    assert_eq!(unclosed.status, StatusCode::CONFLICT, "{}", unclosed.body);
    assert_eq!(
        unclosed.body["context"]["reason"],
        json!("WRITTEN_NOT_CLOSED")
    );
}

#[tokio::test]
async fn every_answer_is_no_store_with_the_security_headers() {
    let tenant = Tenant::new();
    let id = tenant.flagged().await;

    for (router, method, uri, body) in [
        (tenant.owner(), Method::GET, tenant.requests(), None),
        (tenant.reviewer(), Method::GET, OPEN.to_owned(), None),
        (
            tenant.routes(Arc::new(DenyResolver), Uuid::new_v4(), ON),
            Method::POST,
            resolution(&id),
            Some(json!({ "outcome": "deleted" })),
        ),
    ] {
        let answer = send(router, method.clone(), &uri, body).await;
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(
                answer.headers.get(name).map(|v| v.to_str().unwrap()),
                Some(value),
                "{method} {uri}: {name}"
            );
        }
    }
}
