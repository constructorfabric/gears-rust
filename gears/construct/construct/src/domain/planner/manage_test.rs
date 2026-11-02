use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::Predicate;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use tower::ServiceExt;
use uuid::Uuid;

use super::manage::ManageFacts;
use super::processing::Written;
use super::test_data::{AGENT, Chain, Contexts, Profiles, SUBJECT, TENANT, adding_rust};
use crate::api::rest::dto::{CONVERSATION_LIMIT, MESSAGE_LIMIT};
use crate::api::rest::routes::register_manage_route;
use crate::domain::plan::Origin;
use crate::test_support::{
    AllowResolver, DenyResolver, OwnTenantOnlyResolver, RecordingResolver, context_of,
};

/// The tenant an application acting for the subject logs in to.
const APP_HOME: Uuid = Uuid::from_u128(0x0a99);

fn router(chain: &Chain, resolver: Arc<dyn AuthZResolverApi>, caller: Uuid) -> Router {
    router_in(chain, resolver, caller, TENANT)
}

fn router_in(
    chain: &Chain,
    resolver: Arc<dyn AuthZResolverApi>,
    caller: Uuid,
    home: Uuid,
) -> Router {
    let manage = ManageFacts::new(
        Arc::new(chain.processing()),
        Arc::new(Contexts(chain.contexts)),
        PolicyEnforcer::new(resolver),
        Duration::from_secs(20),
    );
    register_manage_route(Router::new(), &OpenApiRegistryImpl::new(), Arc::new(manage))
        .layer(Extension(context_of(caller, home)))
}

/// An application acting for the subject from its own tenant, naming the subject's tenant.
fn app_router(chain: &Chain, resolver: Arc<dyn AuthZResolverApi>) -> Router {
    router_in(chain, resolver, AGENT, APP_HOME)
}

/// Allows like [`AllowResolver`] but names only the tenant: a grant on every subject of the tenant.
struct TenantWideResolver;

#[async_trait]
impl AuthZResolverApi for TenantWideResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let mut response = AllowResolver.evaluate(ctx, request).await?;
        for constraint in &mut response.context.constraints {
            constraint.predicates.retain(
                |predicate| !matches!(predicate, Predicate::In(p) if p.property == pep_properties::RESOURCE_ID),
            );
        }
        Ok(response)
    }
}

/// Allows with no constraint at all.
struct UnconstrainedResolver;

#[async_trait]
impl AuthZResolverApi for UnconstrainedResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

async fn post(router: Router, subject_id: Uuid, body: &Value) -> (StatusCode, Value) {
    post_to(
        router,
        &format!("/construct/v1/subjects/{subject_id}/facts/manage"),
        body,
    )
    .await
}

async fn post_for_tenant(router: Router, tenant: Uuid, body: &Value) -> (StatusCode, Value) {
    post_to(
        router,
        &format!("/construct/v1/subjects/{SUBJECT}/facts/manage?tenant={tenant}"),
        body,
    )
    .await
}

async fn post_to(router: Router, uri: &str, body: &Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
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
            Arc::new(DenyResolver) as Arc<dyn AuthZResolverApi>,
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

#[tokio::test]
async fn every_answer_of_the_route_is_no_store_with_the_security_headers() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    for (resolver, expected) in [
        (
            Arc::new(AllowResolver) as Arc<dyn AuthZResolverApi>,
            StatusCode::OK,
        ),
        (Arc::new(DenyResolver) as _, StatusCode::NOT_FOUND),
    ] {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/construct/v1/subjects/{SUBJECT}/facts/manage"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(said("I also know Rust.").to_string()))
            .expect("a request");
        let response = router(&chain, resolver, SUBJECT)
            .oneshot(request)
            .await
            .expect("a response");

        assert_eq!(response.status(), expected);
        for (name, value) in [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; frame-ancestors 'none'",
            ),
        ] {
            assert_eq!(
                response.headers().get(&name).and_then(|v| v.to_str().ok()),
                Some(value),
                "{name} on {expected}"
            );
        }
    }
}

#[tokio::test]
async fn the_route_asks_the_policy_service_for_manage_on_the_subject_facts() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    let resolver = Arc::new(crate::test_support::RecordingResolver::default());

    let (status, body) = post(
        router(&chain, Arc::clone(&resolver) as _, SUBJECT),
        SUBJECT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let asked: Vec<(String, String)> = resolver
        .asked
        .lock()
        .unwrap()
        .iter()
        .map(|(resource, action, ..)| (resource.clone(), action.clone()))
        .collect();
    assert_eq!(
        asked,
        [(
            "gts.cf.construct.subject.facts.v1~".to_owned(),
            "manage".to_owned()
        )]
    );
}

#[tokio::test]
async fn an_application_with_the_permission_manages_the_facts_of_a_subject_in_the_named_tenant() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    let (status, body) = post_for_tenant(
        app_router(&chain, Arc::new(AllowResolver)),
        TENANT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], "stored");
    let plans = chain.writer.plans();
    let [plan] = plans.as_slice() else {
        panic!("one plan, got {plans:?}");
    };
    assert_eq!(
        (plan.tenant_id, plan.subject_id, &plan.origin),
        (TENANT, SUBJECT, &Origin::Agent { agent: AGENT })
    );
}

#[tokio::test]
async fn a_grant_that_names_only_the_tenant_covers_every_subject_in_it() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    let (status, body) = post_for_tenant(
        app_router(&chain, Arc::new(TenantWideResolver)),
        TENANT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(chain.writer.plans().len(), 1);
}

#[tokio::test]
async fn an_application_without_a_grant_on_the_named_tenant_answers_as_a_subject_that_does_not_exist()
 {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    for resolver in [
        Arc::new(DenyResolver) as Arc<dyn AuthZResolverApi>,
        Arc::new(OwnTenantOnlyResolver) as _,
        Arc::new(UnconstrainedResolver) as _,
    ] {
        let (status, body) = post_for_tenant(
            app_router(&chain, resolver),
            TENANT,
            &said("I also know Rust."),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
    assert!(chain.writer.plans().is_empty());
    assert!(chain.model.requests().is_empty());
}

#[tokio::test]
async fn an_application_asks_the_policy_service_about_the_named_tenant() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    let resolver = Arc::new(RecordingResolver::default());

    let (status, body) = post_for_tenant(
        app_router(&chain, Arc::clone(&resolver) as _),
        TENANT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let asked = resolver.asked.lock().unwrap();
    let [(resource, action, context, owner)] = asked.as_slice() else {
        panic!("one policy call, got {asked:?}");
    };
    assert_eq!(
        (
            resource.as_str(),
            action.as_str(),
            *context,
            owner.as_deref()
        ),
        (
            "gts.cf.construct.subject.facts.v1~",
            "manage",
            Some(TENANT),
            Some(TENANT.to_string().as_str())
        )
    );
}

#[tokio::test]
async fn an_application_without_a_processing_context_stores_nothing() {
    let mut chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    chain.contexts = false;

    let (status, body) = post_for_tenant(
        app_router(&chain, Arc::new(AllowResolver)),
        TENANT,
        &said("I also know Rust."),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(chain.writer.plans().is_empty());
}
