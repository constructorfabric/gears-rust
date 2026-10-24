//! PostgreSQL tier of the foundation (DESIGN §3.7 "Database profiles for tests"): the migration
//! with its regex checks and partial unique indexes, the exception upsert on its partial index,
//! and the races SQLite's single writer cannot show — two first arrivals of one inbox key and two
//! releases of one entry. Ignored by default; run with `make test-integration LANE=rating-pg`
//! (Docker required).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use bss_rating::BssRatingGear;
use bss_rating::authz::ServiceGrant;
use bss_rating::domain::audit::OperatorAudit;
use bss_rating::domain::digest::Sha256Hex;
use bss_rating::domain::error::DomainError;
use bss_rating::domain::exception::{
    ExceptionKey, ExceptionRecord, ExceptionRetrier, ExceptionSweeper, SWEEP_BATCH, SubjectKind,
};
use bss_rating::domain::inbox::{
    AcceptOutcome, Inbox, InboxApplier, InboxParts, InboxSource, InboxState, ProducerPolicy,
    ReleaseCommand, ReleaseResolution,
};
use bss_rating::domain::operation::{IdempotencyKey, OperationKey, OperationKeys, Settlement};
use bss_rating::domain::ports::{Clock, RatingMetricsPort, TenantGrant};
use bss_rating::infra::clock::WallClock;
use bss_rating::infra::storage::repo::audit_repo::OrmAuditRepo;
use bss_rating::infra::storage::repo::exception_repo::OrmExceptionRepo;
use bss_rating::infra::storage::repo::inbox_repo;
use bss_rating::infra::storage::repo::inbox_repo::OrmInboxRepo;
use bss_rating::infra::storage::repo::operation_repo::OrmOperationRepo;
use bss_rating::test_providers::{FakePdp, RecordingMetrics};
use common::{RecordingApplier, exception_register, fact, record_exception, user};
use serde_json::json;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio_util::sync::CancellationToken;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{ConnectOpts, Db};
use toolkit_security::AccessScope;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0xA);

struct Pg {
    url: String,
    db: Db,
    applier: Arc<RecordingApplier>,
    inbox: Inbox,
    _container: ContainerAsync<testcontainers_modules::postgres::Postgres>,
}

async fn pg() -> Pg {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = toolkit_db::connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(8),
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
    let pdp = FakePdp::new();
    let applier = Arc::new(RecordingApplier::default());
    let clock: Arc<dyn Clock> = Arc::new(WallClock);
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let mut appliers: BTreeMap<InboxSource, Arc<dyn InboxApplier>> = BTreeMap::new();
    appliers.insert(
        InboxSource::Fact,
        Arc::clone(&applier) as Arc<dyn InboxApplier>,
    );
    let inbox = Inbox::new(InboxParts {
        db: db.clone(),
        enforcer: pdp.enforcer(),
        repo: Arc::new(OrmInboxRepo),
        appliers,
        producers: ProducerPolicy::default(),
        grant,
        exceptions: exception_register(),
        audit: OperatorAudit::new(Arc::new(OrmAuditRepo), Arc::clone(&clock)),
        metrics: Arc::new(RecordingMetrics::default()) as Arc<dyn RatingMetricsPort>,
        clock,
    });
    Pg {
        url,
        db,
        applier,
        inbox,
        _container: container,
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_two_first_arrivals_of_one_key_store_one_primary_entry() {
    let pg = pg().await;
    let (a, b) = tokio::join!(
        pg.inbox
            .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"}))),
        pg.inbox
            .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "2"}))),
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    let applied = outcomes
        .iter()
        .filter(|o| matches!(o, AcceptOutcome::Applied(_)))
        .count();
    let quarantined = outcomes
        .iter()
        .filter(|o| matches!(o, AcceptOutcome::Quarantined(_)))
        .count();
    assert_eq!((applied, quarantined), (1, 1), "{outcomes:?}");
    assert_eq!(pg.applier.applied().len(), 1);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_one_fact_twice_concurrently_has_one_effect() {
    let pg = pg().await;
    let (a, b) = tokio::join!(
        pg.inbox
            .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"}))),
        pg.inbox
            .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"}))),
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert!(
        outcomes
            .iter()
            .any(|o| matches!(o, AcceptOutcome::Applied(_)))
    );
    assert!(
        outcomes
            .iter()
            .any(|o| matches!(o, AcceptOutcome::Duplicate(_)))
    );
    assert_eq!(pg.applier.applied().len(), 1);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_two_releases_of_one_entry_commit_once() {
    let pg = pg().await;
    pg.inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(conflict) = pg
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "2"})))
        .await
        .unwrap()
    else {
        panic!("expected a quarantine");
    };
    let command =
        |resolution| ReleaseCommand::new(conflict.entry_id, resolution, "r", None).unwrap();
    let (ctx_a, ctx_b) = (user(TENANT), user(TENANT));
    let (a, b) = tokio::join!(
        pg.inbox.release(&ctx_a, command(ReleaseResolution::Apply)),
        pg.inbox
            .release(&ctx_b, command(ReleaseResolution::Discard)),
    );
    let results = [a, b];
    let committed = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(committed, 1, "{results:?}");
    assert!(results.iter().any(|r| matches!(
        r,
        Err(DomainError::Concurrent(_) | DomainError::NotQuarantined { .. })
    )));
    let conn = pg.db.conn().unwrap();
    let stored = inbox_repo::find_by_id(&conn, &AccessScope::for_tenant(TENANT), conflict.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(stored.state, InboxState::Quarantined);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_concurrent_failures_of_one_subject_open_one_exception() {
    let pg = pg().await;
    let key = ExceptionKey::new(
        TENANT,
        SubjectKind::Child,
        "child-1",
        "awaiting_attribution",
    )
    .unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (db, key) = (pg.db.clone(), key.clone());
        tasks.push(tokio::spawn(async move {
            record_exception(&db, &key, "d").await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap().exception_id);
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "one open exception per subject and reason");
    let conn = pg.db.conn().unwrap();
    let open = bss_rating::infra::storage::repo::exception_repo::find_open(&conn, &scope, &key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(open.attempts, 8);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_operation_keys_replay_and_refuse() {
    let pg = pg().await;
    let keys = OperationKeys::new(Arc::new(OrmOperationRepo), Arc::new(WallClock));
    let key = OperationKey {
        tenant_id: TENANT,
        actor: Uuid::from_u128(9),
        idempotency_key: IdempotencyKey::parse("k-1").unwrap(),
    };
    let settle = |digest: Sha256Hex| {
        let (keys, key, db) = (keys.clone(), key.clone(), pg.db.clone());
        async move {
            db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    keys.settle(tx, &AccessScope::for_tenant(TENANT), &key, &digest)
                        .await
                })
            })
            .await
        }
    };
    let digest = Sha256Hex::of_json(&json!({"n": 1}));
    {
        let (keys, key, digest) = (keys.clone(), key.clone(), digest.clone());
        pg.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(TENANT);
                    let Settlement::Fresh(pending) = keys.settle(tx, &scope, &key, &digest).await?
                    else {
                        panic!("a first request settles fresh");
                    };
                    keys.record(tx, &scope, pending, Uuid::from_u128(1), json!({"run": 1}))
                        .await
                })
            })
            .await
            .unwrap();
    }
    assert!(matches!(
        settle(digest).await.unwrap(),
        Settlement::Replay(stored) if stored.response == json!({"run": 1})
    ));
    assert_eq!(
        settle(Sha256Hex::of_json(&json!({"n": 2}))).await,
        Err(DomainError::IdempotencyKeyReused {
            key: "k-1".to_owned()
        })
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_the_schema_refuses_a_malformed_digest() {
    let pg = pg().await;
    let conn = pg.db.conn().unwrap();
    let mut entry = bss_rating::domain::inbox::InboxEntry::arriving(
        &fact(TENANT, "fact-1", 1, json!({})),
        time::OffsetDateTime::now_utc(),
        None,
    );
    // Bypass the domain type's validation to prove the database check holds on its own.
    entry.payload_digest = Sha256Hex::parse(&"a".repeat(64)).unwrap();
    inbox_repo::insert(&conn, &AccessScope::for_tenant(TENANT), &entry)
        .await
        .unwrap();
    let raw = sea_orm::Database::connect(&pg.url).await.unwrap();
    // Sixty-four characters, so only the hex pattern can refuse it, not a length check.
    let not_hex = "g".repeat(64);
    let refused = sea_orm::ConnectionTrait::execute_unprepared(
        &raw,
        &format!("UPDATE bss_rating__inbox SET payload_digest = '{not_hex}'"),
    )
    .await
    .expect_err("a non-hex digest is refused");
    assert!(
        refused.to_string().contains("ck_bss_rating__inbox__digest"),
        "{refused}"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_one_sweep_visits_every_open_exception_once_across_pages() {
    // The sweeper's keyset reads (`DISTINCT tenant_id`, `ORDER BY`, `LIMIT`, `> after`) on
    // PostgreSQL's uuid ordering: more tenants than one discovery page, and one tenant with more
    // exceptions than one page.
    let pg = pg().await;
    let page = usize::try_from(SWEEP_BATCH).unwrap();
    let busy = Uuid::from_u128(0xFFFF);
    let mut keys: Vec<ExceptionKey> = (1..=page)
        .map(|n| {
            let tenant = Uuid::from_u128(0x1_0000 + u128::try_from(n).unwrap());
            ExceptionKey::new(tenant, SubjectKind::Child, "child", "awaiting_attribution").unwrap()
        })
        .collect();
    keys.extend((0..=page).map(|n| {
        ExceptionKey::new(
            busy,
            SubjectKind::Child,
            format!("child-{n}"),
            "awaiting_attribution",
        )
        .unwrap()
    }));
    let register = exception_register();
    let expected: Vec<Uuid> = {
        let keys = keys.clone();
        pg.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let mut ids = Vec::with_capacity(keys.len());
                    for key in &keys {
                        let scope = AccessScope::for_tenant(key.tenant_id());
                        ids.push(register.record(tx, &scope, key, "d").await?.exception_id);
                    }
                    Ok::<_, DomainError>(ids)
                })
            })
            .await
            .unwrap()
    };
    let retrier = Arc::new(VisitingRetrier::default());
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(FakePdp::new().enforcer()));
    let report = ExceptionSweeper::new(
        vec![Arc::clone(&retrier) as Arc<dyn ExceptionRetrier>],
        grant,
        Arc::new(OrmExceptionRepo),
    )
    .sweep_once(&pg.db, &CancellationToken::new())
    .await
    .unwrap();
    assert_eq!(report.tenants, page + 1);
    let mut visited = retrier.visited.lock().unwrap().clone();
    visited.sort_unstable();
    let mut expected = expected;
    expected.sort_unstable();
    assert_eq!(visited, expected, "each open exception exactly once");
}

/// A retrier that records every exception it is handed.
#[derive(Default)]
struct VisitingRetrier {
    visited: std::sync::Mutex<Vec<Uuid>>,
}

#[async_trait::async_trait]
impl ExceptionRetrier for VisitingRetrier {
    fn subject_kind(&self) -> SubjectKind {
        SubjectKind::Child
    }

    fn retries(&self, _reason_code: &str) -> bool {
        true
    }

    async fn retry(
        &self,
        _db: &Db,
        _scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError> {
        self.visited.lock().unwrap().push(exception.exception_id);
        Ok(())
    }
}
