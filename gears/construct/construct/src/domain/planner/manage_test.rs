use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;
use uuid::Uuid;

use super::manage::ManageFacts;
use super::processing::Written;
use super::test_data::{Chain, Profiles, SUBJECT, TENANT, adding_rust};
use crate::api::rest::dto::{CONVERSATION_LIMIT, MESSAGE_LIMIT};
use crate::api::rest::routes::register_manage_route;
use crate::test_support::{AllowResolver, DenyResolver, context_of};

fn router(chain: &Chain, resolver: Arc<dyn AuthZResolverApi>, caller: Uuid) -> Router {
    let manage = ManageFacts::new(
        chain.processing(),
        PolicyEnforcer::new(resolver),
        Duration::from_secs(20),
    );
    register_manage_route(Router::new(), &OpenApiRegistryImpl::new(), Arc::new(manage))
        .layer(Extension(context_of(caller, TENANT)))
}

async fn post(router: Router, subject_id: Uuid, body: &Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/construct/v1/subjects/{subject_id}/facts/manage"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("a request");
    let response = router.oneshot(request).await.expect("a response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("a body");
    (status, serde_json::from_slice(&bytes).expect("a JSON body"))
}

fn said(message: &str) -> Value {
    json!({ "message": message, "conversation": "agent: Which languages do you code in?" })
}

#[tokio::test]
async fn the_subject_gets_what_was_stored() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    let (status, body) = post(
        router(&chain, Arc::new(AllowResolver), SUBJECT),
        SUBJECT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "outcome": "stored", "reason": null, "added": 1, "replaced": 0, "removed": 0 })
    );
    assert_eq!(chain.writer.plans().len(), 1);
}

#[tokio::test]
async fn a_subject_the_caller_may_not_act_for_answers_as_one_that_does_not_exist() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    for (resolver, caller) in [
        (
            Arc::new(AllowResolver) as Arc<dyn AuthZResolverApi>,
            Uuid::new_v4(),
        ),
        (Arc::new(DenyResolver) as Arc<dyn AuthZResolverApi>, SUBJECT),
    ] {
        let (status, body) = post(
            router(&chain, resolver, caller),
            SUBJECT,
            &said("I also know Rust."),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
    assert!(chain.writer.plans().is_empty());
    assert!(chain.model.requests().is_empty());
}

#[tokio::test]
async fn a_blank_message_stores_nothing_without_a_model_call() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    let (status, body) = post(
        router(&chain, Arc::new(AllowResolver), SUBJECT),
        SUBJECT,
        &said("   "),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&body["outcome"], &body["reason"]),
        (&json!("nothing_changed"), &json!("no_change"))
    );
    assert!(chain.model.requests().is_empty());
}

#[tokio::test]
async fn a_text_past_its_limit_is_refused_before_a_model_call() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    let at_the_limits = json!({
        "message": "a".repeat(MESSAGE_LIMIT),
        "conversation": "b".repeat(CONVERSATION_LIMIT),
    });
    let long_message = json!({ "message": "a".repeat(MESSAGE_LIMIT + 1) });
    let long_conversation = json!({
        "message": "I also know Rust.",
        "conversation": "b".repeat(CONVERSATION_LIMIT + 1),
    });

    for (body, field) in [
        (long_message, "message"),
        (long_conversation, "conversation"),
    ] {
        let (status, problem) = post(
            router(&chain, Arc::new(AllowResolver), SUBJECT),
            SUBJECT,
            &body,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
        assert_eq!(problem["context"]["field_violations"][0]["field"], field);
    }
    assert!(chain.model.requests().is_empty());

    let (status, body) = post(
        router(&chain, Arc::new(AllowResolver), SUBJECT),
        SUBJECT,
        &at_the_limits,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_call_with_personalization_off_says_so() {
    let mut chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    chain.settings.personalization_enabled = false;

    let (status, body) = post(
        router(&chain, Arc::new(AllowResolver), SUBJECT),
        SUBJECT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "outcome": "nothing_changed", "reason": "personalization_off", "added": 0, "replaced": 0, "removed": 0 })
    );
}

#[tokio::test]
async fn an_unreachable_profile_answers_unavailable() {
    let chain = Chain {
        profiles: Arc::new(Profiles {
            failing: true,
            ..Profiles::default()
        }),
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };

    let (status, _) = post(
        router(&chain, Arc::new(AllowResolver), SUBJECT),
        SUBJECT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
