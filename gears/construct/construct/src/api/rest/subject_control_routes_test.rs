//! Router-level tests for Subject Control's routes. The security context the
//! gateway injects in production is injected with an `Extension` layer.

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverApi;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_subject_control_routes;
use crate::domain::profile::Category;
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    AllowResolver, DenyResolver, FixedSubjectSettings, MemoryProfile, MemoryWriter, context_of,
    stored_fact, subject_control,
};

const FACTS_RESOURCE_TYPE: &str = "gts.cf.construct.subject.facts.v1~";

struct Subject {
    tenant: Uuid,
    id: Uuid,
    profile: Arc<MemoryProfile>,
}

impl Subject {
    /// A subject with a skill stored at second 10 and a role at second 20.
    fn new() -> Self {
        Self {
            tenant: Uuid::new_v4(),
            id: Uuid::new_v4(),
            profile: MemoryProfile::with(vec![
                stored_fact(Category::Skill, "skills", json!({ "name": "Rust" }), 10),
                stored_fact(Category::Role, "roles", json!({ "title": "Engineer" }), 20),
            ]),
        }
    }

    /// The routes as `caller` of the subject's tenant sees them.
    fn routes(&self, resolver: Arc<dyn AuthZResolverApi>, caller: Uuid) -> Router {
        let service = subject_control(
            resolver,
            Arc::clone(&self.profile),
            MemoryWriter::new(Arc::clone(&self.profile), 0),
            Arc::new(FixedSubjectSettings(Ok(SubjectSettings {
                personalization_enabled: false,
                erasure_in_progress: false,
            }))),
            Arc::default(),
            Some(Uuid::new_v4()),
        );
        register_subject_control_routes(
            Router::new(),
            &OpenApiRegistryImpl::new(),
            Arc::new(service),
        )
        .layer(Extension(context_of(caller, self.tenant)))
    }

    fn own(&self) -> Router {
        self.routes(Arc::new(AllowResolver), self.id)
    }

    fn fact_id(&self, category: Category) -> String {
        self.profile
            .live()
            .into_iter()
            .find(|fact| fact.category == category)
            .unwrap()
            .node_key
            .0
    }

    fn facts(&self) -> String {
        format!("/construct/v1/subjects/{}/facts", self.id)
    }

    fn fact(&self, fact_id: &str) -> String {
        format!("/construct/v1/subjects/{}/facts/{fact_id}", self.id)
    }
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn send(router: Router, method: Method, uri: &str) -> Answer {
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Answer {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

/// `uri` with its query string percent-encoded where it has spaces and quotes.
fn encoded(uri: &str) -> String {
    uri.replace(' ', "%20").replace('\'', "%27")
}

#[tokio::test]
async fn the_owner_lists_its_facts_newest_first_with_kind_origin_and_version() {
    let subject = Subject::new();

    let answer = send(subject.own(), Method::GET, &subject.facts()).await;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let items = answer.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["entity_kind"], "role");
    assert_eq!(items[0]["fact_id"], subject.fact_id(Category::Role));
    assert_eq!(
        items[0]["properties"],
        json!({ "roles": { "title": "Engineer" } })
    );
    assert_eq!(items[0]["origin"], json!({ "kind": "connector" }));
    assert_eq!(items[0]["stored_at"], "1970-01-01T00:00:20Z");
    assert_eq!(items[1]["entity_kind"], "skill");
    assert_eq!(answer.body["version"], 1);
    assert_eq!(answer.body["page_info"]["limit"], 50);
    assert!(answer.body["page_info"]["next_cursor"].is_null());
}

#[tokio::test]
async fn the_list_filters_orders_and_pages() {
    let subject = Subject::new();

    let skills = send(
        subject.own(),
        Method::GET,
        &encoded(&format!(
            "{}?$filter=entity_kind eq 'skill'",
            subject.facts()
        )),
    )
    .await;
    let oldest = send(
        subject.own(),
        Method::GET,
        &encoded(&format!(
            "{}?$orderby=stored_at asc&limit=1",
            subject.facts()
        )),
    )
    .await;
    let bad = send(
        subject.own(),
        Method::GET,
        &encoded(&format!("{}?$filter=value eq 'x'", subject.facts())),
    )
    .await;

    assert_eq!(skills.status, StatusCode::OK, "{}", skills.body);
    assert_eq!(skills.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(skills.body["items"][0]["entity_kind"], "skill");
    assert_eq!(oldest.body["items"][0]["entity_kind"], "skill");
    assert!(oldest.body["page_info"]["next_cursor"].is_string());
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
}

#[tokio::test]
async fn another_subject_or_a_refused_owner_gets_404_on_the_facts_resource() {
    let subject = Subject::new();
    let skill = subject.fact_id(Category::Skill);

    for (who, router) in [
        (
            "a colleague",
            subject.routes(Arc::new(AllowResolver), Uuid::new_v4()),
        ),
        (
            "a refused owner",
            subject.routes(Arc::new(DenyResolver), subject.id),
        ),
    ] {
        for (method, uri) in [
            (Method::GET, subject.facts()),
            (Method::GET, subject.fact(&skill)),
            (Method::DELETE, subject.fact(&skill)),
        ] {
            let answer = send(router.clone(), method.clone(), &uri).await;
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{who} {method} {uri}");
            assert_eq!(
                answer.body["context"]["resource_type"], FACTS_RESOURCE_TYPE,
                "{who} {method} {uri}: {}",
                answer.body
            );
        }
    }
    assert_eq!(subject.profile.live().len(), 2, "nothing was deleted");
}

#[tokio::test]
async fn the_owner_reads_one_fact_deletes_it_and_then_it_is_not_found() {
    let subject = Subject::new();
    let skill = subject.fact_id(Category::Skill);

    let read = send(subject.own(), Method::GET, &subject.fact(&skill)).await;
    let deleted = send(subject.own(), Method::DELETE, &subject.fact(&skill)).await;
    let again = send(subject.own(), Method::GET, &subject.fact(&skill)).await;
    let deleted_again = send(subject.own(), Method::DELETE, &subject.fact(&skill)).await;

    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body["fact_id"], skill);
    assert_eq!(read.body["entity_kind"], "skill");
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(deleted_again.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_export_is_a_json_attachment_and_403_without_its_permission() {
    let subject = Subject::new();
    let officer = Uuid::new_v4();
    let uri = format!("/construct/v1/subjects/{}/export", subject.id);

    let exported = send(
        subject.routes(Arc::new(AllowResolver), officer),
        Method::GET,
        &uri,
    )
    .await;
    let refused = send(
        subject.routes(Arc::new(DenyResolver), officer),
        Method::GET,
        &uri,
    )
    .await;

    assert_eq!(exported.status, StatusCode::OK, "{}", exported.body);
    assert_eq!(
        exported.headers[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"subject-{}.json\"", subject.id)
    );
    assert_eq!(exported.body["subject_id"], subject.id.to_string());
    assert_eq!(exported.body["tenant_id"], subject.tenant.to_string());
    assert_eq!(exported.body["settings"]["personalization_enabled"], false);
    assert_eq!(exported.body["facts"].as_array().unwrap().len(), 2);
    assert!(exported.body["exported_at"].is_string());
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(
        refused.body["context"]["resource_type"],
        FACTS_RESOURCE_TYPE
    );
}

#[tokio::test]
async fn every_answer_is_no_store_with_the_security_headers() {
    let subject = Subject::new();
    let skill = subject.fact_id(Category::Skill);

    for (router, method, uri) in [
        (subject.own(), Method::GET, subject.facts()),
        (subject.own(), Method::GET, subject.fact(&skill)),
        (
            subject.routes(Arc::new(AllowResolver), Uuid::new_v4()),
            Method::DELETE,
            subject.fact(&skill),
        ),
        (
            subject.own(),
            Method::GET,
            format!("/construct/v1/subjects/{}/export", subject.id),
        ),
    ] {
        let answer = send(router, method.clone(), &uri).await;
        for (name, value) in [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ] {
            assert_eq!(
                answer.headers.get(&name).and_then(|v| v.to_str().ok()),
                Some(value),
                "{name} on {method} {uri}"
            );
        }
    }
}
