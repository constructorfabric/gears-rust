//! Router-level tests for the profile read. The security context the gateway
//! injects in production is injected with an `Extension` layer.

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverApi;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_routes;
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::profile::Link;
use crate::domain::profile_read::{
    CategoryCheck, LearnedFrom, OriginKind, StoredFact, StoredLinkFact,
};
use crate::test_support::{
    AllowResolver, DenyResolver, FixedFacts, IntakeFixture, RecordingProfileEvents,
    RecordingResolver, context_of, inmem_db, profile_service_over, settings_service,
};

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

fn rust_skill() -> StoredFact {
    StoredFact {
        node_key: NodeKey("construct:t:fact-1".to_owned()),
        category: Category::PROJECT,
        property: "project".to_owned(),
        value: json!({ "name": "Rust" }),
        owner: None,
        learned: Vec::new(),
        origin: OriginKind::Agent,
        stored_at: at("2026-10-21T10:00:00Z"),
        produced_at: Some(at("2026-10-21T09:59:00Z")),
    }
}

async fn get(
    resolver: Arc<dyn AuthZResolverApi>,
    facts: Vec<StoredFact>,
    uri: &str,
) -> (StatusCode, Option<String>, Value) {
    get_as(resolver, facts, uri, Uuid::new_v4(), Uuid::new_v4()).await
}

/// [`get`] as the caller `caller` of the tenant `tenant`.
async fn get_as(
    resolver: Arc<dyn AuthZResolverApi>,
    facts: Vec<StoredFact>,
    uri: &str,
    caller: Uuid,
    tenant: Uuid,
) -> (StatusCode, Option<String>, Value) {
    get_linked(resolver, facts, Vec::new(), uri, caller, tenant).await
}

/// [`get_as`] over facts with the links between them.
async fn get_linked(
    resolver: Arc<dyn AuthZResolverApi>,
    facts: Vec<StoredFact>,
    links: Vec<StoredLinkFact>,
    uri: &str,
    caller: Uuid,
    tenant: Uuid,
) -> (StatusCode, Option<String>, Value) {
    let db = inmem_db().await;
    let intake = Arc::new(IntakeFixture::default().build(&db));
    let settings = Arc::new(settings_service(&db, Arc::clone(&resolver), true));
    let profile = Arc::new(profile_service_over(
        &db,
        resolver,
        Arc::new(FixedFacts::with_links(facts, links)),
        Uuid::new_v4(),
        Arc::new(RecordingProfileEvents::default()),
        CategoryCheck::OneCall,
    ));
    let router: Router = register_routes(
        Router::new(),
        &OpenApiRegistryImpl::new(),
        intake,
        settings,
        profile,
    )
    .layer(Extension(context_of(caller, tenant)));
    let response = router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get(header::CACHE_CONTROL)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        cache,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn path(subject: Uuid) -> String {
    format!("/construct/v1/subjects/{subject}/profile")
}

#[tokio::test]
async fn a_profile_reads_as_its_categories_with_each_facts_property_origin_and_times() {
    let subject = Uuid::new_v4();

    let (status, cache, body) =
        get(Arc::new(AllowResolver), vec![rust_skill()], &path(subject)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(
        body,
        json!({
            "subject_id": subject.to_string(),
            "categories": [{
                "entity_kind": "project",
                "facts": [{
                    "fact_id": "construct:t:fact-1",
                    "property": "project",
                    "value": { "name": "Rust" },
                    "origin": { "kind": "agent" },
                    "stored_at": "2026-10-21T10:00:00Z",
                    "produced_at": "2026-10-21T09:59:00Z",
                }],
            }],
        })
    );
}

#[tokio::test]
async fn a_subject_without_facts_reads_as_no_categories() {
    let subject = Uuid::new_v4();

    let (status, _, body) = get(Arc::new(AllowResolver), Vec::new(), &path(subject)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["categories"], json!([]));
}

#[tokio::test]
async fn a_subject_the_caller_may_not_act_for_is_404_like_one_that_does_not_exist() {
    let (status, cache, body) = get(
        Arc::new(DenyResolver),
        vec![rust_skill()],
        &path(Uuid::new_v4()),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.get("categories").is_none(), "no profile in the answer");
    assert_eq!(
        cache.as_deref(),
        Some("no-store"),
        "errors are not stored either"
    );
}

#[tokio::test]
async fn a_named_tenant_is_taken_and_anything_else_in_the_query_is_refused() {
    let subject = Uuid::new_v4();

    let (named, _, _) = get(
        Arc::new(AllowResolver),
        Vec::new(),
        &format!("{}?tenant={}", path(subject), Uuid::new_v4()),
    )
    .await;
    let (unknown, _, _) = get(
        Arc::new(AllowResolver),
        Vec::new(),
        &format!("{}?tenant_id={}", path(subject), Uuid::new_v4()),
    )
    .await;

    assert_eq!(named, StatusCode::OK);
    assert_eq!(unknown, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_named_tenant_reaches_the_policy_request() {
    let named = Uuid::new_v4();
    let resolver = Arc::new(RecordingResolver::default());

    let (status, _, _) = get(
        Arc::clone(&resolver) as Arc<dyn AuthZResolverApi>,
        Vec::new(),
        &format!("{}?tenant={named}", path(Uuid::new_v4())),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let asked = resolver.asked.lock().unwrap();
    let (_, action, context, owner) = asked
        .iter()
        .find(|(resource, ..)| resource == crate::domain::profile_read::PROFILE_TYPE)
        .expect("the profile resource is asked about");
    assert_eq!(
        (action.as_str(), *context, owner.clone()),
        ("read", Some(named), Some(named.to_string()))
    );
}

#[tokio::test]
async fn a_fact_without_a_produced_at_has_no_such_key() {
    let mut fact = rust_skill();
    fact.produced_at = None;

    let (_, _, body) = get(Arc::new(AllowResolver), vec![fact], &path(Uuid::new_v4())).await;

    let served = &body["categories"][0]["facts"][0];
    assert!(served.get("produced_at").is_none(), "{served}");
    assert!(served.get("stored_at").is_some());
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_refused_read_is_logged_with_the_caller_the_subject_and_the_tenant() {
    let (subject, caller, home, named) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );

    let (own, _, _) = get_as(
        Arc::new(DenyResolver),
        vec![rust_skill()],
        &path(subject),
        caller,
        home,
    )
    .await;
    let (other, _, _) = get_as(
        Arc::new(DenyResolver),
        vec![rust_skill()],
        &format!("{}?tenant={named}", path(subject)),
        caller,
        home,
    )
    .await;

    assert_eq!(own, StatusCode::NOT_FOUND);
    assert_eq!(other, StatusCode::NOT_FOUND);
    assert!(logs_contain("construct profile read refused"));
    assert!(logs_contain(&format!("subject_id={subject}")));
    assert!(logs_contain(&format!("caller={caller}")));
    assert!(
        logs_contain(&format!("tenant_id={home}")),
        "the caller's own tenant"
    );
    assert!(
        logs_contain(&format!("tenant_id={named}")),
        "the tenant the read named"
    );
}

fn node(kind: Category, key: &str, value: Value, owner: Option<&str>) -> StoredFact {
    StoredFact {
        node_key: NodeKey(key.to_owned()),
        category: kind,
        property: kind.name().to_owned(),
        value,
        owner: owner.map(|owner| NodeKey(owner.to_owned())),
        learned: vec![LearnedFrom {
            tool: "chat".to_owned(),
            message: Some("m1".to_owned()),
        }],
        origin: OriginKind::Connector,
        stored_at: at("2026-10-21T10:00:00Z"),
        produced_at: None,
    }
}

#[tokio::test]
async fn a_node_reads_with_what_it_owns_what_it_links_to_and_where_it_was_learned() {
    let subject = Uuid::new_v4();
    let facts = vec![
        node(
            Category::PERSON,
            "construct:t:priya",
            json!({ "name": "Priya Shah" }),
            None,
        ),
        node(
            Category::PROJECT,
            "construct:t:project",
            json!({ "name": "Atlas" }),
            None,
        ),
        node(
            Category::COMMITMENT,
            "construct:t:promise",
            json!({ "kind": "promise", "text": "send the script" }),
            Some("construct:t:project"),
        ),
    ];
    let links = vec![StoredLinkFact {
        link: Link {
            kind: "works_on".to_owned(),
            from: NodeKey("construct:t:priya".to_owned()),
            to: NodeKey("construct:t:project".to_owned()),
            fields: json!({ "role": "lead" }),
        },
        learned: vec![LearnedFrom {
            tool: "mail".to_owned(),
            message: None,
        }],
    }];

    let (status, cache, body) = get_linked(
        Arc::new(AllowResolver),
        facts,
        links,
        &format!("/construct/v1/subjects/{subject}/profile/nodes/construct:t:project"),
        Uuid::new_v4(),
        Uuid::new_v4(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(body["node"]["entity_kind"], "project");
    assert_eq!(body["node"]["fact_id"], "construct:t:project");
    assert_eq!(body["node"]["value"], json!({ "name": "Atlas" }));
    assert_eq!(body["owned"][0]["entity_kind"], "commitment");
    assert_eq!(body["owned"][0]["owner"], "construct:t:project");
    assert_eq!(body["neighbours"][0]["link"], "works_on");
    let neighbour = &body["neighbours"][0]["neighbours"][0];
    assert_eq!(neighbour["direction"], "in");
    assert_eq!(neighbour["fields"], json!({ "role": "lead" }));
    assert_eq!(neighbour["node"]["entity_kind"], "person");
    assert_eq!(
        body["learned_from"],
        json!([{ "tool": "chat", "count": 1 }, { "tool": "mail", "count": 1 }])
    );
}

#[tokio::test]
async fn a_node_the_subject_does_not_have_or_a_malformed_id_is_404() {
    let subject = Uuid::new_v4();

    for id in ["construct:t:nothing", "a%20b"] {
        let (status, _, _) = get_linked(
            Arc::new(AllowResolver),
            vec![rust_skill()],
            Vec::new(),
            &format!("/construct/v1/subjects/{subject}/profile/nodes/{id}"),
            Uuid::new_v4(),
            Uuid::new_v4(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
    }
}
