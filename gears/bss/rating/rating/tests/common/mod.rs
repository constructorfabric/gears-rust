//! Shared fixtures of the Rating integration tests: a file-backed SQLite database with every
//! migration applied, the provider doubles, an inbox wired to a recording applier, and REST calls.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    dead_code,
    reason = "test fixtures: each integration test binary uses a different subset"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bss_rating::BssRatingGear;
use bss_rating::api::rest::{self, RestState};
use bss_rating::authz::ServiceGrant;
use bss_rating::domain::audit::OperatorAudit;
use bss_rating::domain::error::DomainError;
use bss_rating::domain::exception::{ExceptionKey, ExceptionRecord, ExceptionRegister};
use bss_rating::domain::inbox::{
    InboundFact, Inbox, InboxApplier, InboxEntry, InboxKey, InboxParts, InboxSource,
    ProducerOrigin, ProducerPolicy,
};
use bss_rating::domain::ports::{Clock, RatingMetricsPort, TenantGrant};
use bss_rating::infra::clock::WallClock;
use bss_rating::infra::storage::repo::audit_repo::OrmAuditRepo;
use bss_rating::infra::storage::repo::exception_repo::OrmExceptionRepo;
use bss_rating::infra::storage::repo::inbox_repo::OrmInboxRepo;
use bss_rating::test_providers::{FakePdp, RecordingMetrics};
use serde_json::Value;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::secure::DbTx;
use toolkit_db::{ConnectOpts, Db};
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

/// A migrated file-backed SQLite database; the directory goes with the value.
pub struct TestDb {
    pub db: Db,
    _dir: tempfile::TempDir,
}

pub async fn migrated_db() -> TestDb {
    let dir = tempfile::Builder::new()
        .prefix("bss-rating-")
        .tempdir()
        .unwrap();
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("rating.sqlite3").display()
    );
    let db = toolkit_db::connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        BssRatingGear::default().migrations(),
    )
    .await
    .unwrap();
    TestDb { db, _dir: dir }
}

/// An applier of `fact` entries that records what it applied.
#[derive(Default)]
pub struct RecordingApplier {
    applied: Mutex<Vec<Uuid>>,
    fail: AtomicBool,
}

impl RecordingApplier {
    pub fn applied(&self) -> Vec<Uuid> {
        self.applied.lock().unwrap().clone()
    }

    /// Makes every later apply fail, as an owning feature that refuses the payload.
    pub fn fail(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl InboxApplier for RecordingApplier {
    fn source(&self) -> InboxSource {
        InboxSource::Fact
    }

    async fn apply(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        entry: &InboxEntry,
    ) -> Result<(), DomainError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(DomainError::InvalidArgument {
                field: "payload",
                detail: "refused by the owning feature".to_owned(),
                reason: "INVALID_REQUEST",
            });
        }
        self.applied.lock().unwrap().push(entry.entry_id);
        Ok(())
    }
}

/// Everything an inbox test needs.
pub struct Fixture {
    pub test_db: TestDb,
    pub pdp: Arc<FakePdp>,
    pub metrics: Arc<RecordingMetrics>,
    pub applier: Arc<RecordingApplier>,
    pub inbox: Inbox,
}

impl Fixture {
    pub fn db(&self) -> &Db {
        &self.test_db.db
    }
}

pub async fn fixture() -> Fixture {
    fixture_with(ProducerPolicy::default()).await
}

pub async fn fixture_with(producers: ProducerPolicy) -> Fixture {
    let test_db = migrated_db().await;
    let pdp = FakePdp::new();
    let metrics = Arc::new(RecordingMetrics::default());
    let applier = Arc::new(RecordingApplier::default());
    let clock: Arc<dyn Clock> = Arc::new(WallClock);
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let mut appliers: BTreeMap<InboxSource, Arc<dyn InboxApplier>> = BTreeMap::new();
    appliers.insert(
        InboxSource::Fact,
        Arc::clone(&applier) as Arc<dyn InboxApplier>,
    );
    let inbox = Inbox::new(InboxParts {
        db: test_db.db.clone(),
        enforcer: pdp.enforcer(),
        repo: Arc::new(OrmInboxRepo),
        appliers,
        producers,
        grant,
        exceptions: exception_register(),
        audit: OperatorAudit::new(Arc::new(OrmAuditRepo), Arc::clone(&clock)),
        metrics: Arc::clone(&metrics) as Arc<dyn RatingMetricsPort>,
        clock,
    });
    Fixture {
        test_db,
        pdp,
        metrics,
        applier,
        inbox,
    }
}

/// The exception register over the `SecureORM` repository and the wall clock.
pub fn exception_register() -> ExceptionRegister {
    ExceptionRegister::new(Arc::new(OrmExceptionRepo), Arc::new(WallClock))
}

/// Records a failure of `key`'s subject in a transaction of its own.
pub async fn record_exception(db: &Db, key: &ExceptionKey, detail: &str) -> ExceptionRecord {
    let register = exception_register();
    let scope = AccessScope::for_tenant(key.tenant_id());
    let (key, detail) = (key.clone(), detail.to_owned());
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move { register.record(tx, &scope, &key, &detail).await })
    })
    .await
    .unwrap()
}

/// A pull-read fact of `tenant`.
pub fn fact(tenant: Uuid, business_id: &str, version: i64, payload: Value) -> InboundFact {
    fact_of(InboxSource::Fact, tenant, business_id, version, payload)
}

pub fn fact_of(
    source: InboxSource,
    tenant: Uuid,
    business_id: &str,
    version: i64,
    payload: Value,
) -> InboundFact {
    InboundFact::new(
        InboxKey::new(tenant, source, business_id, version).unwrap(),
        payload,
        ProducerOrigin::PullRead,
        true,
    )
}

/// A user of `tenant`.
pub fn user(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap()
}

/// The foundation's REST router over the fixture.
pub fn router(fixture: &Fixture) -> Router {
    router_with(fixture, &OpenApiRegistryImpl::new())
}

/// The REST plane over `fixture`, its operations registered in `openapi`.
pub fn router_with(fixture: &Fixture, openapi: &OpenApiRegistryImpl) -> Router {
    rest::router(
        Arc::new(RestState {
            inbox: fixture.inbox.clone(),
        }),
        openapi,
    )
}

/// A JSON `POST` as `ctx`; the status and the JSON body.
pub async fn post(
    router: &Router,
    path: &str,
    ctx: Option<&SecurityContext>,
    body: &Value,
    request_id: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(id) = request_id {
        request = request.header("x-request-id", id);
    }
    if let Some(ctx) = ctx {
        request = request.extension(ctx.clone());
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}
