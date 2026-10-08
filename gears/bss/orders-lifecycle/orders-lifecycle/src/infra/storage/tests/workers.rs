//! S2-11: the five Orders-owned workers on real PostgreSQL through their restricted class
//! connections, with the host connection's real session advisory locks.
use super::*;
use crate::domain::audit::{
    AuditTrigger, CheckpointHeader, CheckpointMember, SealedAudit, checkpoint_hash,
};
use crate::infra::maintenance::scope::{
    discover_audit_namespaces, discover_orders, discover_retention_backlog,
};
use crate::infra::maintenance::{
    MaintenanceAuthority, MaintenanceTask, RetentionTable, ServiceActor, TargetScope, TaskGrant,
    lock_target_order,
};
use crate::infra::storage::repo::audit::{self as writer, order_facts};
use crate::infra::workers::audit::{
    CaptureHooks, CaptureOutcome, VerifyCursor, capture_checkpoint, verify_slice,
};
use crate::infra::workers::expiry::{SweepReport, TransitionSweep};
use crate::infra::workers::metrics::{IntegrityFinding, Observation, RecordingMetrics};
use crate::infra::workers::{
    self, PassResult, RecoveryUnavailable, RoleClass, WorkerConnections, WorkerError, WorkerKind,
    WorkerParts, WorkerSettings, WorkerStatus, cleanup, retention,
};
use bss_orders_lifecycle_sdk::catalog::Trigger;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;

fn authority(tasks: &[MaintenanceTask]) -> Arc<MaintenanceAuthority> {
    Arc::new(MaintenanceAuthority::configured(
        ServiceActor::configured(u(900), u(901)).unwrap(),
        tasks.iter().copied(),
    ))
}
fn grant(authority: &MaintenanceAuthority, task: MaintenanceTask) -> TaskGrant<'_> {
    authority.grant(task).unwrap()
}
/// All five class connections through real restricted logins.
async fn class_connections(pg: &Pg) -> anyhow::Result<WorkerConnections> {
    let mut classes = Vec::new();
    for (class, role) in [
        (RoleClass::Discovery, "discovery"),
        (RoleClass::Maintenance, "maintenance"),
        (RoleClass::Retention, "retention"),
        (RoleClass::Verifier, "verifier"),
        (RoleClass::Checkpoint, "checkpoint"),
    ] {
        let (db, _) = pg.role(role).await?;
        classes.push((class, db));
    }
    let connections = WorkerConnections::from_connections(classes);
    connections.attest().await?;
    Ok(connections)
}
fn fast_settings() -> WorkerSettings {
    WorkerSettings {
        cleanup_interval: Duration::from_millis(200),
        retention_interval: Duration::from_millis(200),
        verification_interval: Duration::from_millis(200),
        sweep_interval: Duration::from_millis(200),
        ..WorkerSettings::baseline()
    }
}
fn parts(
    host: Db,
    connections: WorkerConnections,
    authority: Arc<MaintenanceAuthority>,
    settings: WorkerSettings,
    metrics: Arc<RecordingMetrics>,
    hooks: CaptureHooks,
) -> WorkerParts {
    WorkerParts {
        host,
        connections: Arc::new(connections),
        authority,
        settings,
        metrics,
        expiry: Arc::new(workers::expiry::SweepUnavailable),
        auto_void: Arc::new(workers::expiry::SweepUnavailable),
        recovery: Arc::new(RecoveryUnavailable),
        capture_hooks: hooks,
        status: Arc::new(WorkerStatus::default()),
    }
}
fn scope(id: u128) -> AccessScope {
    AccessScope::for_resources(vec![u(id)])
}
/// Committed create in `namespace`: aggregate at counter 1 with its sealed sequence-1 entry.
async fn create_audited_in(db: &Db, id: u128, namespace: u128) -> anyhow::Result<SealedAudit> {
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let mut row = order(id);
            row.resource_tenant_id = u(namespace);
            row.audit_tenant_id = u(namespace);
            let row = repo::insert_order(tx, &scope(id), row).await?;
            let mut locked = repo::LockedOrder::acquire(tx, &scope(id), &row).await?;
            locked.insert_order_version(version(id, 1, None)).await?;
            let facts = locked.audit_facts()?;
            let pending = super::audit::evidence(40).committed_create(Uuid::new_v4(), &facts)?;
            anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
        })
    })
    .await
}
/// A committed draft mutation appended through the sealed writer.
async fn mutate(db: &Db, id: u128) -> anyhow::Result<SealedAudit> {
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let locked = repo::LockedOrder::lock_current(tx, &scope(id), u(id)).await?;
            let mut locked = locked.ok_or_else(|| anyhow::anyhow!("order {id} not visible"))?;
            let mut row = locked.row().clone();
            row.draft_revision += 1;
            let before = order_facts(locked.row())?;
            locked.replace(&scope(id), row).await?;
            let after = locked.audit_facts()?;
            let pending = super::audit::evidence(40).committed_transition(
                Uuid::new_v4(),
                AuditTrigger::Public(Trigger::DraftMutate),
                &before,
                &after,
                None,
            )?;
            anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
        })
    })
    .await
}
async fn namespace_target(db: &Db, namespace: u128) -> anyhow::Result<TargetScope> {
    let authority = authority(&[MaintenanceTask::AuditVerification]);
    let grant = grant(&authority, MaintenanceTask::AuditVerification);
    discover_audit_namespaces(db, &grant, None, 100)
        .await?
        .iter()
        .map(TargetScope::from_discovered_audit_namespace)
        .find(|t| t.audit_namespace() == Some(u(namespace)))
        .ok_or_else(|| anyhow::anyhow!("namespace {namespace} not discovered"))
}
const EVIDENCE_TABLES: [&str; 5] = [
    "bss_orders__transition_audit",
    "bss_orders__order",
    "bss_orders__order_version",
    "bss_orders__audit_checkpoint",
    "bss_orders__audit_checkpoint_member",
];
/// A privileged rewrite: the schema owner bypassing every guard, which the design says local
/// hashes cannot prevent and the worker must detect.
async fn privileged(pg: &Pg, sql: &str) -> anyhow::Result<()> {
    for table in EVIDENCE_TABLES {
        pg.sql(&format!("ALTER TABLE {table} DISABLE TRIGGER ALL"))
            .await?;
    }
    let result = pg.sql(sql).await;
    for table in EVIDENCE_TABLES {
        pg.sql(&format!("ALTER TABLE {table} ENABLE TRIGGER ALL"))
            .await?;
    }
    result
}
async fn snapshot_evidence(pg: &Pg) -> anyhow::Result<()> {
    for table in EVIDENCE_TABLES {
        pg.sql(&format!(
            "DROP TABLE IF EXISTS {table}__backup; CREATE TABLE {table}__backup AS TABLE {table}"
        ))
        .await?;
    }
    Ok(())
}
async fn restore_evidence(pg: &Pg) -> anyhow::Result<()> {
    use std::fmt::Write as _;
    let mut sql = String::new();
    for table in EVIDENCE_TABLES.iter().rev() {
        write!(sql, "DELETE FROM {table};")?;
    }
    for table in EVIDENCE_TABLES {
        write!(sql, "INSERT INTO {table} SELECT * FROM {table}__backup;")?;
    }
    privileged(pg, &sql).await
}
async fn checkpoint_rows(pg: &Pg) -> anyhow::Result<(i64, i64)> {
    Ok((
        pg.scalar("SELECT count(*) AS n FROM bss_orders__audit_checkpoint")
            .await?,
        pg.scalar("SELECT count(*) AS n FROM bss_orders__audit_checkpoint_member")
            .await?,
    ))
}
fn kinds(findings: &[IntegrityFinding]) -> Vec<&'static str> {
    let mut kinds = findings
        .iter()
        .map(IntegrityFinding::kind)
        .collect::<Vec<_>>();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// 01 §3.5 / §6: only expired Preview outcomes, refused audit rows and read-log rows are
/// deleted, in bounded batches under the pass budget; committed audit rows, order-linked
/// outcomes and unexpired rows survive; backlog and oldest overdue age are reported, and a
/// pass that does not run leaves the backlog growing.
#[tokio::test]
async fn retention_pass_purges_only_expired_rows_in_bounded_batches_and_reports_backlog()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited_in(&runtime, 1, 10).await?;
    // Read log: three expired, one live.
    for (id, days) in [(1, 100), (2, 95), (3, 91), (4, 1)] {
        pg.sql(&format!(
            "INSERT INTO bss_orders__read_access_log VALUES('{}',NULL,'{}','{}','user','get','refused','order-not-found',NULL,NULL,now()-interval '{days} days')",
            u(id), u(77), u(40)
        )).await?;
    }
    // Preview outcomes: two expired, one live, one order-linked and old (never purged).
    for (id, order_ref, days) in [
        (1, "NULL".to_owned(), 8),
        (2, "NULL".to_owned(), 9),
        (3, "NULL".to_owned(), 1),
        (4, format!("'{}'", u(1)), 100),
    ] {
        pg.sql(&format!(
            "INSERT INTO bss_orders__gate_outcome(outcome_id,run_id,subject_tenant_id,subject_id,resource_tenant_id,seller_tenant_id,payer_tenant_id,order_id,predicate,has_catalog_selection,verdict,mapping_version,applicability,producer_results,evaluated_at) VALUES('{}','{}','{}','{}','{}','{}','{}',{order_ref},'test',false,'passed','v1','required','[]',now()-interval '{days} days')",
            u(id), u(id), u(10), u(40), u(10), u(20), u(30)
        )).await?;
    }
    // Refused audit: two expired, one live; the committed create of order 1 is 0 days old, so
    // age the committed row too to prove the class, not the age, protects it.
    for (id, days) in [(201, 100), (202, 91), (203, 1)] {
        pg.sql(&format!(
            "INSERT INTO bss_orders__transition_audit(audit_id,hash_version,subject_tenant_id,requested_order_ref,entry_hash,trigger,outcome,actor,actor_class,reason,idempotency_key,created_at) VALUES('{}',3,'{}','{}',decode(repeat('01',32),'hex'),'cancel','refused','{}','user','order-not-found','k{id}',now()-interval '{days} days')",
            u(id), u(10), u(77), u(40)
        )).await?;
    }
    privileged(&pg, "UPDATE bss_orders__transition_audit SET created_at = now() - interval '400 days' WHERE outcome='committed'").await?;
    let connections = class_connections(&pg).await?;
    let retention_db = connections.get(RoleClass::Retention).unwrap().clone();
    let authority = authority(&[MaintenanceTask::RetentionPurge]);
    let grant = grant(&authority, MaintenanceTask::RetentionPurge);
    let metrics = RecordingMetrics::default();
    let cancel = CancellationToken::new();
    // Batch 2, one batch per pass: bounded work, backlog reported.
    let bounded = WorkerSettings {
        retention_batch: 2,
        retention_batches_per_pass: 1,
        ..WorkerSettings::baseline()
    };
    let report = retention::run_pass(&retention_db, &grant, &bounded, &metrics, &cancel).await?;
    let by_store = |table: RetentionTable| {
        report
            .stores
            .iter()
            .find(|s| s.store == table)
            .copied()
            .unwrap()
    };
    let logs = by_store(RetentionTable::ReadAccessLog);
    assert_eq!(
        (
            logs.purged,
            logs.batches,
            logs.backlog,
            logs.budget_exhausted
        ),
        (2, 1, 1, true)
    );
    let overdue = logs.oldest_overdue.unwrap();
    assert!(
        overdue >= Duration::from_hours(23) && overdue <= Duration::from_hours(48),
        "{overdue:?}"
    );
    let preview = by_store(RetentionTable::PreviewDiagnostics);
    assert_eq!((preview.purged, preview.backlog), (2, 0));
    let refused = by_store(RetentionTable::RefusedAudit);
    assert_eq!((refused.purged, refused.backlog), (2, 0));
    assert!(metrics.observations().iter().any(|o| matches!(
        o,
        Observation::RetentionBacklog {
            store: RetentionTable::ReadAccessLog,
            rows: 1,
            oldest_overdue: Some(_)
        }
    )));
    // The second pass drains the backlog; everything else survives.
    let drained = retention::run_pass(
        &retention_db,
        &grant,
        &WorkerSettings::baseline(),
        &metrics,
        &cancel,
    )
    .await?;
    let logs = drained
        .stores
        .iter()
        .find(|s| s.store == RetentionTable::ReadAccessLog)
        .unwrap();
    assert_eq!(
        (logs.purged, logs.backlog, logs.budget_exhausted),
        (1, 0, false)
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__read_access_log")
            .await?,
        1
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__gate_outcome")
            .await?,
        2
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE outcome='refused'")
            .await?,
        1
    );
    assert_eq!(
        pg.scalar(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE outcome='committed'"
        )
        .await?,
        1
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await?,
        0
    );
    // A paused worker: rows keep expiring and the backlog the next pass would report grows.
    for (id, days) in [(5, 200), (6, 150)] {
        pg.sql(&format!(
            "INSERT INTO bss_orders__read_access_log VALUES('{}',NULL,'{}','{}','user','get','refused','order-not-found',NULL,NULL,now()-interval '{days} days')",
            u(id), u(77), u(40)
        )).await?;
    }
    let backlog = discover_retention_backlog(
        &retention_db,
        &grant,
        RetentionTable::ReadAccessLog,
        time::OffsetDateTime::now_utc() - time::Duration::days(90),
    )
    .await?;
    assert_eq!(backlog.rows, 2);
    Ok(())
}

/// DESIGN §3.8 acceptance: two replicas with identical keys; only one acquires; the lock
/// session of the holder is killed while its pass continues on its own data connection; the
/// other replica acquires and runs; the old pass finishes. No row is deleted twice, no live
/// row is deleted, and two concurrent checkpoint contenders on the same snapshot predecessor
/// produce exactly one checkpoint and no fork.
#[tokio::test]
async fn two_replicas_contend_and_lock_session_loss_never_duplicates_effects() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    for id in 1..=3 {
        create_audited_in(&runtime, id, 10).await?;
    }
    for (id, days) in [(1, 100), (2, 95), (3, 1)] {
        pg.sql(&format!(
            "INSERT INTO bss_orders__read_access_log VALUES('{}',NULL,'{}','{}','user','get','refused','order-not-found',NULL,NULL,now()-interval '{days} days')",
            u(id), u(77), u(40)
        )).await?;
    }
    // Replica B: an independent host connection (its own lock session) to the same database.
    let host_b = toolkit_db::connect_db(
        &format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.port
        ),
        toolkit_db::ConnectOpts::default(),
    )
    .await?;
    let authority = authority(&[
        MaintenanceTask::RetentionPurge,
        MaintenanceTask::AuditCheckpoint,
    ]);
    let metrics = Arc::new(RecordingMetrics::default());
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let hooks = CaptureHooks {
        after_snapshot: Some(Arc::clone(&barrier)),
    };
    let connections = class_connections(&pg).await?;
    let parts_a = Arc::new(parts(
        pg.db.clone(),
        connections.clone(),
        Arc::clone(&authority),
        WorkerSettings::baseline(),
        Arc::clone(&metrics),
        hooks.clone(),
    ));
    let parts_b = Arc::new(parts(
        host_b.clone(),
        connections,
        Arc::clone(&authority),
        WorkerSettings::baseline(),
        Arc::clone(&metrics),
        hooks,
    ));
    let cancel = CancellationToken::new();

    // --- Retention: A holds the key, B is contended.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let a = Arc::clone(&parts_a);
    let cancel_a = cancel.clone();
    let pass_a = tokio::spawn(async move {
        let inner = Arc::clone(&a);
        workers::guarded_pass(
            &a,
            WorkerKind::RetentionPurge,
            WorkerKind::RetentionPurge.key(),
            &cancel_a,
            move |cancel| {
                Box::pin(async move {
                    held_tx.send(()).ok();
                    go_rx.await.ok();
                    // The old pass continues on its data connection after its lock session died.
                    let grant = inner
                        .authority
                        .grant(MaintenanceTask::RetentionPurge)
                        .unwrap();
                    let db = inner.connections.get(RoleClass::Retention).unwrap();
                    retention::run_pass(db, &grant, &inner.settings, inner.metrics.as_ref(), cancel)
                        .await
                        .map(|_| ())
                })
            },
        )
        .await
    });
    held_rx.await?;
    let b = Arc::clone(&parts_b);
    let contended = workers::guarded_pass(
        &b,
        WorkerKind::RetentionPurge,
        WorkerKind::RetentionPurge.key(),
        &cancel,
        |_| Box::pin(async { Ok(()) }),
    )
    .await;
    assert_eq!(contended, PassResult::Contended);
    // Kill A's lock session (the one holding the advisory key), keeping A's data connections.
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM pg_locks WHERE locktype='advisory' AND granted")
            .await?,
        1
    );
    pg.sql("SELECT pg_terminate_backend(pid) FROM pg_locks WHERE locktype='advisory' AND granted")
        .await?;
    for _ in 0..100 {
        if pg
            .scalar("SELECT count(*) AS n FROM pg_locks WHERE locktype='advisory' AND granted")
            .await?
            == 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // B now acquires the key and purges.
    let b = Arc::clone(&parts_b);
    let inner = Arc::clone(&b);
    let result_b = workers::guarded_pass(
        &b,
        WorkerKind::RetentionPurge,
        WorkerKind::RetentionPurge.key(),
        &cancel,
        move |cancel| {
            Box::pin(async move {
                let grant = inner
                    .authority
                    .grant(MaintenanceTask::RetentionPurge)
                    .unwrap();
                let db = inner.connections.get(RoleClass::Retention).unwrap();
                retention::run_pass(db, &grant, &inner.settings, inner.metrics.as_ref(), cancel)
                    .await
                    .map(|_| ())
            })
        },
    )
    .await;
    assert_eq!(result_b, PassResult::Completed);
    // The old pass resumes: its conditional deletes find nothing eligible twice.
    go_tx.send(()).ok();
    assert_eq!(pass_a.await?, PassResult::Completed);
    let purged: u64 = metrics
        .observations()
        .iter()
        .filter_map(|o| match o {
            Observation::RetentionBatch {
                store: RetentionTable::ReadAccessLog,
                purged,
            } => Some(*purged),
            _ => None,
        })
        .sum();
    assert_eq!(
        purged, 2,
        "each expired row deleted exactly once across both passes"
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__read_access_log")
            .await?,
        1
    );
    // A reacquires after reconnect: the lock session recovers.
    let again = workers::guarded_pass(
        &parts_a,
        WorkerKind::RetentionPurge,
        WorkerKind::RetentionPurge.key(),
        &cancel,
        |_| Box::pin(async { Ok(()) }),
    )
    .await;
    assert_eq!(again, PassResult::Completed);

    Ok(())
}

/// DESIGN §3.8 / D-100 item 1: two checkpoint contenders (two replicas) capture from the same
/// snapshot predecessor; the `(audit_tenant_id, checkpoint_sequence)` uniqueness rejects the
/// competing append and rolls back every member, so exactly one checkpoint exists and the next
/// capture chains onto it (no fork).
#[tokio::test]
async fn concurrent_checkpoint_contenders_on_one_snapshot_never_fork() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    for id in 1..=3 {
        create_audited_in(&runtime, id, 10).await?;
    }
    let host_b = toolkit_db::connect_db(
        &format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.port
        ),
        toolkit_db::ConnectOpts::default(),
    )
    .await?;
    let authority = authority(&[MaintenanceTask::AuditCheckpoint]);
    let metrics = Arc::new(RecordingMetrics::default());
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let hooks = CaptureHooks {
        after_snapshot: Some(Arc::clone(&barrier)),
    };
    let connections = class_connections(&pg).await?;
    let parts_a = Arc::new(parts(
        pg.db.clone(),
        connections.clone(),
        Arc::clone(&authority),
        WorkerSettings::baseline(),
        Arc::clone(&metrics),
        hooks.clone(),
    ));
    let parts_b = Arc::new(parts(
        host_b,
        connections,
        Arc::clone(&authority),
        WorkerSettings::baseline(),
        Arc::clone(&metrics),
        hooks,
    ));
    let cancel = CancellationToken::new();
    // --- Checkpoint: two contenders hold the same snapshot predecessor; exactly one appends.
    let target =
        namespace_target(parts_a.connections.get(RoleClass::Verifier).unwrap(), 10).await?;
    let (ta, tb) = (target.clone(), target.clone());
    let (ca, cb) = (cancel.clone(), cancel.clone());
    let (pa, pb) = (Arc::clone(&parts_a), Arc::clone(&parts_b));
    let capture_a = tokio::spawn(async move {
        capture_checkpoint(
            pa.connections.get(RoleClass::Checkpoint).unwrap(),
            &ta,
            &pa.capture_hooks,
            &ca,
        )
        .await
    });
    let capture_b = tokio::spawn(async move {
        capture_checkpoint(
            pb.connections.get(RoleClass::Checkpoint).unwrap(),
            &tb,
            &pb.capture_hooks,
            &cb,
        )
        .await
    });
    barrier.wait().await;
    let outcomes = [capture_a.await??, capture_b.await??];
    let appended = outcomes
        .iter()
        .filter(|o| {
            matches!(
                o,
                CaptureOutcome::Appended {
                    sequence: 1,
                    members: 3
                }
            )
        })
        .count();
    let conflicts = outcomes
        .iter()
        .filter(|o| matches!(o, CaptureOutcome::Conflict))
        .count();
    assert_eq!((appended, conflicts), (1, 1), "{outcomes:?}");
    assert_eq!(
        checkpoint_rows(&pg).await?,
        (1, 3),
        "no partial members from the loser"
    );
    // The next capture (no contention) chains onto the winner.
    let hooks_off = CaptureHooks::default();
    let next = capture_checkpoint(
        parts_a.connections.get(RoleClass::Checkpoint).unwrap(),
        &target,
        &hooks_off,
        &cancel,
    )
    .await?;
    assert_eq!(
        next,
        CaptureOutcome::Appended {
            sequence: 2,
            members: 3
        }
    );
    assert_eq!(checkpoint_rows(&pg).await?, (2, 6));
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__audit_checkpoint c2 JOIN bss_orders__audit_checkpoint c1 ON c1.checkpoint_sequence=1 AND c2.checkpoint_sequence=2 AND c2.prev_checkpoint_hash=c1.checkpoint_hash").await?,
        1
    );
    Ok(())
}

/// 01 §3.1 item 6 / §3.8 roster: expired settled markers are swept under the row lock with a
/// fresh database-time recheck; live leases, replaced generations and markers whose D-188
/// execution is unresolved are preserved, the unresolved one is handed to the recovery port,
/// and backlog/oldest ages are reported.
#[tokio::test]
async fn cleanup_sweeps_expired_markers_and_preserves_live_replaced_and_unresolved()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create(&runtime, 1).await?;
    // (a) expired settled refusal → deleted.
    pg.sql(&format!(
        "INSERT INTO bss_orders__transition_audit(audit_id,hash_version,subject_tenant_id,requested_order_ref,entry_hash,trigger,outcome,actor,actor_class,reason,idempotency_key,created_at) VALUES('{}',3,'{}','{}',decode(repeat('01',32),'hex'),'cancel','refused','{}','user','order-not-found','old',now()-interval '2 days')",
        u(101), u(10), u(77), u(40)
    )).await?;
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','old','hash','settled','{}',0,'refused','order-not-found','{}','{{\"formatVersion\":1,\"body\":{{\"reason\":\"order-not-found\"}}}}',now()-interval '2 days',now()-interval '1 day')", u(201), u(101))).await?;
    // (b) retention expired but the in-flight lease is still live → preserved.
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,lease_expires_at,created_at,expires_at) VALUES('create','actor','live','hash','in_flight','{}',0,now()+interval '1 hour',now()-interval '2 days',now()-interval '1 day')", u(202))).await?;
    // (c) expired in-flight marker linked to an unresolved attempt whose lease lapsed.
    runtime
        .transaction_ref_mapped(|tx| {
            Box::pin(async move {
                let target = AccessScope::for_resources(vec![u(1)]);
                let row = repo::find_order(tx, &target, u(1)).await?.unwrap();
                let mut lock = repo::LockedOrder::acquire(tx, &target, &row).await?;
                let mut proposed = row;
                proposed.version_allocation_high_water = 2;
                lock.replace(&target, proposed).await?;
                let mut attempt = super::executions::attempt();
                attempt.idempotency_execution_id = u(203);
                attempt.lease_until =
                    Some(time::OffsetDateTime::now_utc() - time::Duration::hours(1));
                lock.insert_commercial_attempt(attempt).await?;
                let mut registry = super::executions::registry(201);
                registry.execution_id = u(203);
                registry.lease_expires_at =
                    Some(time::OffsetDateTime::now_utc() - time::Duration::hours(1));
                registry.expires_at = time::OffsetDateTime::now_utc() - time::Duration::days(1);
                let principal = AccessScope::single(
                    toolkit_security::access_scope::ScopeConstraint::new(vec![
                        toolkit_security::access_scope::ScopeFilter::eq("principal_scope", "actor"),
                    ]),
                );
                assert!(repo::private::offer_idempotency(tx, &principal, registry).await?);
                anyhow::Ok(())
            })
        })
        .await?;
    let connections = class_connections(&pg).await?;
    let maintenance = connections.get(RoleClass::Maintenance).unwrap().clone();
    let authority = authority(&[MaintenanceTask::IdempotencyCleanup]);
    let grant = grant(&authority, MaintenanceTask::IdempotencyCleanup);
    let metrics = RecordingMetrics::default();
    let cancel = CancellationToken::new();
    // Discover first, then replace one expired generation before the sweep (d).
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','replaced','hash','settled','{}',0,'refused','order-not-found','{}','{{\"formatVersion\":1,\"body\":{{\"reason\":\"order-not-found\"}}}}',now()-interval '2 days',now()-interval '1 day')", u(204), u(101))).await?;
    let discovered = crate::infra::maintenance::scope::discover_expired_idempotency(
        &maintenance,
        &grant,
        time::OffsetDateTime::now_utc(),
        10,
    )
    .await?;
    assert_eq!(discovered.len(), 4);
    pg.sql(&format!("DELETE FROM bss_orders__idempotency WHERE idempotency_key='replaced'; INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','replaced','hash','settled','{}',0,'refused','order-not-found','{}','{{\"formatVersion\":1,\"body\":{{\"reason\":\"order-not-found\"}}}}',now(),now()+interval '1 day')", u(205), u(101))).await?;
    let replaced_target = TargetScope::from_discovered_idempotency(
        discovered
            .iter()
            .find(|d| {
                TargetScope::from_discovered_idempotency(d)
                    .idempotency()
                    .unwrap()
                    .idempotency_key
                    == "replaced"
            })
            .unwrap(),
    );
    let swept = maintenance
        .transaction_ref_mapped(move |tx| {
            Box::pin(
                async move { Ok(repo::private::sweep_expired_marker(tx, &replaced_target).await?) },
            )
        })
        .await
        .map_err(|e: anyhow::Error| e)?;
    assert_eq!(swept, repo::private::SweepOutcome::Replaced);

    let mut cursor = cleanup::CleanupCursor::default();
    let report = cleanup::run_pass(
        &maintenance,
        &grant,
        &WorkerSettings::baseline(),
        &RecoveryUnavailable,
        &metrics,
        &mut cursor,
        &cancel,
    )
    .await?;
    // Short pages: the next pass restarts from the oldest expiry and lease.
    assert_eq!(cursor, cleanup::CleanupCursor::default());
    assert_eq!(
        (
            report.markers.deleted,
            report.markers.live,
            report.markers.unresolved,
            report.markers.replaced,
            report.markers.failed
        ),
        (1, 1, 1, 0, 0),
        "{report:?}"
    );
    assert_eq!(
        (
            report.backlog,
            report.recovery_backlog,
            report.recovery.unavailable
        ),
        (2, 1, 1)
    );
    assert!(report.recovery_oldest.unwrap() >= Duration::from_secs(3500));
    assert!(report.oldest_expired.unwrap() >= Duration::from_secs(86_000));
    let keys = {
        let raw = &pg.raw;
        let rows = raw
            .query_all_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT idempotency_key AS k FROM bss_orders__idempotency ORDER BY idempotency_key",
            ))
            .await?;
        rows.iter()
            .map(|r| r.try_get::<String>("", "k").unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(keys, ["key", "live", "replaced"]);
    let observations = metrics.observations();
    assert!(observations.iter().any(
        |o| matches!(o, Observation::Recovery { backlog: 1, tally, .. } if tally.unavailable == 1)
    ));
    // D-188: the recovery continuation runs before the marker sweep of the same pass.
    let position = |pred: fn(&Observation) -> bool| observations.iter().position(pred).unwrap();
    assert!(
        position(|o| matches!(o, Observation::Recovery { .. }))
            < position(|o| matches!(o, Observation::Cleanup(_)))
    );
    // The DB guard independently refuses the unresolved marker even for a direct privileged delete.
    assert!(
        pg.sql("DELETE FROM bss_orders__idempotency WHERE idempotency_key='key'")
            .await
            .is_err()
    );
    Ok(())
}

/// One privileged tamper/removal case and the findings each phase must raise.
struct Case {
    name: &'static str,
    sql: String,
    /// Expected checkpoint-phase finding kinds; `None` when the daily reconciliation cannot
    /// see the case (the verifier must).
    capture: Option<Vec<&'static str>>,
    verify: Vec<&'static str>,
}

/// D-100 acceptance: reconciliation and checkpoints on real namespaces, then every tamper and
/// removal case. A finding alerts, rolls the snapshot back and never records a new checkpoint;
/// the verifier recomputes content, not stored hashes. Cases the local roll-up cannot evidence
/// are asserted as such.
#[tokio::test]
async fn checkpoints_reconcile_namespaces_and_detect_every_tamper_and_removal_case()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited_in(&runtime, 1, 10).await?;
    mutate(&runtime, 1).await?;
    mutate(&runtime, 1).await?;
    create_audited_in(&runtime, 2, 10).await?;
    create_audited_in(&runtime, 3, 10).await?;
    create(&runtime, 4).await?; // counter 0: in the inventory, not a member
    pg.sql(&format!("UPDATE bss_orders__order SET audit_tenant_id='{}', resource_tenant_id='{}' WHERE order_id='{}'", u(10), u(10), u(4))).await.ok();
    create_audited_in(&runtime, 5, 11).await?; // another namespace: never mixed in
    let connections = class_connections(&pg).await?;
    let checkpoint_db = connections.get(RoleClass::Checkpoint).unwrap().clone();
    let verifier_db = connections.get(RoleClass::Verifier).unwrap().clone();
    let target = namespace_target(&verifier_db, 10).await?;
    let cancel = CancellationToken::new();
    let hooks = CaptureHooks::default();
    let capture = |db: Db, target: TargetScope| {
        let (hooks, cancel) = (hooks.clone(), cancel.clone());
        async move { capture_checkpoint(&db, &target, &hooks, &cancel).await }
    };
    let verify = |db: Db, target: TargetScope, limit: u64| {
        let cancel = cancel.clone();
        async move {
            let mut cursor = VerifyCursor::default();
            verify_slice(&db, &target, &mut cursor, limit, &cancel).await
        }
    };
    assert_eq!(
        capture(checkpoint_db.clone(), target.clone()).await?,
        CaptureOutcome::Appended {
            sequence: 1,
            members: 3
        }
    );
    // Independent recomputation of the stored header from stored members and D-100 encoding.
    let header = repo::audit::latest_checkpoint(&verifier_db.conn()?, &target)
        .await?
        .unwrap();
    let members =
        repo::audit::checkpoint_members_page(&verifier_db.conn()?, &target, 1, None, 500).await?;
    let recomputed = checkpoint_hash(
        &CheckpointHeader {
            format_version: header.format_version,
            audit_tenant_id: header.audit_tenant_id,
            checkpoint_sequence: 1,
            captured_at: header.captured_at,
            member_count: header.member_count,
            prev_checkpoint_hash: header.prev_checkpoint_hash.clone(),
        },
        &members
            .iter()
            .map(|m| CheckpointMember {
                order_id: m.order_id,
                audit_sequence: m.audit_sequence,
                entry_hash: m.entry_hash.clone(),
            })
            .collect::<Vec<_>>(),
    )?;
    assert_eq!(recomputed.to_vec(), header.checkpoint_hash);
    assert_eq!(
        header.prev_checkpoint_hash,
        crate::domain::audit::checkpoint_genesis(u(10)).to_vec()
    );
    assert_eq!(
        members
            .iter()
            .map(|m| (m.order_id, m.audit_sequence))
            .collect::<Vec<_>>(),
        [(u(1), 3), (u(2), 1), (u(3), 1)]
    );
    // Legitimate growth keeps the recorded prefix and chains the checkpoints.
    mutate(&runtime, 2).await?;
    assert_eq!(
        capture(checkpoint_db.clone(), target.clone()).await?,
        CaptureOutcome::Appended {
            sequence: 2,
            members: 3
        }
    );
    let clean = verify(verifier_db.clone(), target.clone(), 500).await?;
    assert_eq!(
        (clean.orders, clean.completed_pass, clean.findings.len()),
        (4, true, 0),
        "{clean:?}"
    );
    // Bounded slices cover every order and complete the pass on a short page.
    let mut cursor = VerifyCursor::default();
    let mut covered = 0;
    let mut slices = 0;
    loop {
        let slice = verify_slice(&verifier_db, &target, &mut cursor, 2, &cancel).await?;
        covered += slice.orders;
        slices += 1;
        assert!(slice.findings.is_empty());
        if slice.completed_pass {
            break;
        }
    }
    assert_eq!((covered, slices), (4, 3));
    assert!(cursor.coverage_age().is_some());
    // An expired refusal purge raises no alarm (refusals are outside the chain).
    pg.sql(&format!(
        "INSERT INTO bss_orders__transition_audit(audit_id,hash_version,subject_tenant_id,requested_order_ref,entry_hash,trigger,outcome,actor,actor_class,reason,idempotency_key,created_at) VALUES('{}',3,'{}','{}',decode(repeat('01',32),'hex'),'cancel','refused','{}','user','order-not-found','purged',now()-interval '100 days')",
        u(300), u(10), u(1), u(40)
    )).await?;
    let retention_db = connections.get(RoleClass::Retention).unwrap().clone();
    let purge_authority = authority(&[MaintenanceTask::RetentionPurge]);
    retention::run_pass(
        &retention_db,
        &grant(&purge_authority, MaintenanceTask::RetentionPurge),
        &WorkerSettings::baseline(),
        &RecordingMetrics::default(),
        &cancel,
    )
    .await?;
    assert!(
        verify(verifier_db.clone(), target.clone(), 500)
            .await?
            .findings
            .is_empty()
    );
    // Namespace 11 has its own checkpoint history, independent of namespace 10.
    let other = namespace_target(&verifier_db, 11).await?;
    assert_eq!(
        capture(checkpoint_db.clone(), other.clone()).await?,
        CaptureOutcome::Appended {
            sequence: 1,
            members: 1
        }
    );

    snapshot_evidence(&pg).await?;
    let baseline = checkpoint_rows(&pg).await?;
    let cases = [
        Case { name: "tail removal, counter intact", sql: format!("DELETE FROM bss_orders__transition_audit WHERE order_id='{}' AND sequence=3", u(1)), capture: Some(vec!["head_missing"]), verify: vec!["chain"] },
        Case { name: "middle removal, counter intact", sql: format!("DELETE FROM bss_orders__transition_audit WHERE order_id='{}' AND sequence=2", u(1)), capture: None, verify: vec!["chain"] },
        Case { name: "full-trail removal, order intact", sql: format!("DELETE FROM bss_orders__transition_audit WHERE order_id='{}'", u(3)), capture: Some(vec!["head_missing"]), verify: vec!["chain"] },
        Case { name: "full-order removal after checkpoint", sql: format!("DELETE FROM bss_orders__transition_audit WHERE order_id='{0}'; DELETE FROM bss_orders__order_version WHERE order_id='{0}'; DELETE FROM bss_orders__order WHERE order_id='{0}'", u(3)), capture: Some(vec!["order_missing"]), verify: vec![] },
        Case { name: "counter rollback below the chain", sql: format!("UPDATE bss_orders__order SET audit_sequence=2 WHERE order_id='{}'", u(1)), capture: Some(vec!["counter_rollback", "overflow"]), verify: vec!["chain", "member_beyond_counter"] },
        Case { name: "counter and tail removed together", sql: format!("DELETE FROM bss_orders__transition_audit WHERE order_id='{0}' AND sequence=3; UPDATE bss_orders__order SET audit_sequence=2 WHERE order_id='{0}'", u(1)), capture: Some(vec!["counter_rollback"]), verify: vec!["member_beyond_counter"] },
        Case { name: "changed content with the old stored digest", sql: format!("UPDATE bss_orders__transition_audit SET actor='00000000-0000-0000-0000-0000000000ee' WHERE order_id='{}' AND sequence=1", u(1)), capture: None, verify: vec!["chain"] },
        Case { name: "member tampering in the latest checkpoint", sql: format!("UPDATE bss_orders__audit_checkpoint_member SET entry_hash=decode(repeat('ab',32),'hex') WHERE checkpoint_sequence=2 AND order_id='{}'", u(1)), capture: Some(vec!["prefix_digest"]), verify: vec!["checkpoint_digest", "member_digest"] },
        Case { name: "member removal from the latest checkpoint", sql: format!("DELETE FROM bss_orders__audit_checkpoint_member WHERE checkpoint_sequence=2 AND order_id='{}'", u(2)), capture: None, verify: vec!["checkpoint_members"] },
        Case { name: "header tampering", sql: "UPDATE bss_orders__audit_checkpoint SET checkpoint_hash=decode(repeat('cd',32),'hex') WHERE audit_tenant_id='00000000-0000-0000-0000-00000000000a' AND checkpoint_sequence=1".to_owned(), capture: None, verify: vec!["checkpoint_digest", "checkpoint_predecessor"] },
        Case { name: "checkpoint removal (history gap)", sql: "DELETE FROM bss_orders__audit_checkpoint_member WHERE checkpoint_sequence=1 AND audit_tenant_id='00000000-0000-0000-0000-00000000000a'; DELETE FROM bss_orders__audit_checkpoint WHERE checkpoint_sequence=1 AND audit_tenant_id='00000000-0000-0000-0000-00000000000a'".to_owned(), capture: None, verify: vec!["checkpoint_sequence"] },
    ];
    for case in cases {
        privileged(&pg, &case.sql).await?;
        if let Some(expected) = &case.capture {
            let outcome = capture(checkpoint_db.clone(), target.clone()).await?;
            let CaptureOutcome::Mismatch(findings) = outcome else {
                panic!("{}: expected a mismatch, got {outcome:?}", case.name)
            };
            assert_eq!(&kinds(&findings), expected, "{}: {findings:?}", case.name);
            assert_eq!(
                checkpoint_rows(&pg).await?,
                baseline,
                "{}: a mismatch never records a checkpoint",
                case.name
            );
        }
        let slice = verify(verifier_db.clone(), target.clone(), 500).await?;
        assert_eq!(
            kinds(&slice.findings),
            case.verify,
            "{}: {:?}",
            case.name,
            slice.findings
        );
        restore_evidence(&pg).await?;
        assert!(
            verify(verifier_db.clone(), target.clone(), 500)
                .await?
                .findings
                .is_empty(),
            "{}: restore",
            case.name
        );
    }
    // Limitation shown, not covered: a whole order lost before its first checkpoint, with its
    // counter, is not independently evidenced by the local roll-ups.
    create_audited_in(&runtime, 6, 10).await?;
    privileged(&pg, &format!("DELETE FROM bss_orders__transition_audit WHERE order_id='{0}'; DELETE FROM bss_orders__order_version WHERE order_id='{0}'; DELETE FROM bss_orders__order WHERE order_id='{0}'", u(6))).await?;
    assert!(
        verify(verifier_db.clone(), target.clone(), 500)
            .await?
            .findings
            .is_empty()
    );
    assert_eq!(
        capture(checkpoint_db.clone(), target.clone()).await?,
        CaptureOutcome::Appended {
            sequence: 3,
            members: 3
        }
    );
    Ok(())
}

/// Startup attestation (DESIGN "database privilege is runtime-owned"): a class connection
/// must hold exactly its role's privileges. A connection on the wrong restricted login lacks a
/// granted statement; a superuser connection holds an ungranted one; an unresolved secret
/// reference never connects. Each refuses startup and names the class, never the DSN.
#[tokio::test]
async fn privilege_census_refuses_misprovisioned_class_connections() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let dsn = |user: &str| {
        SecretString::from(format!(
            "postgres://{user}:fixture@127.0.0.1:{}/postgres",
            pg.port
        ))
    };
    for role in [
        "discovery",
        "maintenance",
        "retention",
        "verifier",
        "checkpoint",
    ] {
        pg.role(role).await?;
    }
    let opts = toolkit_db::ConnectOpts {
        max_conns: Some(2),
        ..toolkit_db::ConnectOpts::default()
    };
    let proper = std::collections::BTreeMap::from([
        (RoleClass::Discovery, dsn("test_discovery")),
        (RoleClass::Maintenance, dsn("test_maintenance")),
        (RoleClass::Retention, dsn("test_retention")),
        (RoleClass::Verifier, dsn("test_verifier")),
        (RoleClass::Checkpoint, dsn("test_checkpoint")),
    ]);
    let connections = WorkerConnections::connect(&proper, opts.clone()).await?;
    assert_eq!(connections.classes().len(), 5);
    // Wrong restricted login: the retention class on the maintenance user lacks DELETE on audit.
    let wrong = std::collections::BTreeMap::from([(RoleClass::Retention, dsn("test_maintenance"))]);
    let error = WorkerConnections::connect(&wrong, opts.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("`retention`") && error.contains("lacks"),
        "{error}"
    );
    assert!(
        !error.contains("fixture"),
        "no credential in the message: {error}"
    );
    // Over-privileged login: the superuser as verifier can delete audit rows.
    let excess = std::collections::BTreeMap::from([(
        RoleClass::Verifier,
        SecretString::from(format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.port
        )),
    )]);
    let error = WorkerConnections::connect(&excess, opts.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("`verifier`") && error.contains("outside"),
        "{error}"
    );
    // Unresolved secret reference.
    let unresolved = std::collections::BTreeMap::from([(
        RoleClass::Discovery,
        SecretString::from("${ORDERS_DISCOVERY_DSN}"),
    )]);
    let error = WorkerConnections::connect(&unresolved, opts)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("unresolved"), "{error}");
    Ok(())
}

/// A sweep body for the expiry slot that performs the infrastructure's locked-row recheck
/// (Stage 5 supplies the TTL policy and the engine entry): a candidate changed between
/// discovery and the aggregate lock is skipped, never widened.
struct RecheckSweep {
    runtime: Db,
    pg_port: u16,
}
#[async_trait::async_trait]
impl TransitionSweep for RecheckSweep {
    async fn sweep(
        &self,
        grant: &TaskGrant<'_>,
        discovery: &Db,
        batch: u64,
        _: &CancellationToken,
    ) -> Result<SweepReport, WorkerError> {
        let later = time::OffsetDateTime::now_utc() + time::Duration::days(1);
        let found = discover_orders(discovery, grant, "draft", later, batch).await?;
        let raw = sea_orm::Database::connect(format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            self.pg_port
        ))
        .await
        .map_err(|e| WorkerError::Store(e.into()))?;
        let mut report = SweepReport {
            candidates: found.len() as u64,
            ..SweepReport::default()
        };
        for (index, candidate) in found.iter().enumerate() {
            if index == 0 {
                // The first candidate changes under us after discovery (a concurrent edit).
                sea_orm::ConnectionTrait::execute_unprepared(&raw, &format!("UPDATE bss_orders__order SET state_entered_at = state_entered_at + interval '1 second' WHERE order_id='{}'", candidate.order_id())).await.map_err(|e| WorkerError::Store(e.into()))?;
            }
            let target = TargetScope::from_discovered_order(candidate);
            let eligible: Result<bool, anyhow::Error> = self
                .runtime
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move { Ok(lock_target_order(tx, &target).await?.is_some()) })
                })
                .await;
            if eligible? {
                report.transitioned += 1;
            } else {
                report.skipped += 1;
            }
        }
        Ok(report)
    }
}

/// The scheduler runs every configured worker under its advisory key and class connection,
/// reports the undelivered expiry body as unavailable, rechecks expiry predicates under the
/// aggregate lock, stops cooperatively on cancellation and releases its keys.
#[tokio::test]
async fn scheduler_runs_configured_workers_cooperatively_and_stops_on_cancel() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited_in(&runtime, 1, 10).await?;
    create(&runtime, 2).await?;
    create(&runtime, 3).await?;
    let authority = authority(&[
        MaintenanceTask::StateExpiry,
        MaintenanceTask::DraftAutoVoid,
        MaintenanceTask::IdempotencyCleanup,
        MaintenanceTask::RetentionPurge,
        MaintenanceTask::AuditVerification,
        MaintenanceTask::AuditCheckpoint,
    ]);
    let metrics = Arc::new(RecordingMetrics::default());
    let mut parts = parts(
        pg.db.clone(),
        class_connections(&pg).await?,
        authority,
        fast_settings(),
        Arc::clone(&metrics),
        CaptureHooks::default(),
    );
    parts.auto_void = Arc::new(RecheckSweep {
        runtime: runtime.clone(),
        pg_port: pg.port,
    });
    let status = Arc::clone(&parts.status);
    let cancel = CancellationToken::new();
    let handles = workers::start(parts, &cancel);
    assert_eq!(
        handles.scheduled(),
        [
            WorkerKind::Expiry,
            WorkerKind::DraftAutoVoid,
            WorkerKind::IdempotencyCleanup,
            WorkerKind::RetentionPurge,
            WorkerKind::Audit
        ]
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let done = status.last(WorkerKind::RetentionPurge) == Some(PassResult::Completed)
                && status.last(WorkerKind::IdempotencyCleanup) == Some(PassResult::Completed)
                && status.last(WorkerKind::Audit) == Some(PassResult::Completed)
                && status.last(WorkerKind::Expiry) == Some(PassResult::Unavailable)
                && status.last(WorkerKind::DraftAutoVoid) == Some(PassResult::Completed);
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    let observations = metrics.observations();
    assert!(observations.iter().any(|o| matches!(o, Observation::Checkpoint { namespace, result: workers::metrics::CheckpointResult::Appended, members: 1, .. } if *namespace == u(10))));
    assert!(observations.iter().any(|o| matches!(o, Observation::Verification { namespace, orders: 3, .. } if *namespace == u(10))));
    assert!(observations.iter().any(|o| matches!(
        o,
        Observation::Sweep {
            worker: WorkerKind::DraftAutoVoid,
            candidates: 3,
            transitioned: 2,
            skipped: 1
        }
    )));
    assert!(observations.iter().any(|o| matches!(
        o,
        Observation::Pass {
            worker: WorkerKind::Expiry,
            result: PassResult::Unavailable
        }
    )));
    assert_eq!(
        checkpoint_rows(&pg).await?.0,
        1,
        "one daily checkpoint, not one per tick"
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), handles.stop()).await?;
    // Every key is released: a fresh single attempt on each succeeds.
    for key in [
        "expiry",
        "draft-auto-void",
        "idempotency-cleanup",
        "retention-purge",
        &WorkerKind::namespace_key(u(10)),
    ] {
        let guard = pg
            .db
            .try_lock(
                workers::LOCK_NAMESPACE,
                key,
                toolkit_db::LockConfig {
                    max_retries: Some(0),
                    ..toolkit_db::LockConfig::default()
                },
            )
            .await?;
        guard.expect("released key").release().await?;
    }
    Ok(())
}

/// Capacity sample for the production demonstration (D-100 item 1; §3.8 "24-hour checkpoint
/// and 30-day full scan"): a namespace larger than one page is checkpointed from one snapshot
/// through bounded pages and fully verified; the measured durations are printed for the
/// report. This is a local sample, not a production capacity proof.
#[tokio::test]
async fn capacity_sample_checkpoint_and_full_verification_stay_bounded_per_page()
-> anyhow::Result<()> {
    const ORDERS: u64 = 520;
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let seeded = std::time::Instant::now();
    for id in 1..=u128::from(ORDERS) {
        create_audited_in(&runtime, id, 10).await?;
        if id % 4 == 0 {
            mutate(&runtime, id).await?;
        }
    }
    let seeding = seeded.elapsed();
    let connections = class_connections(&pg).await?;
    let target = namespace_target(connections.get(RoleClass::Verifier).unwrap(), 10).await?;
    let cancel = CancellationToken::new();
    let started = std::time::Instant::now();
    let outcome = capture_checkpoint(
        connections.get(RoleClass::Checkpoint).unwrap(),
        &target,
        &CaptureHooks::default(),
        &cancel,
    )
    .await?;
    let capture = started.elapsed();
    assert_eq!(
        outcome,
        CaptureOutcome::Appended {
            sequence: 1,
            members: ORDERS
        }
    );
    let started = std::time::Instant::now();
    let outcome = capture_checkpoint(
        connections.get(RoleClass::Checkpoint).unwrap(),
        &target,
        &CaptureHooks::default(),
        &cancel,
    )
    .await?;
    let recapture = started.elapsed();
    assert_eq!(
        outcome,
        CaptureOutcome::Appended {
            sequence: 2,
            members: ORDERS
        }
    );
    let started = std::time::Instant::now();
    let mut cursor = VerifyCursor::default();
    let mut verified = 0;
    loop {
        let slice = verify_slice(
            connections.get(RoleClass::Verifier).unwrap(),
            &target,
            &mut cursor,
            500,
            &cancel,
        )
        .await?;
        assert!(slice.findings.is_empty());
        verified += slice.orders;
        if slice.completed_pass {
            break;
        }
    }
    let verification = started.elapsed();
    assert_eq!(verified, ORDERS);
    assert_eq!(checkpoint_rows(&pg).await?, (2, 2 * i64::try_from(ORDERS)?));
    println!(
        "capacity sample: {ORDERS} orders ({} audit rows) seeded in {} ms; checkpoint 1 {} ms; checkpoint 2 with prefix reconciliation {} ms; full verification {} ms",
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await?,
        seeding.as_millis(),
        capture.as_millis(),
        recapture.as_millis(),
        verification.as_millis()
    );
    assert!(
        capture < Duration::from_secs(60)
            && recapture < Duration::from_secs(60)
            && verification < Duration::from_secs(120)
    );
    Ok(())
}

/// D-100 item 2 ("enumerate orders, not only surviving audit rows, so an empty/deleted trail is
/// a finding"): the audit worker's roster is the union of live orders, checkpoint headers and
/// committed rows. A namespace whose whole committed trail is removed, and one whose only
/// order was removed after a checkpoint recorded it, stay scheduled and raise findings
/// instead of vanishing from the schedule; an empty tenant is checkpointed with no members.
#[tokio::test]
async fn namespaces_stay_scheduled_after_their_whole_trail_or_only_order_is_removed()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited_in(&runtime, 1, 10).await?;
    create_audited_in(&runtime, 5, 11).await?; // a single-order namespace
    create(&runtime, 8).await?; // counter 0: an empty tenant
    privileged(&pg, &format!("UPDATE bss_orders__order SET audit_tenant_id='{0}', resource_tenant_id='{0}' WHERE order_id='{1}'", u(12), u(8))).await?;
    let connections = class_connections(&pg).await?;
    let checkpoint_db = connections.get(RoleClass::Checkpoint).unwrap().clone();
    let verifier_db = connections.get(RoleClass::Verifier).unwrap().clone();
    let verifier_authority = authority(&[MaintenanceTask::AuditVerification]);
    let grant = grant(&verifier_authority, MaintenanceTask::AuditVerification);
    let cancel = CancellationToken::new();
    let listed = |after: Option<Uuid>, limit: u64| {
        let (db, grant) = (verifier_db.clone(), &grant);
        async move {
            anyhow::Ok(
                discover_audit_namespaces(&db, grant, after, limit)
                    .await?
                    .iter()
                    .map(|n| {
                        TargetScope::from_discovered_audit_namespace(n)
                            .audit_namespace()
                            .unwrap()
                    })
                    .collect::<Vec<_>>(),
            )
        }
    };
    assert_eq!(listed(None, 100).await?, [u(10), u(11), u(12)]);
    assert_eq!(listed(None, 1).await?, [u(10)]);
    assert_eq!(listed(Some(u(10)), 1).await?, [u(11)]);
    assert_eq!(listed(Some(u(11)), 100).await?, [u(12)]);
    let hooks = CaptureHooks::default();
    // The empty tenant is checkpointed with no members (D-100 acceptance "empty tenants").
    let empty = namespace_target(&verifier_db, 12).await?;
    assert_eq!(
        capture_checkpoint(&checkpoint_db, &empty, &hooks, &cancel).await?,
        CaptureOutcome::Appended {
            sequence: 1,
            members: 0
        }
    );
    let target = namespace_target(&verifier_db, 11).await?;
    assert_eq!(
        capture_checkpoint(&checkpoint_db, &target, &hooks, &cancel).await?,
        CaptureOutcome::Appended {
            sequence: 1,
            members: 1
        }
    );
    let verify = || {
        let (db, target, cancel) = (verifier_db.clone(), target.clone(), cancel.clone());
        async move {
            let mut cursor = VerifyCursor::default();
            verify_slice(&db, &target, &mut cursor, 500, &cancel).await
        }
    };
    // The whole committed trail of the namespace is removed; its live order still owes a chain.
    privileged(
        &pg,
        &format!(
            "DELETE FROM bss_orders__transition_audit WHERE order_id='{}'",
            u(5)
        ),
    )
    .await?;
    assert_eq!(listed(None, 100).await?, [u(10), u(11), u(12)]);
    let outcome = capture_checkpoint(&checkpoint_db, &target, &hooks, &cancel).await?;
    let CaptureOutcome::Mismatch(findings) = outcome else {
        panic!("expected a mismatch, got {outcome:?}")
    };
    assert_eq!(kinds(&findings), ["head_missing"]);
    assert_eq!(kinds(&verify().await?.findings), ["chain"]);
    assert_eq!(checkpoint_rows(&pg).await?, (2, 1));
    // The order itself is removed too: only the checkpoint remembers the namespace.
    privileged(&pg, &format!("DELETE FROM bss_orders__order_version WHERE order_id='{0}'; DELETE FROM bss_orders__order WHERE order_id='{0}'", u(5))).await?;
    assert_eq!(listed(None, 100).await?, [u(10), u(11), u(12)]);
    let outcome = capture_checkpoint(&checkpoint_db, &target, &hooks, &cancel).await?;
    let CaptureOutcome::Mismatch(findings) = outcome else {
        panic!("expected a mismatch, got {outcome:?}")
    };
    assert_eq!(kinds(&findings), ["order_missing"]);
    assert_eq!(checkpoint_rows(&pg).await?, (2, 1));
    // The scheduler visits the orphaned namespace under its own key and alerts.
    let metrics = Arc::new(RecordingMetrics::default());
    let parts = parts(
        pg.db.clone(),
        connections,
        authority(&[MaintenanceTask::AuditCheckpoint]),
        WorkerSettings {
            checkpoint_interval: Duration::ZERO, // a capture is due now
            ..WorkerSettings::baseline()
        },
        Arc::clone(&metrics),
        CaptureHooks::default(),
    );
    let mut state = crate::infra::workers::audit::AuditWorkerState::default();
    assert_eq!(
        crate::infra::workers::audit::namespace_pass(&parts, &mut state, target, &cancel).await,
        PassResult::Completed
    );
    assert!(metrics.observations().iter().any(|o| matches!(
        o,
        Observation::Finding { namespace, finding: IntegrityFinding::OrderMissing { order_id, recorded: 1 } }
            if *namespace == u(11) && *order_id == u(5)
    )));
    Ok(())
}

/// 01 §3.1 item 6 (bounded 500-row passes): markers the sweep must preserve cannot occupy every
/// slot of every pass. With a batch of two and two preserved live-lease markers ahead of a
/// deletable one, the second pass continues behind them and deletes it; a short page restarts
/// the cycle from the oldest expiry.
#[tokio::test]
async fn cleanup_cursor_reaches_deletable_markers_behind_preserved_ones() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    pg.sql(&format!(
        "INSERT INTO bss_orders__transition_audit(audit_id,hash_version,subject_tenant_id,requested_order_ref,entry_hash,trigger,outcome,actor,actor_class,reason,idempotency_key,created_at) VALUES('{}',3,'{}','{}',decode(repeat('01',32),'hex'),'cancel','refused','{}','user','order-not-found','old',now()-interval '2 days')",
        u(101), u(10), u(77), u(40)
    )).await?;
    for (key, execution, days) in [("p1", 301, 3), ("p2", 302, 2)] {
        pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,lease_expires_at,created_at,expires_at) VALUES('create','actor','{key}','hash','in_flight','{}',0,now()+interval '1 hour',now()-interval '{} days',now()-interval '{days} days')", u(execution), days + 1)).await?;
    }
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','d','hash','settled','{}',0,'refused','order-not-found','{}','{{\"formatVersion\":1,\"body\":{{\"reason\":\"order-not-found\"}}}}',now()-interval '2 days',now()-interval '1 day')", u(303), u(101))).await?;
    let connections = class_connections(&pg).await?;
    let maintenance = connections.get(RoleClass::Maintenance).unwrap().clone();
    let authority = authority(&[MaintenanceTask::IdempotencyCleanup]);
    let grant = grant(&authority, MaintenanceTask::IdempotencyCleanup);
    let metrics = RecordingMetrics::default();
    let cancel = CancellationToken::new();
    let settings = WorkerSettings {
        cleanup_batch: 2,
        ..WorkerSettings::baseline()
    };
    let mut cursor = cleanup::CleanupCursor::default();
    macro_rules! pass {
        () => {
            cleanup::run_pass(
                &maintenance,
                &grant,
                &settings,
                &RecoveryUnavailable,
                &metrics,
                &mut cursor,
                &cancel,
            )
            .await?
        };
    }
    let first = pass!();
    assert_eq!((first.markers.live, first.markers.deleted), (2, 0));
    assert_eq!(first.backlog, 3);
    assert_eq!(
        cursor.markers.map(|c| c.execution_id),
        Some(u(302)),
        "a full page leaves the cursor behind its last marker"
    );
    let second = pass!();
    assert_eq!((second.markers.live, second.markers.deleted), (0, 1));
    assert_eq!(second.backlog, 2);
    assert_eq!(cursor.markers, None, "a short page restarts the cycle");
    let third = pass!();
    assert_eq!((third.markers.live, third.markers.deleted), (2, 0));
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await?,
        2
    );
    Ok(())
}

/// After a checkpoint mismatch the capture is withheld for one interval: the last checkpoint
/// stands, the finding is alerted once per attempt rather than on every tick, the reported age
/// keeps growing, and the capture resumes (and succeeds once the evidence is intact) when the
/// interval elapses.
#[tokio::test]
async fn checkpoint_capture_is_withheld_after_a_mismatch_until_the_interval_elapses()
-> anyhow::Result<()> {
    use crate::infra::workers::audit::{AuditWorkerState, namespace_pass};
    use crate::infra::workers::metrics::CheckpointResult;
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited_in(&runtime, 1, 10).await?;
    let connections = class_connections(&pg).await?;
    let target = namespace_target(connections.get(RoleClass::Verifier).unwrap(), 10).await?;
    let metrics = Arc::new(RecordingMetrics::default());
    let interval = Duration::from_millis(1500);
    let parts = parts(
        pg.db.clone(),
        connections,
        authority(&[MaintenanceTask::AuditCheckpoint]),
        WorkerSettings {
            checkpoint_interval: interval,
            ..WorkerSettings::baseline()
        },
        Arc::clone(&metrics),
        CaptureHooks::default(),
    );
    let cancel = CancellationToken::new();
    let mut state = AuditWorkerState::default();
    let results = |metrics: &RecordingMetrics| {
        metrics
            .observations()
            .iter()
            .filter_map(|o| match o {
                Observation::Checkpoint { result, .. } => Some(*result),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let findings = |metrics: &RecordingMetrics| {
        metrics
            .observations()
            .iter()
            .filter(|o| matches!(o, Observation::Finding { .. }))
            .count()
    };
    macro_rules! run {
        () => {
            namespace_pass(&parts, &mut state, target.clone(), &cancel).await
        };
    }
    assert_eq!(run!(), PassResult::Completed);
    assert_eq!(run!(), PassResult::Completed);
    assert_eq!(
        results(&metrics),
        [CheckpointResult::Appended, CheckpointResult::Current]
    );
    snapshot_evidence(&pg).await?;
    privileged(
        &pg,
        &format!(
            "DELETE FROM bss_orders__transition_audit WHERE order_id='{}'",
            u(1)
        ),
    )
    .await?;
    tokio::time::sleep(interval + Duration::from_millis(100)).await;
    assert_eq!(run!(), PassResult::Completed);
    assert_eq!(run!(), PassResult::Completed);
    assert_eq!(
        results(&metrics)[2..],
        [CheckpointResult::Mismatch, CheckpointResult::Withheld]
    );
    assert_eq!(findings(&metrics), 1, "alerted once, not on every tick");
    assert_eq!(checkpoint_rows(&pg).await?, (1, 1));
    tokio::time::sleep(interval + Duration::from_millis(100)).await;
    assert_eq!(run!(), PassResult::Completed);
    assert_eq!(results(&metrics)[4..], [CheckpointResult::Mismatch]);
    assert_eq!(findings(&metrics), 2);
    restore_evidence(&pg).await?;
    tokio::time::sleep(interval + Duration::from_millis(100)).await;
    assert_eq!(run!(), PassResult::Completed);
    assert_eq!(results(&metrics)[5..], [CheckpointResult::Appended]);
    assert_eq!(checkpoint_rows(&pg).await?, (2, 2));
    Ok(())
}
