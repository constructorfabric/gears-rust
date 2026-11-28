//! A credential per sync and the repository's binding to it
//! (gears-rust#5970, asks 1 and 2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::register_routes;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::local_client::LocalClient;
use github_mirror::domain::ports::credentials::GithubCredentials;
use github_mirror::domain::ports::github::{FetchedRepository, GithubPort};
use github_mirror::domain::repo::{ListingFilter, PageWindow};
use github_mirror::domain::service::{Service, SyncRequest};
use github_mirror_sdk::{CredentialRef, GithubMirrorClientV1, SyncOptions};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";

/// What GitHub shows one token: the repository with a description and an
/// issue title naming who is looking.
fn seen_by(who: &str) -> FetchedRepository {
    let mut fetched = common::fetched_repository();
    fetched.repository.description = Some(format!("seen by {who}"));
    for issue in &mut fetched.issues {
        issue.title = format!("{who}'s issue");
    }
    fetched
}

fn github_seeing(who: &str) -> Arc<dyn GithubPort> {
    Arc::new(common::FakeGithub {
        result: Some(seen_by(who)),
    })
}

/// GitHub as a token that cannot read the repository sees it: not there.
fn github_refusing() -> Arc<dyn GithubPort> {
    Arc::new(common::FakeGithub { result: None })
}

/// The credential store and its clients: one GitHub view per
/// `(tenant, key)`, and a record of every reference asked for.
#[derive(Default)]
struct FakeCredentials {
    clients: HashMap<(Uuid, String), Arc<dyn GithubPort>>,
    asked: Mutex<Vec<CredentialRef>>,
}

impl FakeCredentials {
    fn with(mut self, tenant: Uuid, key: &str, github: Arc<dyn GithubPort>) -> Self {
        self.clients.insert((tenant, key.to_owned()), github);
        self
    }

    fn asked(&self) -> Vec<CredentialRef> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl GithubCredentials for FakeCredentials {
    async fn client_for(
        &self,
        _ctx: &SecurityContext,
        reference: &CredentialRef,
    ) -> Result<Arc<dyn GithubPort>, DomainError> {
        self.asked.lock().unwrap().push(reference.clone());
        self.clients
            .get(&(reference.tenant_id, reference.key.clone()))
            .cloned()
            .ok_or_else(|| DomainError::Validation {
                field: "credential_key".to_owned(),
                message: format!("no secret `{}`", reference.key),
            })
    }
}

/// A service whose gear-wide token sees `gear-wide`, with `credentials`.
async fn service_with(credentials: Arc<FakeCredentials>) -> Arc<Service> {
    Arc::new(
        common::build_service(
            common::inmem_db().await,
            "https://api.github.com",
            github_seeing("gear-wide"),
            common::enforcer(),
            std::time::Duration::from_mins(5),
        )
        .with_credentials(credentials),
    )
}

fn credential(tenant: Uuid, key: &str) -> CredentialRef {
    CredentialRef {
        tenant_id: tenant,
        key: key.to_owned(),
    }
}

fn with_credential(tenant: Uuid, key: &str) -> SyncRequest {
    SyncRequest {
        credential: Some(credential(tenant, key)),
        ..SyncRequest::default()
    }
}

async fn description(service: &Service, ctx: &SecurityContext) -> Option<String> {
    service
        .get_repo(ctx, OWNER, NAME)
        .await
        .expect("the repository must be mirrored")
        .description
}

async fn issue_titles(service: &Service, ctx: &SecurityContext) -> Vec<String> {
    let (page, _) = service
        .list_issues(
            ctx,
            OWNER,
            NAME,
            PageWindow::first(10),
            ListingFilter::default(),
        )
        .await
        .expect("the issues must be readable");
    page.items.into_iter().map(|issue| issue.title).collect()
}

async fn bound_key(service: &Service, ctx: &SecurityContext) -> Option<String> {
    service
        .list_repo_sync_status(ctx, &ODataQuery::default(), None)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|status| status.repo_full_name == format!("{OWNER}/{NAME}"))
        .expect("the repository must have a run status")
        .credential_key
}

async fn mirrored_repositories(service: &Service, ctx: &SecurityContext) -> usize {
    service
        .list_repos(ctx, &ODataQuery::default())
        .await
        .unwrap()
        .items
        .len()
}

#[tokio::test]
async fn two_tenants_sync_one_repository_each_with_its_own_credential() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let credentials = Arc::new(
        FakeCredentials::default()
            .with(a, "github", github_seeing("A"))
            .with(b, "github", github_seeing("B")),
    );
    let service = service_with(Arc::clone(&credentials)).await;
    let client = LocalClient::new(Arc::clone(&service));
    let (ctx_a, ctx_b) = (common::caller_in(a), common::caller_in(b));

    for (ctx, tenant) in [(&ctx_a, a), (&ctx_b, b)] {
        client
            .sync_repository_with(
                ctx,
                OWNER,
                NAME,
                SyncOptions {
                    credential: Some(credential(tenant, "github")),
                },
            )
            .await
            .expect("each tenant's own credential reads the repository");
    }

    assert_eq!(
        description(&service, &ctx_a).await.as_deref(),
        Some("seen by A")
    );
    assert_eq!(
        description(&service, &ctx_b).await.as_deref(),
        Some("seen by B")
    );
    assert_eq!(bound_key(&service, &ctx_a).await.as_deref(), Some("github"));
    assert_eq!(bound_key(&service, &ctx_b).await.as_deref(), Some("github"));
    assert_eq!(
        credentials.asked(),
        vec![credential(a, "github"), credential(b, "github")],
        "each sync reads its own tenant's credential, once"
    );
}

#[tokio::test]
async fn a_credential_that_cannot_read_the_repository_fails_the_sync_and_writes_no_rows() {
    let refused = Uuid::new_v4();
    let credentials =
        Arc::new(FakeCredentials::default().with(refused, "github", github_refusing()));
    let service = service_with(credentials).await;
    let ctx = common::caller_in(refused);

    let err = service
        .sync_now(&ctx, OWNER, NAME, None, with_credential(refused, "github"))
        .await
        .expect_err("a credential GitHub does not show the repository to must fail");

    assert!(matches!(err, DomainError::NotFound), "{err:?}");
    assert_eq!(
        mirrored_repositories(&service, &ctx).await,
        0,
        "no fallback to the gear-wide token, which would have read it"
    );
    assert!(matches!(
        service.get_repo(&ctx, OWNER, NAME).await,
        Err(DomainError::NotFound)
    ));
    assert_eq!(
        bound_key(&service, &ctx).await,
        None,
        "a credential that never read the repository is not bound to it"
    );
}

#[tokio::test]
async fn the_sdk_reports_a_refused_credential_as_not_found() {
    let refused = Uuid::new_v4();
    let credentials =
        Arc::new(FakeCredentials::default().with(refused, "github", github_refusing()));
    let client = LocalClient::new(service_with(credentials).await);

    let err = client
        .sync_repository_with(
            &common::caller_in(refused),
            OWNER,
            NAME,
            SyncOptions {
                credential: Some(credential(refused, "github")),
            },
        )
        .await
        .expect_err("the refusal must reach the SDK caller");

    assert_eq!(err.status_code(), 404, "{err}");
}

#[tokio::test]
async fn reads_are_isolated_per_credential_binding() {
    let (a, b, onlooker) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let credentials = Arc::new(
        FakeCredentials::default()
            .with(a, "github", github_seeing("A"))
            .with(b, "github", github_refusing()),
    );
    let service = service_with(credentials).await;
    let (ctx_a, ctx_b) = (common::caller_in(a), common::caller_in(b));
    service
        .sync_now(&ctx_a, OWNER, NAME, None, with_credential(a, "github"))
        .await
        .expect("A's credential reads the repository");
    service
        .sync_now(&ctx_b, OWNER, NAME, None, with_credential(b, "github"))
        .await
        .expect_err("B's credential cannot");

    assert_eq!(issue_titles(&service, &ctx_a).await, vec!["A's issue"]);
    assert_eq!(mirrored_repositories(&service, &ctx_b).await, 0);
    assert!(matches!(
        service
            .list_issues(
                &ctx_b,
                OWNER,
                NAME,
                PageWindow::first(10),
                ListingFilter::default()
            )
            .await,
        Err(DomainError::NotFound)
    ));
    assert_eq!(
        mirrored_repositories(&service, &common::caller_in(onlooker)).await,
        0
    );
}

#[tokio::test]
async fn a_later_sync_naming_no_credential_keeps_the_bound_one() {
    let tenant = Uuid::new_v4();
    let credentials =
        Arc::new(FakeCredentials::default().with(tenant, "github", github_seeing("A")));
    let service = service_with(Arc::clone(&credentials)).await;
    let ctx = common::caller_in(tenant);
    service
        .sync_now(&ctx, OWNER, NAME, None, with_credential(tenant, "github"))
        .await
        .unwrap();

    service
        .sync_now(&ctx, OWNER, NAME, None, SyncRequest::default())
        .await
        .expect("the bound credential still reads the repository");

    assert_eq!(
        description(&service, &ctx).await.as_deref(),
        Some("seen by A"),
        "the gear-wide token must not read a repository bound to a tenant's credential"
    );
    assert_eq!(
        credentials.asked().len(),
        2,
        "the bound secret is read again"
    );
}

#[tokio::test]
async fn a_queued_sync_and_its_resume_use_the_credential() {
    let tenant = Uuid::new_v4();
    let credentials =
        Arc::new(FakeCredentials::default().with(tenant, "github", github_seeing("A")));
    let service = service_with(Arc::clone(&credentials)).await;
    let mut pump = common::SyncPump::take(&service).await;
    let ctx = common::caller_in(tenant);

    service
        .enqueue_sync_with(&ctx, OWNER, NAME, with_credential(tenant, "github"))
        .await
        .unwrap();
    assert_eq!(pump.drain(&service).await, 1);
    assert_eq!(bound_key(&service, &ctx).await.as_deref(), Some("github"));

    let other = service
        .enqueue_sync_with(&ctx, OWNER, NAME, with_credential(tenant, "other"))
        .await
        .expect_err("a credential the store does not hold is refused before queueing");
    assert!(
        matches!(other, DomainError::Validation { ref field, .. } if field == "credential_key"),
        "{other:?}"
    );
    assert_eq!(pump.drain(&service).await, 0, "nothing was queued for it");

    assert_eq!(
        description(&service, &ctx).await.as_deref(),
        Some("seen by A")
    );
}

#[tokio::test]
async fn a_request_with_another_credential_does_not_join_a_running_sync() {
    let tenant = Uuid::new_v4();
    let credentials = Arc::new(
        FakeCredentials::default()
            .with(tenant, "first", github_seeing("A"))
            .with(tenant, "second", github_seeing("A2")),
    );
    let service = service_with(credentials).await;
    let mut pump = common::SyncPump::take(&service).await;
    let ctx = common::caller_in(tenant);

    let queued = service
        .enqueue_sync_with(&ctx, OWNER, NAME, with_credential(tenant, "first"))
        .await
        .unwrap();
    let joined = service
        .enqueue_sync_with(&ctx, OWNER, NAME, with_credential(tenant, "first"))
        .await
        .unwrap();
    assert_eq!(joined.session_id, queued.session_id, "same terms join");

    let refused = service
        .enqueue_sync_with(&ctx, OWNER, NAME, with_credential(tenant, "second"))
        .await
        .expect_err("another credential must not be told the running sync is its own");
    assert!(matches!(refused, DomainError::Conflict(_)), "{refused:?}");
    assert_eq!(pump.drain(&service).await, 1);
}

#[tokio::test]
async fn a_credential_of_another_tenant_is_refused_without_asking_the_store() {
    let (mine, theirs) = (Uuid::new_v4(), Uuid::new_v4());
    let credentials =
        Arc::new(FakeCredentials::default().with(theirs, "github", github_seeing("them")));
    let service = service_with(Arc::clone(&credentials)).await;
    let ctx = common::caller_in(mine);

    let err = service
        .sync_now(&ctx, OWNER, NAME, None, with_credential(theirs, "github"))
        .await
        .expect_err("a tenant may name only its own credentials");

    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "credential_tenant_id"),
        "{err:?}"
    );
    assert!(credentials.asked().is_empty());
    assert_eq!(mirrored_repositories(&service, &ctx).await, 0);
}

#[tokio::test]
async fn without_a_credential_the_gear_wide_token_syncs_as_before() {
    let credentials = Arc::new(FakeCredentials::default());
    let service = service_with(Arc::clone(&credentials)).await;
    let ctx = common::caller();

    LocalClient::new(Arc::clone(&service))
        .sync_repository(&ctx, OWNER, NAME)
        .await
        .expect("the gear-wide token reads the repository");

    assert_eq!(
        description(&service, &ctx).await.as_deref(),
        Some("seen by gear-wide")
    );
    assert_eq!(bound_key(&service, &ctx).await, None);
    assert!(credentials.asked().is_empty(), "no credential is read");
}

#[tokio::test]
async fn a_named_credential_is_refused_by_a_gear_without_a_credential_store() {
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        github_seeing("gear-wide"),
    );
    let tenant = Uuid::new_v4();
    let ctx = common::caller_in(tenant);

    let err = service
        .sync_now(&ctx, OWNER, NAME, None, with_credential(tenant, "github"))
        .await
        .expect_err("the gear-wide token must not stand in for a named credential");

    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    assert_eq!(mirrored_repositories(&service, &ctx).await, 0);
}

fn post(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn the_rest_sync_route_takes_a_credential_key() {
    let tenant = Uuid::new_v4();
    let credentials =
        Arc::new(FakeCredentials::default().with(tenant, "github", github_seeing("A")));
    let service = service_with(Arc::clone(&credentials)).await;
    let mut pump = common::SyncPump::take(&service).await;
    let ctx = common::caller_in(tenant);
    let router = register_routes(
        Router::new(),
        &OpenApiRegistryImpl::new(),
        Arc::clone(&service),
    )
    .layer(axum::Extension(ctx.clone()));

    let accepted = router
        .clone()
        .oneshot(post(&format!(
            "/github-mirror/v1/repos/{OWNER}/{NAME}/sync?credential_key=github"
        )))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(pump.drain(&service).await, 1);
    assert_eq!(credentials.asked(), vec![credential(tenant, "github")]);
    assert_eq!(
        description(&service, &ctx).await.as_deref(),
        Some("seen by A")
    );

    let foreign = router
        .oneshot(post(&format!(
            "/github-mirror/v1/repos/{OWNER}/{NAME}/sync?credential_key=github&credential_tenant_id={}",
            Uuid::new_v4()
        )))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::BAD_REQUEST);
}
