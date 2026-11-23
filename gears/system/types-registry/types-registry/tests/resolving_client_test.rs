#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The SDK's directory-resolving clients against real registries over TCP (REST contract,
//! T35): each call resolves the registry anew, follows it when it moves, and is a
//! canonical `ServiceUnavailable` while nothing is resolvable — never a fallback.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use secrecy::SecretString;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{CredentialState, InternalTokenProvider};
use toolkit_contract::runtime::resolving::{EndpointResolver, ResolveError};
use toolkit_contract::wiring::ClientTuning;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry::domain::local_client::LocalClient;
use types_registry_sdk::{
    BatchGetEntitiesRequest, DeleteEntitiesRequest, DeleteItem, DeletionOutcome, EntityFilter,
    EntityKey, EntityLookup, IdempotencyKey, ListEntitiesRequest, Operation, OperationStatus,
    PlatformTypesRegistryApi, PlatformTypesRegistryApiRestResolvingClient, Projection,
    PublisherContext, RegisterEntitiesRequest, RegisterItem, RegistrationOutcome, TypesRegistryApi,
    TypesRegistryApiRestResolvingClient,
};
use uuid::Uuid;

mod common;
use common::platform_server::{INTERNAL_TOKEN, PlatformServer, TENANT_BEARER, serve};

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");

/// What the directory answers next: an endpoint, nothing registered, or an outage.
#[derive(Clone)]
enum Directory {
    At(String),
    Empty,
    Down,
}

/// A directory the test points wherever it likes.
struct Scripted(Mutex<Directory>);

impl Scripted {
    fn new(state: Directory) -> Arc<Self> {
        Arc::new(Self(Mutex::new(state)))
    }

    fn point(&self, state: Directory) {
        *self.0.lock() = state;
    }
}

#[async_trait]
impl EndpointResolver for Scripted {
    async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError> {
        assert_eq!(
            gear, "types-registry",
            "resolves the registry by its gear name"
        );
        match self.0.lock().clone() {
            Directory::At(uri) => Ok(Some(uri)),
            Directory::Empty => Ok(None),
            Directory::Down => Err(ResolveError::new(gear, "directory unreachable")),
        }
    }
}

/// The tuning the proxy-wiring phase hands a consumer: the process's token source, which
/// counts how often it is consulted.
fn tuning(resolutions: &Arc<AtomicUsize>) -> ClientTuning {
    let counter = Arc::clone(resolutions);
    ClientTuning::default().with_internal_token_provider(Some(InternalTokenProvider::new(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned()))
        },
    )))
}

fn platform() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn tenant() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .bearer_token(TENANT_BEARER.to_owned())
        .build()
        .expect("a tenant context")
}

fn read() -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        items: vec![EntityKey::GtsId(gts::GtsId::try_new(CF_TYPE).unwrap()).into()],
        projection: Projection::Default,
        fresh: false,
    }
}

/// A platform read that must reach the registry resolved: the platform client's cache
/// would otherwise answer from its window while the test moves between endpoints.
fn routed() -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        fresh: true,
        ..read()
    }
}

/// Registers [`CF_TYPE`] on `server` only, through its own local client, and waits.
async fn seed(server: &PlatformServer) {
    let local = LocalClient::new(Arc::clone(&server.service));
    let submitted = PlatformTypesRegistryApi::register_entities(
        &local,
        &platform(),
        IdempotencyKey::new("resolving-seed").unwrap(),
        RegisterEntitiesRequest {
            items: vec![RegisterItem {
                gts_id: gts::GtsId::try_new(CF_TYPE).unwrap(),
                content: json!({
                    "$id": format!("gts://{CF_TYPE}"),
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                }),
                expected_resource_version: None,
                skip_compatibility_check: false,
            }],
            dry_run: false,
            publisher: PublisherContext {
                name: "resolving-client-test".to_owned(),
                version: "1.0.0".parse().unwrap(),
            },
        },
    )
    .await
    .expect("accepted");
    common::await_delivery("the seed completes", || async {
        let operation = local
            .get_operation(&platform(), submitted.operation_id)
            .await
            .expect("read");
        (operation.status() == OperationStatus::Completed).then_some(())
    })
    .await;
}

fn found(response: &types_registry_sdk::BatchGetEntitiesResponse) -> bool {
    matches!(response.0.values().next(), Some(EntityLookup::Found { .. }))
}

fn assert_unavailable(result: Result<impl std::fmt::Debug, CanonicalError>) {
    let error = result.expect_err("nothing to resolve");
    assert!(
        matches!(error, CanonicalError::ServiceUnavailable { .. }),
        "{error:?}"
    );
}

/// The platform client follows the registry from one endpoint to another and is
/// unavailable, not wrong, while the directory has none or cannot answer.
#[tokio::test]
async fn the_platform_resolving_client_follows_the_registry() {
    let first = serve().await;
    let second = serve().await;
    seed(&first).await;
    let directory = Scripted::new(Directory::Empty);
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = PlatformTypesRegistryApiRestResolvingClient::new(
        Arc::clone(&directory) as Arc<dyn EndpointResolver>,
        "types-registry",
        tuning(&resolutions),
    );

    assert_unavailable(client.batch_get_entities(&platform(), routed()).await);

    directory.point(Directory::At(first.base_url.clone()));
    let at_first = client
        .batch_get_entities(&platform(), routed())
        .await
        .expect("served with the process's token");
    assert!(found(&at_first), "{at_first:?}");

    directory.point(Directory::At(second.base_url.clone()));
    let at_second = client
        .batch_get_entities(&platform(), routed())
        .await
        .expect("served by the registry it moved to");
    assert_eq!(
        at_second.0.values().next(),
        Some(&EntityLookup::NotFound),
        "the second registry holds nothing"
    );

    directory.point(Directory::Down);
    assert_unavailable(client.batch_get_entities(&platform(), routed()).await);
    assert_unavailable(client.get_operation(&platform(), Uuid::nil()).await);

    directory.point(Directory::At(first.base_url.clone()));
    assert!(found(
        &client
            .batch_get_entities(&platform(), routed())
            .await
            .expect("recovered")
    ));
    assert_eq!(
        resolutions.load(Ordering::SeqCst),
        3,
        "a fresh token for each of the three calls served, across both rebuilds; none for \
         a call that resolved nothing"
    );
}

/// The tenant client follows the registry the same way and forwards each caller's bearer.
#[tokio::test]
async fn the_tenant_resolving_client_follows_the_registry_with_the_callers_bearer() {
    let first = serve().await;
    let second = serve().await;
    seed(&first).await;
    let directory = Scripted::new(Directory::At(first.base_url.clone()));
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = TypesRegistryApiRestResolvingClient::new(
        Arc::clone(&directory) as Arc<dyn EndpointResolver>,
        "types-registry",
        tuning(&resolutions),
    );

    let served = client
        .batch_get_entities(&tenant(), read())
        .await
        .expect("a validated bearer is served");
    assert!(found(&served), "{served:?}");
    let refused = client
        .batch_get_entities(&SecurityContext::anonymous(), read())
        .await
        .expect_err("the process's token is not a tenant credential");
    assert!(
        matches!(refused, CanonicalError::Unauthenticated { .. }),
        "{refused:?}"
    );

    let mut fresh = read();
    fresh.fresh = true;
    let wrong_bearer = SecurityContext::builder()
        .subject_id(tenant().subject_id())
        .subject_tenant_id(tenant().subject_tenant_id())
        .bearer_token("wrong-bearer".to_owned())
        .build()
        .expect("tenant context");
    assert!(matches!(
        client.batch_get_entities(&wrong_bearer, read()).await,
        Err(CanonicalError::Unauthenticated { .. })
    ));

    directory.point(Directory::At(second.base_url.clone()));
    assert!(found(
        &client
            .batch_get_entities(&tenant(), read())
            .await
            .expect("cache hit")
    ));
    assert_eq!(
        client
            .batch_get_entities(&tenant(), fresh.clone())
            .await
            .expect("served by the registry it moved to")
            .0
            .values()
            .next(),
        Some(&EntityLookup::NotFound),
        "the second registry holds nothing"
    );

    directory.point(Directory::Empty);
    assert_unavailable(
        client
            .list_entities(&tenant(), ListEntitiesRequest::default())
            .await,
    );
    directory.point(Directory::Down);
    assert_unavailable(client.batch_get_entities(&tenant(), fresh).await);

    directory.point(Directory::At(first.base_url.clone()));
    assert!(found(
        &client
            .batch_get_entities(&tenant(), read())
            .await
            .expect("back on the first registry")
    ));
    assert_eq!(
        resolutions.load(Ordering::SeqCst),
        0,
        "a tenant call never consults the token"
    );
}

/// Every platform method goes through the wrapper: a registration is submitted, polled,
/// found by discovery, replayed under its key and deleted on the registry resolved.
#[tokio::test]
async fn the_platform_resolving_client_forwards_every_method() {
    let server = serve().await;
    let directory = Scripted::new(Directory::At(server.base_url.clone()));
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = PlatformTypesRegistryApiRestResolvingClient::new(
        Arc::clone(&directory) as Arc<dyn EndpointResolver>,
        "types-registry",
        tuning(&resolutions),
    );
    let publisher = PublisherContext {
        name: "resolving-client-test".to_owned(),
        version: "1.0.0".parse().unwrap(),
    };
    let registration = || RegisterEntitiesRequest {
        items: vec![RegisterItem {
            gts_id: gts::GtsId::try_new(CF_TYPE).unwrap(),
            content: json!({
                "$id": format!("gts://{CF_TYPE}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
            }),
            expected_resource_version: None,
            skip_compatibility_check: false,
        }],
        dry_run: false,
        publisher: publisher.clone(),
    };

    let submitted = client
        .register_entities(&platform(), key("resolving-register"), registration())
        .await
        .expect("accepted and read back");
    let completed = await_completed(&client, submitted.operation_id).await;
    let Operation::Registration(completed) = completed else {
        panic!("a registration");
    };
    assert!(
        matches!(
            completed.items[0].outcome,
            RegistrationOutcome::Succeeded {
                resource_version: 1
            }
        ),
        "{:?}",
        completed.items
    );

    let page = client
        .list_entities(
            &platform(),
            ListEntitiesRequest {
                filter: EntityFilter {
                    pattern: Some(gts::GtsIdPattern::try_new(CF_TYPE).unwrap()),
                    ..EntityFilter::default()
                },
                ..ListEntitiesRequest::default()
            },
        )
        .await
        .expect("a page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].gts_id.as_ref(), CF_TYPE);

    let replay = client
        .register_entities(&platform(), key("resolving-register"), registration())
        .await
        .expect("replayed");
    assert_eq!(
        replay.operation_id, submitted.operation_id,
        "the same operation"
    );

    let deletion = client
        .delete_entities(
            &platform(),
            key("resolving-delete"),
            DeleteEntitiesRequest {
                items: vec![DeleteItem {
                    key: EntityKey::GtsId(gts::GtsId::try_new(CF_TYPE).unwrap()),
                    expected_resource_version: 1,
                }],
                dry_run: false,
                publisher,
            },
        )
        .await
        .expect("accepted and read back");
    let Operation::Deletion(deleted) = await_completed(&client, deletion.operation_id).await else {
        panic!("a deletion");
    };
    assert!(
        matches!(deleted.items[0].outcome, DeletionOutcome::Succeeded { .. }),
        "{:?}",
        deleted.items
    );
}

fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey::new(text).unwrap()
}

/// Polls `operation_id` through `client` until it completes.
async fn await_completed(
    client: &PlatformTypesRegistryApiRestResolvingClient,
    operation_id: Uuid,
) -> Operation {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let operation = client
            .get_operation(&platform(), operation_id)
            .await
            .expect("polled");
        if operation.status() == OperationStatus::Completed {
            return operation;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the operation completes"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The platform client's cache sits above resolution (T36): a read it answers resolves
/// nothing, one it cannot answer fails as the directory does, and a registration polled
/// to its terminal outcome through the client leaves no stale entry.
#[tokio::test]
async fn the_platform_resolving_client_caches_above_resolution() {
    let server = serve().await;
    seed(&server).await;
    let directory = Scripted::new(Directory::At(server.base_url.clone()));
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = PlatformTypesRegistryApiRestResolvingClient::new(
        Arc::clone(&directory) as Arc<dyn EndpointResolver>,
        "types-registry",
        tuning(&resolutions),
    );
    let cached = read;
    let version = |response: types_registry_sdk::BatchGetEntitiesResponse| match response
        .0
        .into_values()
        .next()
    {
        Some(EntityLookup::Found { entity, .. }) => match entity.origin {
            Some(types_registry_sdk::Origin::Managed {
                resource_version, ..
            }) => resource_version,
            _ => panic!("a managed origin is selected"),
        },
        other => panic!("found, got {other:?}"),
    };

    let warm = client
        .batch_get_entities(&platform(), cached())
        .await
        .expect("warm");
    assert_eq!(version(warm), 1);

    directory.point(Directory::Down);
    let hit = client
        .batch_get_entities(&platform(), cached())
        .await
        .expect("answered by the cache, resolving nothing");
    assert_eq!(version(hit), 1);
    assert_unavailable(client.batch_get_entities(&platform(), routed()).await);

    directory.point(Directory::At(server.base_url.clone()));
    let submitted = client
        .register_entities(
            &platform(),
            key("resolving-cache-update"),
            RegisterEntitiesRequest {
                items: vec![RegisterItem {
                    gts_id: gts::GtsId::try_new(CF_TYPE).unwrap(),
                    content: json!({
                        "$id": format!("gts://{CF_TYPE}"),
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "type": "object",
                        "description": "revised",
                    }),
                    expected_resource_version: Some(1),
                    skip_compatibility_check: false,
                }],
                dry_run: false,
                publisher: PublisherContext {
                    name: "resolving-client-test".to_owned(),
                    version: "1.0.0".parse().unwrap(),
                },
            },
        )
        .await
        .expect("accepted");
    let Operation::Registration(done) = await_completed(&client, submitted.operation_id).await
    else {
        panic!("a registration");
    };
    assert!(
        matches!(
            done.items[0].outcome,
            RegistrationOutcome::Succeeded {
                resource_version: 2
            }
        ),
        "{:?}",
        done.items
    );
    let after = client
        .batch_get_entities(&platform(), cached())
        .await
        .expect("read");
    assert_eq!(
        version(after),
        2,
        "no stale entry after the terminal outcome"
    );
}

/// The platform resolving client keeps the identities its `resolve_*` helpers learned
/// above resolution, and they outlive an observed revision: once the revision drops the
/// snapshot entry, an ordinary read under the mandatory fields needs the registry, while
/// a known identity resolves with the directory down. An unknown one, and every tenant
/// resolution, still need it.
#[tokio::test]
async fn the_platform_resolving_client_resolves_known_identities_without_resolution() {
    use types_registry_sdk::{
        FieldSelection, GtsTypeId, PlatformTypesRegistryApiExt, ReadOptions, TypesRegistryApiExt,
    };

    let server = serve().await;
    seed(&server).await;
    let directory = Scripted::new(Directory::At(server.base_url.clone()));
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = PlatformTypesRegistryApiRestResolvingClient::new(
        Arc::clone(&directory) as Arc<dyn EndpointResolver>,
        "types-registry",
        tuning(&resolutions),
    );
    let tenant_client = TypesRegistryApiRestResolvingClient::new(
        Arc::clone(&directory) as Arc<dyn EndpointResolver>,
        "types-registry",
        ClientTuning::default(),
    );
    let type_id = GtsTypeId::try_new(CF_TYPE).unwrap();
    let uuid = client
        .resolve_type_uuid(&platform(), &type_id)
        .await
        .expect("registered");

    let submitted = client
        .register_entities(
            &platform(),
            key("resolving-identity-revision"),
            RegisterEntitiesRequest {
                items: vec![RegisterItem {
                    gts_id: gts::GtsId::try_new(CF_TYPE).unwrap(),
                    content: json!({
                        "$id": format!("gts://{CF_TYPE}"),
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "type": "object",
                        "description": "revised",
                    }),
                    expected_resource_version: Some(1),
                    skip_compatibility_check: false,
                }],
                dry_run: false,
                publisher: PublisherContext {
                    name: "resolving-client-test".to_owned(),
                    version: "1.0.0".parse().unwrap(),
                },
            },
        )
        .await
        .expect("accepted");
    await_completed(&client, submitted.operation_id).await;

    directory.point(Directory::Down);
    let light = ReadOptions::new().with_projection(FieldSelection::light());
    assert_unavailable(
        client
            .batch_get_type_schemas(&platform(), std::slice::from_ref(&type_id), light)
            .await,
    );
    assert_eq!(
        client
            .resolve_type_uuid(&platform(), &type_id)
            .await
            .expect("known"),
        uuid
    );
    assert_eq!(
        client
            .resolve_type_id(&platform(), uuid)
            .await
            .expect("known"),
        type_id
    );
    let unknown = GtsTypeId::try_new(gts_id!("cf.core.example.unknown.v1~")).unwrap();
    assert_unavailable(client.resolve_type_uuid(&platform(), &unknown).await);
    assert_unavailable(
        TypesRegistryApiExt::resolve_type_uuid(&tenant_client, &tenant(), &type_id).await,
    );
}
