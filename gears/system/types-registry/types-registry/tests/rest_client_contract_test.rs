#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The SDK's REST clients against the registry's real routes over TCP, behind the `OoP`
//! route-auth stack: the platform client on the platform routes (REST contract, T34; SPEC
//! §10.1, D17, D19) and the tenant client on the tenant reads (T35).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use secrecy::SecretString;
use serde_json::json;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument, Problem};
use toolkit_contract::runtime::config::{
    ClientConfig, CredentialState, InternalTokenProvider, RetryConfig,
};
use toolkit_gts::gts_id;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry::domain::local_client::LocalClient;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityFilter, EntityKey, EntityKind,
    EntityLookup, FieldSelection, IdempotencyKey, ListEntitiesRequest, Operation, OperationStatus,
    PageRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiRestClient, Projection,
    PublisherContext, ReadOptions, RegisterEntitiesRequest, RegisterItem, RegistrationOutcome,
    TypesRegistryApi, TypesRegistryApiExt, TypesRegistryApiRestClient, Validator, reason,
};
use uuid::Uuid;

mod common;
use common::platform_server::{INTERNAL_TOKEN, PlatformServer, TENANT_BEARER, serve, serve_with};

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const CF_OTHER: &str = gts_id!("cf.core.example.other.v1~");

/// One request as it reached the listener, before authentication.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    token: bool,
    bearer: bool,
    /// The `Authorization` header as sent.
    authorization: Option<String>,
}

/// Records every request, and answers the first `n` matching ones `503` before the
/// registry sees them.
#[derive(Default)]
struct Faults {
    seen: Mutex<Vec<Seen>>,
    /// `(method, path suffix, remaining failures)`.
    fail: Mutex<Vec<(String, String, usize)>>,
}

impl Faults {
    fn fail(&self, method: &str, suffix: &str, times: usize) {
        self.fail
            .lock()
            .push((method.to_owned(), suffix.to_owned(), times));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().clone()
    }

    fn wrap(self: &Arc<Self>) -> impl FnOnce(Router) -> Router + use<> {
        let faults = Arc::clone(self);
        move |app| {
            app.layer(from_fn(move |request: Request<Body>, next: Next| {
                let faults = Arc::clone(&faults);
                async move { faults.handle(request, next).await }
            }))
        }
    }

    async fn handle(&self, request: Request<Body>, next: Next) -> Response {
        let method = request.method().to_string();
        let path = request.uri().path().to_owned();
        self.seen.lock().push(Seen {
            method: method.clone(),
            path: path.clone(),
            token: request.headers().contains_key(INTERNAL_TOKEN_HEADER),
            bearer: request.headers().contains_key("authorization"),
            authorization: request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        });
        let refuse = {
            let mut rules = self.fail.lock();
            rules
                .iter_mut()
                .find(|(m, suffix, left)| *m == method && path.ends_with(suffix) && *left > 0)
                .map(|rule| rule.2 -= 1)
                .is_some()
        };
        if refuse {
            return (StatusCode::SERVICE_UNAVAILABLE, "injected").into_response();
        }
        next.run(request).await
    }
}

/// A client whose token provider counts how often it is consulted: once per attempt.
fn client(
    server: &PlatformServer,
    resolutions: &Arc<AtomicUsize>,
) -> PlatformTypesRegistryApiRestClient {
    let counter = Arc::clone(resolutions);
    let provider = InternalTokenProvider::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned()))
    });
    let config = ClientConfig::new(server.base_url.clone())
        .with_internal_token_provider(provider)
        .with_retry(RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(50),
            multiplier: 2.0,
        });
    PlatformTypesRegistryApiRestClient::new(config).expect("a REST client")
}

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "rest-contract-test".to_owned(),
        version: "1.0.0".parse().expect("version"),
    }
}

fn schema(gts_id: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn register(gts_id: &str) -> RegisterEntitiesRequest {
    RegisterEntitiesRequest {
        items: vec![RegisterItem {
            gts_id: gts::GtsId::try_new(gts_id).expect("an identifier"),
            content: schema(gts_id),
            expected_resource_version: None,
            skip_compatibility_check: false,
        }],
        dry_run: false,
        publisher: publisher(),
    }
}

fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey::new(text).expect("a key")
}

/// The local and the TCP client agree: the SDK's whole conformance suite over REST.
#[tokio::test]
async fn the_rest_client_conforms_to_the_contract() {
    let server = serve().await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    types_registry_sdk::testing_platform::conformance::run(&client(&server, &resolutions)).await;
}

/// Submit → poll → exact read → `batchGet` (`found`, then `unchanged` under the same
/// `$select`) → discovery; then replay and a conflicting reuse of the key.
#[tokio::test]
async fn submit_read_and_replay_over_tcp() {
    let faults = Arc::new(Faults::default());
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let operation = client
        .register_entities(&ctx(), key("tcp-register"), register(CF_TYPE))
        .await
        .expect("accepted and read back");
    let operation = if operation.status == OperationStatus::Completed {
        operation
    } else {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let Operation::Registration(polled) = client
                .get_operation(&ctx(), operation.operation_id)
                .await
                .expect("polled")
            else {
                panic!("a registration");
            };
            if polled.status == OperationStatus::Completed {
                break polled;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the operation completes"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(
        matches!(
            operation.items[0].outcome,
            RegistrationOutcome::Succeeded {
                resource_version: 1
            }
        ),
        "{:?}",
        operation.items
    );

    let selection = Projection::Select(FieldSelection::full());
    let read = |if_none_match| BatchGetEntitiesRequest {
        items: vec![BatchGetItem {
            key: EntityKey::GtsId(gts::GtsId::try_new(CF_TYPE).unwrap()),
            if_none_match,
        }],
        projection: selection.clone(),
        fresh: false,
    };
    let found = client
        .batch_get_entities(&ctx(), read(None))
        .await
        .expect("read")
        .0;
    let Some(EntityLookup::Found { entity, etag }) = found.values().next() else {
        panic!("found: {found:?}");
    };
    assert_eq!(entity.content, Some(schema(CF_TYPE)));
    let unchanged = client
        .batch_get_entities(&ctx(), read(Some(etag.clone())))
        .await
        .expect("read")
        .0;
    assert_eq!(
        unchanged.values().next(),
        Some(&EntityLookup::Unchanged { etag: etag.clone() })
    );

    let page = client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                filter: types_registry_sdk::EntityFilter {
                    pattern: Some(gts::GtsIdPattern::try_new(CF_TYPE).unwrap()),
                    ..Default::default()
                },
                projection: Projection::Default,
                page: PageRequest::default(),
            },
        )
        .await
        .expect("a page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].gts_id.as_ref(), CF_TYPE);

    let replay = client
        .register_entities(&ctx(), key("tcp-register"), register(CF_TYPE))
        .await
        .expect("replayed");
    assert_eq!(
        replay.operation_id, operation.operation_id,
        "the same operation"
    );
    assert!(matches!(
        replay.items[0].outcome,
        RegistrationOutcome::Succeeded {
            resource_version: 1
        }
    ));

    let conflict = client
        .register_entities(&ctx(), key("tcp-register"), register(CF_OTHER))
        .await
        .expect_err("a different request under one key");
    assert!(
        matches!(conflict, CanonicalError::AlreadyExists { .. }),
        "{conflict:?}"
    );

    let seen = faults.seen();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|request| request.token && !request.bearer),
        "every request carries the internal token and never a bearer: {seen:?}"
    );
    assert_eq!(
        resolutions.load(Ordering::SeqCst),
        seen.len(),
        "one token resolution per attempt"
    );
}

/// A transient failure retries a keyed submission and a read with a fresh token each
/// time; the retried submission is one operation.
#[tokio::test]
async fn transient_failures_are_retried_with_a_fresh_token() {
    let faults = Arc::new(Faults::default());
    faults.fail("POST", "/v2/platform/entities", 1);
    faults.fail("POST", "/v2/platform/entities:batchGet", 2);
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let operation = client
        .register_entities(&ctx(), key("tcp-retry"), register(CF_TYPE))
        .await
        .expect("accepted on the second attempt");
    client
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest {
                items: vec![BatchGetItem::from(EntityKey::GtsId(
                    gts::GtsId::try_new(CF_TYPE).unwrap(),
                ))],
                projection: Projection::Default,
                fresh: false,
            },
        )
        .await
        .expect("read on the third attempt");

    let submissions = faults
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "POST" && seen.path.ends_with("/v2/platform/entities"))
        .count();
    assert_eq!(submissions, 2, "one refused attempt, one accepted");
    assert_eq!(resolutions.load(Ordering::SeqCst), faults.seen().len());
    let records = server
        .service
        .operation(common::caller(), operation.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(records.operation_id, operation.operation_id);
}

/// D19: a lost read-back names the accepted operation; replaying the key recovers it.
#[tokio::test]
async fn a_lost_read_back_names_the_operation_and_a_replay_recovers_it() {
    let faults = Arc::new(Faults::default());
    faults.fail("GET", "", usize::MAX);
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let lost = client
        .register_entities(&ctx(), key("tcp-lost"), register(CF_TYPE))
        .await
        .expect_err("the read-back fails");
    let CanonicalError::Aborted {
        ctx: ref aborted,
        ref resource_name,
        ..
    } = lost
    else {
        panic!("Aborted, got {lost:?}");
    };
    assert_eq!(aborted.reason, reason::aborted::OPERATION_READ_FAILED);
    let operation_id: Uuid = resource_name
        .as_deref()
        .expect("names the operation")
        .parse()
        .expect("an operation id");

    faults.fail.lock().clear();
    let replay = client
        .register_entities(&ctx(), key("tcp-lost"), register(CF_TYPE))
        .await
        .expect("replayed");
    assert_eq!(replay.operation_id, operation_id);
}

/// Problems map to `CanonicalError` without loss: category, resource and violations.
#[tokio::test]
async fn refusals_map_to_their_canonical_errors() {
    let server = serve().await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let missing = Uuid::from_u128(7);
    let not_found = client
        .get_operation(&ctx(), missing)
        .await
        .expect_err("no such operation");
    let CanonicalError::NotFound {
        ref resource_type,
        ref resource_name,
        ..
    } = not_found
    else {
        panic!("NotFound, got {not_found:?}");
    };
    assert_eq!(
        resource_type.as_deref(),
        Some(types_registry_sdk::OPERATION_RESOURCE_TYPE)
    );
    assert_eq!(resource_name.as_deref(), Some(missing.to_string().as_str()));

    let refused = client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                page: PageRequest {
                    limit: Some(100_000),
                    cursor: None,
                },
                ..Default::default()
            },
        )
        .await
        .expect_err("a page over the configured maximum");
    let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        ..
    } = refused
    else {
        panic!("InvalidArgument, got {refused:?}");
    };
    assert_eq!(field_violations[0].field, "limit");
}

// --- The tenant client (T35) ---

/// A tenant client whose config also carries a counting internal-token provider, which a
/// tenant read must never consult.
fn tenant_client(
    server: &PlatformServer,
    resolutions: &Arc<AtomicUsize>,
) -> TypesRegistryApiRestClient {
    let counter = Arc::clone(resolutions);
    let provider = InternalTokenProvider::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned()))
    });
    let config = ClientConfig::new(server.base_url.clone())
        .with_internal_token_provider(provider)
        .with_retry(RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(50),
            multiplier: 2.0,
        });
    TypesRegistryApiRestClient::new(config).expect("a REST client")
}

/// A caller holding `bearer`, as the gateway hands it to a gear.
fn caller(bearer: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .bearer_token(bearer.to_owned())
        .build()
        .expect("a tenant context")
}

/// Registers `ids` through the served registry's local platform client and waits.
async fn seed(local: &LocalClient, ids: &[&str]) {
    let submitted = PlatformTypesRegistryApi::register_entities(
        local,
        &ctx(),
        key("tenant-seed"),
        RegisterEntitiesRequest {
            items: ids
                .iter()
                .map(|gts_id| RegisterItem {
                    gts_id: gts::GtsId::try_new(gts_id).expect("an identifier"),
                    content: schema(gts_id),
                    expected_resource_version: None,
                    skip_compatibility_check: false,
                })
                .collect(),
            dry_run: false,
            publisher: publisher(),
        },
    )
    .await
    .expect("accepted");
    common::await_delivery("the seed completes", || async {
        let operation = local
            .get_operation(&ctx(), submitted.operation_id)
            .await
            .expect("read");
        (operation.status() == OperationStatus::Completed).then_some(())
    })
    .await;
}

/// A refusal as a caller sees it on the wire, without the per-response trace members.
fn wire(error: CanonicalError) -> serde_json::Value {
    let mut problem = serde_json::to_value(Problem::from(error)).expect("serializes");
    for member in ["trace_id", "instance"] {
        problem.as_object_mut().expect("an object").remove(member);
    }
    problem
}

fn type_key(gts_id: &str) -> EntityKey {
    EntityKey::GtsId(gts::GtsId::try_new(gts_id).expect("an identifier"))
}

/// The tenant client over TCP answers every read as the in-process tenant client does:
/// lookups, validators, `unchanged`, cursors and refusals.
#[tokio::test]
async fn tenant_reads_over_tcp_answer_as_the_local_tenant_client() {
    let server = serve().await;
    let local = LocalClient::new(Arc::clone(&server.service));
    seed(&local, &[CF_TYPE, CF_OTHER]).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let remote = tenant_client(&server, &resolutions);
    let tenant = caller(TENANT_BEARER);

    let projection = Projection::Select(FieldSelection::with(&[
        EntityField::Content,
        EntityField::Origin,
    ]));
    let absent = type_key(gts_id!("cf.core.example.absent.v1~"));
    let first = BatchGetEntitiesRequest {
        items: vec![type_key(CF_TYPE).into(), absent.clone().into()],
        projection: projection.clone(),
        fresh: false,
    };
    let found = TypesRegistryApi::batch_get_entities(&remote, &tenant, first.clone())
        .await
        .expect("read");
    assert_eq!(
        found,
        TypesRegistryApi::batch_get_entities(&local, &tenant, first)
            .await
            .expect("read"),
        "the same lookups, validator bytes included"
    );
    let EntityLookup::Found { entity, etag } = &found.0[&type_key(CF_TYPE)] else {
        panic!("found: {found:?}");
    };
    assert_eq!(entity.content, Some(schema(CF_TYPE)));
    assert_eq!(found.0[&absent], EntityLookup::NotFound);

    let conditional = BatchGetEntitiesRequest {
        items: vec![BatchGetItem {
            key: type_key(CF_TYPE),
            if_none_match: Some(etag.clone()),
        }],
        projection,
        fresh: false,
    };
    let unchanged = TypesRegistryApi::batch_get_entities(&remote, &tenant, conditional.clone())
        .await
        .expect("read");
    assert_eq!(
        unchanged.0[&type_key(CF_TYPE)],
        EntityLookup::Unchanged { etag: etag.clone() }
    );
    assert_eq!(
        unchanged,
        TypesRegistryApi::batch_get_entities(&local, &tenant, conditional)
            .await
            .expect("read")
    );

    let options = ReadOptions {
        projection: Projection::Select(FieldSelection::with(&[EntityField::Content])),
        ..ReadOptions::default()
    };
    let type_id = gts::GtsTypeId::try_new(CF_TYPE).expect("a Type Schema identifier");
    assert_eq!(
        TypesRegistryApiExt::get_type_schema(&remote, &tenant, &type_id, options.clone())
            .await
            .expect("present"),
        TypesRegistryApiExt::get_type_schema(&local, &tenant, &type_id, options)
            .await
            .expect("present"),
        "the extension's exact read"
    );

    let oversized = BatchGetEntitiesRequest {
        items: vec![BatchGetItem {
            key: type_key(CF_TYPE),
            if_none_match: Some(Validator::from_bytes(vec![b'v'; 2048])),
        }],
        projection: Projection::Default,
        fresh: false,
    };
    assert_eq!(
        wire(
            TypesRegistryApi::batch_get_entities(&remote, &tenant, oversized.clone())
                .await
                .expect_err("over the validator bound")
        ),
        wire(
            TypesRegistryApi::batch_get_entities(&local, &tenant, oversized)
                .await
                .expect_err("over the validator bound")
        )
    );

    let filter = EntityFilter {
        pattern: Some(gts::GtsIdPattern::try_new("gts.cf.core.example.*").expect("pattern")),
        kind: Some(EntityKind::TypeSchema),
        ..EntityFilter::default()
    };
    let page = |cursor| ListEntitiesRequest {
        filter: filter.clone(),
        projection: Projection::Default,
        page: PageRequest {
            limit: Some(1),
            cursor,
        },
    };
    let first_page = TypesRegistryApi::list_entities(&remote, &tenant, page(None))
        .await
        .expect("first page");
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(
        first_page,
        TypesRegistryApi::list_entities(&local, &tenant, page(None))
            .await
            .expect("first page"),
        "the same page and the same cursor bytes"
    );
    let cursor = first_page.next.clone().expect("a second page exists");
    let second_page = TypesRegistryApi::list_entities(&remote, &tenant, page(Some(cursor.clone())))
        .await
        .expect("second page");
    assert_ne!(second_page.items[0].gts_id, first_page.items[0].gts_id);
    assert_eq!(second_page.next, None);
    assert_eq!(
        second_page,
        TypesRegistryApi::list_entities(&local, &tenant, page(Some(cursor.clone())))
            .await
            .expect("second page")
    );

    let rebound = ListEntitiesRequest {
        filter: EntityFilter {
            kind: Some(EntityKind::Instance),
            ..filter.clone()
        },
        ..page(Some(cursor))
    };
    assert_eq!(
        wire(
            TypesRegistryApi::list_entities(&remote, &tenant, rebound.clone())
                .await
                .expect_err("a cursor replayed under another filter")
        ),
        wire(
            TypesRegistryApi::list_entities(&local, &tenant, rebound)
                .await
                .expect_err("a cursor replayed under another filter")
        )
    );
    assert_eq!(
        resolutions.load(Ordering::SeqCst),
        0,
        "a tenant read never consults the internal token"
    );
}

/// Each call sends its own caller's bearer, which the listener validates again; a call
/// never sends the internal token, and a caller without a bearer sends none.
#[tokio::test]
async fn each_tenant_call_forwards_its_own_bearer_and_never_the_internal_token() {
    let faults = Arc::new(Faults::default());
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let remote = tenant_client(&server, &resolutions);
    let read = || BatchGetEntitiesRequest {
        items: vec![type_key(CF_TYPE).into()],
        projection: Projection::Default,
        fresh: false,
    };

    TypesRegistryApi::batch_get_entities(&remote, &caller(TENANT_BEARER), read())
        .await
        .expect("a valid bearer is served");
    let forged = TypesRegistryApi::batch_get_entities(&remote, &caller("forged-bearer"), read())
        .await
        .expect_err("the listener validates the forwarded bearer");
    assert!(
        matches!(forged, CanonicalError::Unauthenticated { .. }),
        "{forged:?}"
    );
    let anonymous = TypesRegistryApi::list_entities(
        &remote,
        &SecurityContext::anonymous(),
        ListEntitiesRequest::default(),
    )
    .await
    .expect_err("no bearer, no tenant read");
    assert!(
        matches!(anonymous, CanonicalError::Unauthenticated { .. }),
        "{anonymous:?}"
    );

    let seen = faults.seen();
    assert_eq!(
        seen.iter()
            .map(|request| (request.path.as_str(), request.authorization.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            (
                "/types-registry/v2/entities:batchGet",
                Some(format!("Bearer {TENANT_BEARER}").as_str())
            ),
            (
                "/types-registry/v2/entities:batchGet",
                Some("Bearer forged-bearer")
            ),
            ("/types-registry/v2/entities", None),
        ],
        "one attempt each: a refused credential is not retried"
    );
    assert!(
        seen.iter().all(|request| !request.token),
        "never the internal token: {seen:?}"
    );
    assert_eq!(resolutions.load(Ordering::SeqCst), 0);
}

/// Both tenant reads are safe, so a transient failure is retried with the same bearer.
#[tokio::test]
async fn transient_tenant_failures_are_retried_with_the_same_bearer() {
    let faults = Arc::new(Faults::default());
    faults.fail("POST", "/v2/entities:batchGet", 2);
    faults.fail("GET", "/v2/entities", 1);
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let remote = tenant_client(&server, &resolutions);
    let tenant = caller(TENANT_BEARER);

    TypesRegistryApi::batch_get_entities(
        &remote,
        &tenant,
        BatchGetEntitiesRequest {
            items: vec![type_key(CF_TYPE).into()],
            projection: Projection::Default,
            fresh: false,
        },
    )
    .await
    .expect("read on the third attempt");
    TypesRegistryApi::list_entities(&remote, &tenant, ListEntitiesRequest::default())
        .await
        .expect("listed on the second attempt");

    let seen = faults.seen();
    let attempts = |method: &str, path: &str| {
        seen.iter()
            .filter(|request| request.method == method && request.path == path)
            .count()
    };
    assert_eq!(attempts("POST", "/types-registry/v2/entities:batchGet"), 3);
    assert_eq!(attempts("GET", "/types-registry/v2/entities"), 2);
    let bearer = format!("Bearer {TENANT_BEARER}");
    assert!(
        seen.iter().all(
            |request| request.authorization.as_deref() == Some(bearer.as_str()) && !request.token
        ),
        "{seen:?}"
    );
}

/// The tenant exact read has no client method — the extension reads through `batchGet` —
/// so its HTTP contract is checked as sent: a bearer-authenticated `GET` carries the
/// key's `ETag`, the quoted form of the validator the tenant client returns, and a current
/// `If-None-Match` answers a bodyless `304` with the same `ETag`.
#[tokio::test]
async fn the_tenant_exact_read_serves_the_clients_validator_and_a_bodyless_304() {
    let server = serve().await;
    let local = LocalClient::new(Arc::clone(&server.service));
    seed(&local, &[CF_TYPE]).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let remote = tenant_client(&server, &resolutions);
    let found = TypesRegistryApi::batch_get_entities(
        &remote,
        &caller(TENANT_BEARER),
        BatchGetEntitiesRequest {
            items: vec![type_key(CF_TYPE).into()],
            projection: Projection::Default,
            fresh: false,
        },
    )
    .await
    .expect("read");
    let Some(EntityLookup::Found { etag, .. }) = found.0.get(&type_key(CF_TYPE)) else {
        panic!("found: {found:?}");
    };
    let quoted = format!(
        "\"{}\"",
        std::str::from_utf8(etag.as_bytes()).expect("a token")
    );

    let http = toolkit_http::HttpClientBuilder::new()
        .retry(None)
        .build()
        .expect("an HTTP client");
    let url = format!("{}/types-registry/v2/entities/{CF_TYPE}", server.base_url);
    let read = http
        .get(&url)
        .bearer_auth(TENANT_BEARER)
        .send()
        .await
        .expect("a response");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(
        read.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some(quoted.as_str()),
        "the header is the quoted client validator"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&read.bytes().await.expect("a body")).expect("JSON");
    assert_eq!(body["gts_id"], CF_TYPE);

    let not_modified = http
        .get(&url)
        .bearer_auth(TENANT_BEARER)
        .header("if-none-match", &quoted)
        .send()
        .await
        .expect("a response");
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        not_modified
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok()),
        Some(quoted.as_str())
    );
    assert!(not_modified.bytes().await.expect("a body").is_empty());

    let refused = http
        .get(&url)
        .header(INTERNAL_TOKEN_HEADER, INTERNAL_TOKEN)
        .send()
        .await
        .expect("a response");
    assert_eq!(
        refused.status(),
        StatusCode::UNAUTHORIZED,
        "a tenant read is not served to the internal token alone"
    );
}
