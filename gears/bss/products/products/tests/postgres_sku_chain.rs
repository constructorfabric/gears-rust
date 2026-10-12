#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Execute the new chain and its concurrency boundaries on `PostgreSQL`.
mod pg_support;

use bss_approval::{ApprovalError, ApproveOutcome, Engine, Policy, Store, SubmitRequest};
use bss_products::{
    domain::{
        approvals::SkuPublish,
        category::NewCategory,
        references::{RefKind, reservation_allowed},
        sku::NewSku,
    },
    infra::{
        broker::EventSink,
        events,
        storage::{
            RepoError, RepoRefusal,
            repo::{self, Fence, HeadWrite},
        },
    },
};
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use time::OffsetDateTime;
use tokio::sync::Barrier;
use toolkit_db::{
    Db, DbError,
    secure::{AccessScope, DBRunner, TxConfig},
};
use uuid::Uuid;

#[derive(Debug)]
enum TxError {
    Db(DbError),
    Repo(RepoError),
    Approval(ApprovalError),
}
impl From<DbError> for TxError {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}
impl From<RepoError> for TxError {
    fn from(e: RepoError) -> Self {
        Self::Repo(e)
    }
}
impl From<ApprovalError> for TxError {
    fn from(e: ApprovalError) -> Self {
        Self::Approval(e)
    }
}
/// The same typed driver extraction the doors pass to `transaction_with_retry`.
fn db_error(error: &TxError) -> Option<&DbErr> {
    match error {
        TxError::Db(DbError::Sea(e))
        | TxError::Repo(RepoError::Driver { source: e, .. })
        | TxError::Approval(ApprovalError::Db(e)) => Some(e),
        _ => None,
    }
}
fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}
fn sql(s: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Postgres, s.into())
}
async fn count(raw: &DatabaseConnection, query: &str) -> i64 {
    raw.query_one_raw(sql(query))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}
fn new_sku(category_id: Uuid, code: &str) -> NewSku {
    NewSku {
        code: code.into(),
        name: code.into(),
        r#type: SkuType::Recurring,
        category_id: Some(category_id),
        description: String::new(),
        sellable: true,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
    }
}
async fn category(runner: &impl DBRunner, scope: &AccessScope, tenant: Uuid, code: &str) -> Uuid {
    repo::insert_category(
        runner,
        scope,
        tenant,
        NewCategory {
            code: code.into(),
            name: code.into(),
            is_default: false,
            sort_order: 0,
        },
        now(),
    )
    .await
    .unwrap()
    .id
}
struct Fixture {
    pg: Pg,
    db: Db,
    scope: AccessScope,
    tenant: Uuid,
    sku: Sku,
}
impl Fixture {
    async fn new() -> Self {
        let pg = Pg::applied().await;
        let db = pg.db().await;
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let conn = db.conn().unwrap();
        let cat = category(&conn, &scope, tenant, "C").await;
        let sku = repo::insert_sku(&conn, &scope, tenant, new_sku(cat, "SKU"), tenant, now())
            .await
            .unwrap();
        Self {
            pg,
            db,
            scope,
            tenant,
            sku,
        }
    }
    async fn publish(&self) {
        repo::set_lifecycle(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            self.sku.id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            now(),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn pg_indexes() {
    let pg = Pg::applied().await;
    let raw = pg.raw().await;
    let rows = raw
        .query_all_raw(sql(
            "SELECT indexname FROM pg_indexes WHERE schemaname = 'bss'",
        ))
        .await
        .unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|r| r.try_get("", "indexname").unwrap())
        .collect();
    for expected in [
        "uq_products_sku_code",
        "uq_products_sku_name",
        "ix_products_sku_version_as_of",
        "uq_products_sku_reference_live",
        "ix_products_approval_unit_queue",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing index {expected}: {names:?}"
        );
    }
    raw.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn unique_code_and_name_report_the_named_indexes() {
    let f = Fixture::new().await;
    let raw = f.pg.raw().await;
    for (code, name, expected) in [
        ("SKU", "different", "uq_products_sku_code"),
        ("different", "SKU", "uq_products_sku_name"),
    ] {
        let error = raw.execute_raw(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO bss.products_sku (id, tenant_id, code, name, type, category_id, lifecycle, created_by, created_at, updated_at) VALUES ($1,$2,$3,$4,'recurring',$5,'draft',$2,now(),now())",
            [Uuid::new_v4().into(), f.tenant.into(), code.into(), name.into(), f.sku.category_id.into()])).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn conditional_lock_has_exactly_one_winner_on_two_connections() {
    let f = Fixture::new().await;
    let first = f.pg.db().await;
    let second = f.pg.db().await;
    let observer = f.pg.raw().await;
    let acquired = Arc::new(tokio::sync::Notify::new());
    let ready = acquired.clone();
    let first_scope = f.scope.clone();
    let second_scope = f.scope.clone();
    let tenant = f.tenant;
    let id = f.sku.id;
    let revision = f.sku.revision;
    let winner = first.transaction_with_retry::<_, TxError, _, _>(
        TxConfig::default(),
        db_error,
        move |tx| {
            let scope = first_scope.clone();
            let ready = ready.clone();
            let observer = observer.clone();
            Box::pin(async move {
                let won =
                    repo::try_lock_sku(tx, &scope, tenant, id, Uuid::new_v4(), revision).await?;
                ready.notify_one();
                pg_support::wait_until_a_backend_blocks(&observer).await;
                Ok(won)
            })
        },
    );
    let loser = async {
        acquired.notified().await;
        second
            .transaction_with_retry::<_, TxError, _, _>(TxConfig::default(), db_error, move |tx| {
                let scope = second_scope.clone();
                Box::pin(async move {
                    Ok(
                        repo::try_lock_sku(tx, &scope, tenant, id, Uuid::new_v4(), revision)
                            .await?,
                    )
                })
            })
            .await
    };
    let (won, lost) = tokio::join!(winner, loser);
    assert_eq!((won.unwrap(), lost.unwrap()), (true, false));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn decision_primary_key_is_per_actor_and_generation() {
    let f = Fixture::new().await;
    let raw = f.pg.raw().await;
    let unit = Uuid::new_v4();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO bss.products_approval_unit (id,tenant_id,kind,ref_type,ref_id,state,quorum_required,generation,submitted_by,submitted_at,snapshot,snapshot_hash,version) VALUES ($1,$2,'sku_publish','sku',$3,'pending',2,1,$2,now(),'{}','hash',1)",
        [unit.into(), f.tenant.into(), f.sku.id.into()])).await.unwrap();
    let vote = |generation| {
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO bss.products_approval_decision (unit_id,tenant_id,actor,generation,decision,note,at,stale) VALUES ($1,$2,$2,$3,'approve',NULL,now(),false)",
            [
                unit.into(),
                f.tenant.into(),
                sea_orm::Value::Int(Some(generation)),
            ],
        )
    };
    raw.execute_raw(vote(1)).await.unwrap();
    let error = raw.execute_raw(vote(1)).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("products_approval_decision_pkey"),
        "{error}"
    );
    raw.execute_raw(vote(2)).await.unwrap();
    assert_eq!(
        count(
            &raw,
            "SELECT count(*) AS n FROM bss.products_approval_decision"
        )
        .await,
        2
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn audit_trigger_refuses_update_and_delete() {
    let f = Fixture::new().await;
    let raw = f.pg.raw().await;
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO bss.products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,written_at,seal_state) VALUES ($1,$2,$2,'sku.create','sku',$3,now(),'unsealed')",
        [Uuid::new_v4().into(), f.tenant.into(), f.sku.id.into()])).await.unwrap();
    for statement in [
        "UPDATE bss.products_audit_log SET action='rewritten'",
        "DELETE FROM bss.products_audit_log",
    ] {
        let error = raw.execute_raw(sql(statement)).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("products_audit_log is append-only"),
            "{error}"
        );
    }
    assert_eq!(
        count(
            &raw,
            "SELECT count(*) AS n FROM bss.products_audit_log WHERE action='sku.create'"
        )
        .await,
        1
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn concurrent_approvers_retry_typed_serialization_and_apply_once() {
    let f = Fixture::new().await;
    toolkit_db::migration_runner::run_migrations_for_testing(
        &f.db,
        toolkit_db::outbox::outbox_migrations_with_prefix(events::OUTBOX_TABLE_PREFIX).unwrap(),
    )
    .await
    .unwrap();
    let handle = toolkit_db::outbox::Outbox::builder(f.db.clone())
        .table_prefix(events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(events::PARTITIONS),
        )
        .leased(events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    let subject = SkuPublish {
        scope: f.scope.clone(),
        tenant_id: f.tenant,
        actor: f.tenant,
        now: now(),
        usage_type: None,
        outbox: events::TxOutbox::new(EventSink::Interim(Arc::clone(handle.outbox()))),
    };
    let store = repo::ProductsApprovalStore {
        scope: f.scope.clone(),
        tenant_id: f.tenant,
    };
    let (s, b, id) = (store.clone(), subject.clone(), f.sku.id);
    let submitted = f
        .db
        .transaction_with_retry::<_, TxError, _, _>(TxConfig::serializable(), db_error, move |tx| {
            let (s, b) = (s.clone(), b.clone());
            Box::pin(async move {
                Ok(Engine::submit(
                    &s,
                    &b,
                    tx,
                    SubmitRequest {
                        tenant_id: b.tenant_id,
                        ref_id: id,
                        item_ids: &[id],
                        actor: b.actor,
                        policy: &Policy {
                            default_quorum: 1,
                            overrides: std::collections::BTreeMap::default(),
                        },
                        common_effective_date: None,
                        note: None,
                        now: b.now,
                    },
                )
                .await?)
            })
        })
        .await
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let retries = Arc::new(AtomicUsize::new(0));
    let approve = |db: Db, actor| {
        let (store, mut subject, barrier, retries) = (
            store.clone(),
            subject.clone(),
            barrier.clone(),
            retries.clone(),
        );
        subject.actor = actor;
        async move {
            let mut first = true;
            db.transaction_with_retry::<_, TxError, _, _>(
                TxConfig::serializable(),
                |e| {
                    if let TxError::Approval(ApprovalError::Db(db)) = e {
                        assert!(db.to_string().contains("could not serialize"), "{db}");
                        retries.fetch_add(1, Ordering::SeqCst);
                    }
                    db_error(e)
                },
                move |tx| {
                    let (store, subject, barrier) =
                        (store.clone(), subject.clone(), barrier.clone());
                    let synchronize = std::mem::take(&mut first);
                    Box::pin(async move {
                        // Pin both serializable snapshots before either CAS can win.
                        store.unit(tx, submitted.unit.id).await?;
                        if synchronize {
                            barrier.wait().await;
                        }
                        Ok(Engine::approve(
                            &store,
                            &subject,
                            tx,
                            submitted.unit.id,
                            actor,
                            1,
                            None,
                            now(),
                        )
                        .await?)
                    })
                },
            )
            .await
        }
    };
    let (one, two) = tokio::join!(
        approve(f.pg.db().await, Uuid::new_v4()),
        approve(f.pg.db().await, Uuid::new_v4())
    );
    assert_eq!(
        [&one, &two]
            .iter()
            .filter(|r| matches!(r, Ok(ApproveOutcome::Applied)))
            .count(),
        1,
        "{one:?} / {two:?}"
    );
    assert_eq!(
        [&one, &two]
            .iter()
            .filter(|r| matches!(
                r,
                Err(TxError::Approval(
                    ApprovalError::Contended | ApprovalError::AlreadyDecided
                ))
            ))
            .count(),
        1,
        "{one:?} / {two:?}"
    );
    assert_eq!(
        retries.load(Ordering::SeqCst),
        1,
        "the engine must preserve the serialization DbErr for the retry loop"
    );
    let raw = f.pg.raw().await;
    assert_eq!(
        count(
            &raw,
            "SELECT count(*) AS n FROM bss.products_approval_decision"
        )
        .await,
        1
    );
    assert_eq!(
        count(&raw, "SELECT count(*) AS n FROM bss.products_sku_version").await,
        1
    );
    assert_eq!(
        repo::find_sku(&f.db.conn().unwrap(), &f.scope, f.tenant, id)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        Lifecycle::Published
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn category_retire_vs_insert_serializes_and_retries() {
    let f = Fixture::new().await;
    let cat = category(&f.db.conn().unwrap(), &f.scope, f.tenant, "EMPTY").await;
    let barrier = Arc::new(Barrier::new(2));
    let retries = Arc::new(AtomicUsize::new(0));
    let race = |db: Db, retire| {
        let (scope, barrier, retries) = (f.scope.clone(), barrier.clone(), retries.clone());
        let tenant = f.tenant;
        async move {
            let mut first = true;
            db.transaction_with_retry::<_, TxError, _, _>(
                TxConfig::serializable(),
                |e| {
                    if let Some(db) = db_error(e) {
                        assert!(db.to_string().contains("could not serialize"), "{db}");
                        retries.fetch_add(1, Ordering::SeqCst);
                    }
                    db_error(e)
                },
                move |tx| {
                    let (scope, barrier) = (scope.clone(), barrier.clone());
                    let synchronize = std::mem::take(&mut first);
                    Box::pin(async move {
                        repo::find_category(tx, &scope, tenant, cat).await?;
                        repo::count_skus_in_category(tx, &scope, tenant, cat).await?;
                        if synchronize {
                            barrier.wait().await;
                        }
                        if retire {
                            Ok(matches!(
                                repo::retire_category_if_unused(tx, &scope, tenant, cat, now())
                                    .await?,
                                Some(HeadWrite::Written(_))
                            ))
                        } else {
                            match repo::insert_sku(
                                tx,
                                &scope,
                                tenant,
                                new_sku(cat, "RACER"),
                                tenant,
                                now(),
                            )
                            .await
                            {
                                Ok(_) => Ok(true),
                                Err(RepoError::Refused(RepoRefusal::CategoryRetired)) => Ok(false),
                                Err(e) => Err(e.into()),
                            }
                        }
                    })
                },
            )
            .await
            .unwrap()
        }
    };
    let (retired, inserted) =
        tokio::join!(race(f.pg.db().await, true), race(f.pg.db().await, false));
    assert_ne!(retired, inserted);
    assert!(retries.load(Ordering::SeqCst) >= 1);
    let conn = f.db.conn().unwrap();
    let state = repo::find_category(&conn, &f.scope, f.tenant, cat)
        .await
        .unwrap()
        .unwrap();
    let count = repo::count_skus_in_category(&conn, &f.scope, f.tenant, cat)
        .await
        .unwrap();
    assert!(
        (state.status == "active" && count == 1) || (state.status == "retired" && count == 0),
        "{} / {count}",
        state.status
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn reserve_vs_both_fences_never_both_commit() {
    for fence in [Fence::Retire, Fence::TypeChange] {
        let f = Fixture::new().await;
        f.publish().await;
        let barrier = Arc::new(Barrier::new(2));
        let retries = Arc::new(AtomicUsize::new(0));
        let race = |db: Db, reserve| {
            let (scope, barrier, retries) = (f.scope.clone(), barrier.clone(), retries.clone());
            let (tenant, id) = (f.tenant, f.sku.id);
            async move {
                let mut first = true;
                db.transaction_with_retry::<_, TxError, _, _>(
                    TxConfig::serializable(),
                    |e| {
                        if let Some(db) = db_error(e) {
                            assert!(db.to_string().contains("could not serialize"), "{db}");
                            retries.fetch_add(1, Ordering::SeqCst);
                        }
                        db_error(e)
                    },
                    move |tx| {
                        let (scope, barrier) = (scope.clone(), barrier.clone());
                        let synchronize = std::mem::take(&mut first);
                        Box::pin(async move {
                            let sku = repo::find_sku(tx, &scope, tenant, id).await?.unwrap();
                            repo::live_references(tx, &scope, tenant, id).await?;
                            if synchronize {
                                barrier.wait().await;
                            }
                            if reserve {
                                if reservation_allowed(
                                    sku.lifecycle,
                                    sku.type_change_pending || sku.retire_pending,
                                )
                                .is_err()
                                {
                                    return Ok(false);
                                }
                                repo::reserve_reference(
                                    tx,
                                    &scope,
                                    tenant,
                                    id,
                                    "pricing",
                                    RefKind::PriceBookEntry,
                                    Uuid::new_v4(),
                                    tenant,
                                    now(),
                                )
                                .await?;
                                Ok(true)
                            } else {
                                Ok(matches!(
                                    repo::fence_sku(
                                        tx,
                                        &scope,
                                        tenant,
                                        id,
                                        fence,
                                        Uuid::new_v4(),
                                        now()
                                    )
                                    .await?,
                                    HeadWrite::Written(_)
                                ))
                            }
                        })
                    },
                )
                .await
                .unwrap()
            }
        };
        let (reserved, fenced) =
            tokio::join!(race(f.pg.db().await, true), race(f.pg.db().await, false));
        assert_ne!(reserved, fenced, "{fence:?}");
        assert!(retries.load(Ordering::SeqCst) >= 1);
        let conn = f.db.conn().unwrap();
        let live = repo::live_references(&conn, &f.scope, f.tenant, f.sku.id)
            .await
            .unwrap();
        let sku = repo::find_sku(&conn, &f.scope, f.tenant, f.sku.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!(sku.type_change_pending || sku.retire_pending) || live.is_empty());
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn reserved_rows_block_until_release_and_tombstones_cannot_confirm() {
    let f = Fixture::new().await;
    f.publish().await;
    let conn = f.db.conn().unwrap();
    let r = repo::reserve_reference(
        &conn,
        &f.scope,
        f.tenant,
        f.sku.id,
        "pricing",
        RefKind::PriceBookEntry,
        Uuid::new_v4(),
        f.tenant,
        now() - time::Duration::days(365),
    )
    .await
    .unwrap();
    for fence in [Fence::Retire, Fence::TypeChange] {
        assert!(matches!(
            repo::fence_sku(
                &conn,
                &f.scope,
                f.tenant,
                f.sku.id,
                fence,
                Uuid::new_v4(),
                now()
            )
            .await
            .unwrap(),
            HeadWrite::Unmatched
        ));
    }
    repo::release_reference(
        &conn,
        &f.scope,
        f.tenant,
        r.id,
        f.tenant,
        None,
        false,
        now(),
    )
    .await
    .unwrap();
    assert!(matches!(
        repo::fence_sku(
            &conn,
            &f.scope,
            f.tenant,
            f.sku.id,
            Fence::Retire,
            Uuid::new_v4(),
            now()
        )
        .await
        .unwrap(),
        HeadWrite::Written(_)
    ));
    assert_eq!(
        repo::confirm_reference(&conn, &f.scope, f.tenant, r.id, now())
            .await
            .unwrap(),
        repo::ConfirmOutcome::Released
    );
    assert_eq!(
        repo::find_reference(&conn, &f.scope, f.tenant, r.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "released"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn sku_versions_refuse_update_and_delete() {
    let f = Fixture::new().await;
    repo::append_version(
        &f.db.conn().unwrap(),
        &f.scope,
        f.tenant,
        f.sku.id,
        1,
        now().date(),
        &bss_products_sdk::models::SkuContent::from(&f.sku),
        now(),
    )
    .await
    .unwrap();
    let raw = f.pg.raw().await;
    for statement in [
        "UPDATE bss.products_sku_version SET content='{}'",
        "DELETE FROM bss.products_sku_version",
    ] {
        let error = raw
            .execute_raw(sql(statement))
            .await
            .expect_err("version mutations must fail");
        assert!(
            error
                .to_string()
                .contains("products_sku_version is append-only"),
            "{error}"
        );
    }
    assert_eq!(
        count(
            &raw,
            "SELECT count(*) AS n FROM bss.products_sku_version WHERE content->>'code'='SKU'"
        )
        .await,
        1
    );
    raw.close().await.unwrap();
}

/// A write answers what it wrote (pricing D-453, which products follows), at the storage level:
/// Postgres keeps whole microseconds, so an instant the engine is handed survives a round trip
/// only cut to its microsecond (`stored_instant`). A publish applied at once (quorum 0) with a cut
/// instant reads back exactly the instants the engine answered; uncut, an instant 789 ns past its
/// microsecond would not. That every door binds through the cut is held by
/// `tests/stored_instant_census.rs`, not by this test, which calls the engine directly.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_submit_answers_the_instants_the_store_keeps() {
    let f = Fixture::new().await;
    toolkit_db::migration_runner::run_migrations_for_testing(
        &f.db,
        toolkit_db::outbox::outbox_migrations_with_prefix(events::OUTBOX_TABLE_PREFIX).unwrap(),
    )
    .await
    .unwrap();
    let handle = toolkit_db::outbox::Outbox::builder(f.db.clone())
        .table_prefix(events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(events::PARTITIONS),
        )
        .leased(events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    let store = repo::ProductsApprovalStore {
        scope: f.scope.clone(),
        tenant_id: f.tenant,
    };
    let fine = {
        let t = OffsetDateTime::now_utc();
        t.replace_nanosecond(t.microsecond() * 1_000 + 789).unwrap()
    };
    let submit = |sku: Uuid, at: OffsetDateTime| {
        let (store, db) = (store.clone(), f.db.clone());
        let subject = SkuPublish {
            scope: f.scope.clone(),
            tenant_id: f.tenant,
            actor: f.tenant,
            now: at,
            usage_type: None,
            outbox: events::TxOutbox::new(EventSink::Interim(Arc::clone(handle.outbox()))),
        };
        async move {
            db.transaction_with_retry::<_, TxError, _, _>(
                TxConfig::serializable(),
                db_error,
                move |tx| {
                    let (s, b) = (store.clone(), subject.clone());
                    Box::pin(async move {
                        Ok(Engine::submit(
                            &s,
                            &b,
                            tx,
                            SubmitRequest {
                                tenant_id: b.tenant_id,
                                ref_id: sku,
                                item_ids: &[sku],
                                actor: b.actor,
                                policy: &Policy {
                                    default_quorum: 0,
                                    overrides: std::collections::BTreeMap::default(),
                                },
                                common_effective_date: None,
                                note: None,
                                now: b.now,
                            },
                        )
                        .await?)
                    })
                },
            )
            .await
            .unwrap()
        }
    };
    let stored = |id: Uuid| {
        let (scope, conn) = (f.scope.clone(), f.db.conn().unwrap());
        async move {
            repo::find_unit(&conn, &scope, f.tenant, id)
                .await
                .unwrap()
                .unwrap()
        }
    };
    let cut = bss_products::infra::storage::stored_instant(fine);
    let submitted = submit(f.sku.id, cut).await;
    assert!(submitted.applied);
    let read = stored(submitted.unit.id).await;
    assert_eq!(
        (read.submitted_at, read.decided_at),
        (submitted.unit.submitted_at, submitted.unit.decided_at),
        "the answer's instants are the stored unit's"
    );
    assert_eq!(read.submitted_at, cut);
    // The premise: Postgres drops the digits past the microsecond.
    let conn = f.db.conn().unwrap();
    let cat = category(&conn, &f.scope, f.tenant, "UNCUT").await;
    let other = repo::insert_sku(
        &conn,
        &f.scope,
        f.tenant,
        new_sku(cat, "UNCUT"),
        f.tenant,
        now(),
    )
    .await
    .unwrap();
    let uncut = submit(other.id, fine).await;
    assert_ne!(
        stored(uncut.unit.id).await.submitted_at,
        uncut.unit.submitted_at,
        "an uncut instant is not what Postgres keeps"
    );
}
