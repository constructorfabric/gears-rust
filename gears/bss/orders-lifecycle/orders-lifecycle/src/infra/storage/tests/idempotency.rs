//! S2-05: the authoritative idempotency gate and D-188/D-198 durable execution ownership on
//! real PostgreSQL, through the restricted runtime role.
use super::*;
use crate::domain::idempotency::{
    DraftRevisionInput, ExecutionOwner, Fingerprint, FingerprintAxes, FingerprintInput,
    FingerprintTarget, LeaseDuration, PrincipalScope, RegistryKey, RegistryOperation, Settlement,
    StoredResponse,
};
use crate::infra::storage::repo::TransactionRunner;
use crate::infra::storage::repo::idempotency::{
    self as reg, AttemptInputs, ClaimOrigin, ControlInputs, Durability, DurableExecution,
    ExecutionLink, ExecutionTerminal, Frozen, Gate, GateRequest, GateTarget, Probe, Recorded,
    RegistryError, Replay, ReplayOutcome,
};
use crate::infra::storage::scoped::PrivateScope;
use bss_orders_lifecycle_sdk::catalog::{Reason, Trigger};
use bss_orders_lifecycle_sdk::models::IdempotencyKey;
use sea_orm::EntityTrait;
use serde_json::{Value, json};
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::{
    AccessScope, SecurityContext,
    access_scope::{ScopeConstraint, ScopeFilter},
};

const LEASE_SECONDS: u32 = 30;

fn lease() -> LeaseDuration {
    LeaseDuration::from_seconds(LEASE_SECONDS).unwrap()
}
fn principal(subject: u128) -> PrincipalScope {
    PrincipalScope::from_context(
        &SecurityContext::builder()
            .subject_id(u(subject))
            .subject_tenant_id(u(10))
            .subject_type("user")
            .build()
            .unwrap(),
    )
    .unwrap()
}
fn key(op: Trigger, subject: u128, text: &str) -> RegistryKey {
    RegistryKey::new(
        RegistryOperation::Trigger(op),
        principal(subject),
        IdempotencyKey::try_from(text.to_owned()).unwrap(),
    )
}
/// The public catalog operation that issues each trigger used here.
fn catalog_operation(op: Trigger) -> &'static str {
    match op {
        Trigger::Create => "create",
        Trigger::DraftMutate => "patch_order",
        Trigger::Submit => "submit",
        Trigger::Amendment => "amend",
        Trigger::Cancel => "cancel",
        Trigger::Hold => "hold",
        Trigger::BeginFulfillment => "begin_fulfillment",
        other => panic!("no catalog operation mapped for {other:?}"),
    }
}
fn fp(op: Trigger, target: Option<u128>, doc: &Value) -> Fingerprint {
    FingerprintInput {
        operation: catalog_operation(op),
        trigger: op,
        target: target.map_or(FingerprintTarget::Create, |id| FingerprintTarget::Order {
            order_id: u(id),
            expected_version: 1,
        }),
        axes: FingerprintAxes {
            seller_tenant_id: u(20),
            resource_tenant_id: u(10),
            payer_tenant_id: u(30),
        },
        draft_revision: DraftRevisionInput::NotApplicable,
        contribution: doc,
    }
    .fingerprint()
    .unwrap()
}
fn response(status: u16, body: Value) -> StoredResponse {
    StoredResponse::new(status, body, std::collections::BTreeMap::new(), None).unwrap()
}
fn private_scope(k: &RegistryKey) -> PrivateScope {
    PrivateScope::for_principal(k.principal().as_str())
}
fn request<'r>(
    k: &'r RegistryKey,
    f: &'r Fingerprint,
    durability: Durability,
    presented: Option<&'r ExecutionOwner>,
) -> GateRequest<'r> {
    GateRequest {
        key: k,
        fingerprint: f,
        lease: lease(),
        durability,
        presented,
    }
}

/// Observable gate result, detached from the transaction.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Replay(Replay),
    Mismatch,
    Still,
    Owned(ClaimOrigin, Uuid),
}
fn seen<T: TransactionRunner>(gate: &Gate<'_, T>) -> Seen {
    match gate {
        Gate::Replay(r) => Seen::Replay(r.clone()),
        Gate::Mismatch => Seen::Mismatch,
        Gate::StillProcessing => Seen::Still,
        Gate::Owned(o) => Seen::Owned(o.origin(), o.record().execution_id),
    }
}

async fn lock<T: TransactionRunner>(tx: &T, id: u128) -> anyhow::Result<repo::LockedOrder<'_, T>> {
    let scope = AccessScope::for_resources(vec![u(id)]);
    let row = repo::find_order(tx, &scope, u(id)).await?.unwrap();
    Ok(repo::LockedOrder::acquire(tx, &scope, &row).await?)
}
fn refusal_scope(order: Option<u128>) -> AccessScope {
    order.map_or_else(
        || {
            AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
                crate::gts::permissions::properties::SUBJECT_TENANT_ID,
                u(10),
            )]))
        },
        |id| AccessScope::for_resources(vec![u(id)]),
    )
}
/// A refusal audit row (resolved for an order, unresolved for create).
fn refusal_row(order: Option<u128>, trigger: &str, audit: u128) -> entity::transition_audit::Model {
    let mut row = super::constraints::audit(audit);
    row.trigger = trigger.into();
    row.reason = "not-admissible".into();
    row.requested_order_ref = order.map(u);
    if let Some(id) = order {
        row.order_id = Some(u(id));
        row.audit_tenant_id = Some(u(10));
        row.resource_tenant_id = Some(u(10));
        row.from_state = Some("draft".into());
        row.to_state = Some("draft".into());
        row.version = Some(1);
    }
    row
}
fn refusal(audit: u128) -> Settlement {
    Settlement::Refused {
        reason: Reason::NotAdmissible,
        audit_id: u(audit),
        response: response(409, json!({"reason": "not-admissible", "audit": u(audit)})),
    }
}
async fn settle_refused<T: TransactionRunner>(
    tx: &T,
    gate: Gate<'_, T>,
    order: Option<u128>,
    trigger: &str,
    audit: u128,
) -> anyhow::Result<entity::idempotency::Model> {
    let Gate::Owned(owned) = gate else {
        anyhow::bail!("not owned: {gate:?}");
    };
    repo::private::insert_transition_audit(
        tx,
        &refusal_scope(order),
        refusal_row(order, trigger, audit),
    )
    .await?;
    Ok(owned.settle(refusal(audit)).await?)
}
/// Committed audit appended under the locked aggregate (advances its audit sequence).
async fn committed_audit<T: TransactionRunner>(
    locked: &mut repo::LockedOrder<'_, T>,
    tx: &T,
    trigger: &str,
    audit: u128,
) -> anyhow::Result<()> {
    let current = locked.row().audit_sequence;
    // Fixture orders from `create` carry no create audit; a later entry is never sequence 1.
    let sequence = if trigger == "create" {
        current + 1
    } else {
        current.max(1) + 1
    };
    let id = locked.row().order_id;
    let scope = AccessScope::for_resources(vec![id]);
    // Raw unsealed fixture rows position the counter directly; production advances it only
    // through the sealed writer.
    locked.set_audit_sequence_for_raw_fixture(sequence).await?;
    let mut row = super::constraints::audit(audit);
    row.order_id = Some(id);
    row.requested_order_ref = (trigger != "create").then_some(id);
    row.audit_tenant_id = Some(u(10));
    row.resource_tenant_id = Some(u(10));
    row.sequence = Some(sequence);
    row.prev_hash = Some(vec![0; 32]);
    row.from_state = (trigger != "create").then(|| "draft".into());
    row.to_state = Some("draft".into());
    row.trigger = trigger.into();
    row.reason = trigger.into();
    row.outcome = "committed".into();
    row.version = Some(1);
    repo::private::insert_transition_audit(tx, &scope, row).await?;
    Ok(())
}
/// The admitted create body: aggregate, empty version 1 and committed create audit.
async fn create_in<T: TransactionRunner>(tx: &T, id: u128, audit: u128) -> anyhow::Result<()> {
    let scope = AccessScope::for_resources(vec![u(id)]);
    let row = repo::insert_order(tx, &scope, order(id)).await?;
    let mut locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
    locked.insert_order_version(version(id, 1, None)).await?;
    committed_audit(&mut locked, tx, "create", audit).await
}
/// Every registry row of the test principals, read through the secure API (superuser).
async fn registry(pg: &Pg) -> Vec<entity::idempotency::Model> {
    let conn = pg.db.conn().unwrap();
    let mut rows = vec![];
    for subject in [40, 41] {
        let scope = PrivateScope::for_principal(principal(subject).as_str());
        rows.extend(
            entity::idempotency::Entity::find()
                .secure()
                .scope_with(scope.access_scope())
                .all(&conn)
                .await
                .unwrap(),
        );
    }
    rows.sort_by(|a, b| {
        (&a.idempotency_key, &a.principal_scope).cmp(&(&b.idempotency_key, &b.principal_scope))
    });
    rows
}
async fn count(pg: &Pg, table: &str) -> i64 {
    pg.scalar(&format!("SELECT count(*) AS n FROM bss_orders__{table}"))
        .await
        .unwrap()
}
/// Wait until some session is blocked on a lock (the concurrent contender is really waiting).
async fn await_lock_wait(pg: &Pg) -> anyhow::Result<()> {
    for _ in 0..400 {
        if pg
            .scalar("SELECT count(*) AS n FROM pg_stat_activity WHERE wait_event_type='Lock'")
            .await?
            > 0
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("the contender never blocked on the registry lock")
}
/// Seed one registry row directly (fault/state injection, superuser).
async fn seed_marker(
    pg: &Pg,
    k: &RegistryKey,
    order: Option<u128>,
    fingerprint: &str,
    execution: u128,
    state: &str,
) -> anyhow::Result<()> {
    let order = order.map_or("NULL".to_owned(), |id| format!("'{}'", u(id)));
    let values = state.replace("$EXEC", &u(execution).to_string());
    pg.sql(&format!(
        "INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,order_id,request_fingerprint,status,execution_id,fencing_generation,lease_expires_at,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('{}','{}','{}',{order},'{fingerprint}',{values})",
        k.operation().token(),
        k.principal().as_str(),
        k.key_text(),
    ))
    .await
}
/// Settled/in-flight column values; `$EXEC` is the execution UUID placeholder.
fn in_flight(lease: &str, created: &str, expires: &str) -> String {
    format!("'in_flight','$EXEC',0,{lease},NULL,NULL,NULL,NULL,{created},{expires}")
}
fn settled_refused(audit: u128, created: &str, expires: &str) -> String {
    format!(
        "'settled','$EXEC',0,NULL,'refused','not-admissible','{}','{{\"formatVersion\":1,\"status\":409,\"body\":{{\"reason\":\"not-admissible\"}}}}',{created},{expires}",
        u(audit)
    )
}
async fn seed_refusal_audit(pg: &Pg, order: Option<u128>, trigger: &str, audit: u128) {
    super::constraints::write(pg, refusal_row(order, trigger, audit))
        .await
        .unwrap();
}

/// Claim stamps fresh DB time and the operation's absolute window; replay and reclaim never
/// extend it. Workflow replays after day one; ordinary keys expire after 24 hours. The D-201
/// internal rebuild keeps the workflow window (D-203).
#[tokio::test]
async fn claims_stamp_db_time_and_operation_windows_and_never_extend_them() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let doc = json!({});
    for (operation, fp_trigger, k_text, window_hours, audit) in [
        (
            RegistryOperation::Trigger(Trigger::DraftMutate),
            Trigger::DraftMutate,
            "ordinary",
            24,
            524u128,
        ),
        (
            RegistryOperation::Trigger(Trigger::BeginFulfillment),
            Trigger::BeginFulfillment,
            "workflow",
            24 * 30,
            525,
        ),
        (
            RegistryOperation::ReplaceFulfillmentGrant,
            Trigger::BeginFulfillment,
            "rebuild",
            24 * 30,
            526,
        ),
    ] {
        let k = RegistryKey::new(
            operation,
            principal(40),
            IdempotencyKey::try_from(k_text.to_owned()).unwrap(),
        );
        let f = fp(fp_trigger, Some(1), &doc);
        let token = operation;
        let (k2, f2) = (k.clone(), f.clone());
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&k2),
                    &request(&k2, &f2, Durability::Ordinary, None),
                )
                .await?;
                settle_refused(tx, gate, Some(1), &token.token(), audit).await?;
                anyhow::Ok(())
            })
        })
        .await?;
        let row = registry(&pg)
            .await
            .into_iter()
            .find(|r| r.idempotency_key == k_text)
            .unwrap();
        assert_eq!(
            row.expires_at - row.created_at,
            time::Duration::hours(window_hours)
        );
        let drift = pg
            .scalar(&format!(
                "SELECT abs(extract(epoch FROM clock_timestamp()-'{}'::timestamptz))::bigint AS n",
                row.created_at
            ))
            .await?;
        assert!(drift < 60, "created_at is fresh database time");
        // A replay is a pure read: the stored row is byte-identical afterwards.
        let replay = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 1).await?;
                    let gate = reg::resolve(
                        GateTarget::Order(&locked),
                        &private_scope(&k),
                        &request(&k, &f, Durability::Ordinary, None),
                    )
                    .await?;
                    anyhow::Ok(seen(&gate))
                })
            })
            .await?;
        assert!(matches!(replay, Seen::Replay(_)));
        assert!(registry(&pg).await.contains(&row));
    }

    // Reclaim of a matching expired lease replaces only the lease deadline.
    let k = key(Trigger::DraftMutate, 40, "reclaim");
    let f = fp(Trigger::DraftMutate, Some(1), &doc);
    seed_marker(
        &pg,
        &k,
        Some(1),
        f.as_str(),
        700,
        &in_flight(
            "now()-interval '1 second'",
            "now()-interval '1 hour'",
            "now()+interval '23 hours'",
        ),
    )
    .await?;
    let before = registry(&pg)
        .await
        .into_iter()
        .find(|r| r.idempotency_key == "reclaim")
        .unwrap();
    let (k2, f2) = (k.clone(), f.clone());
    let origin = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&k2),
                    &request(&k2, &f2, Durability::Ordinary, None),
                )
                .await?;
                let s = seen(&gate);
                settle_refused(tx, gate, Some(1), "draft-mutate", 601).await?;
                anyhow::Ok(s)
            })
        })
        .await?;
    assert_eq!(origin, Seen::Owned(ClaimOrigin::Reclaimed, u(700)));
    let after = registry(&pg)
        .await
        .into_iter()
        .find(|r| r.idempotency_key == "reclaim")
        .unwrap();
    assert_eq!(
        (after.created_at, after.expires_at),
        (before.created_at, before.expires_at)
    );
    assert_eq!(after.execution_id, before.execution_id);

    // Workflow-class receipt replays on day two; an ordinary 25-hour-old key is a new execution.
    let wf = key(Trigger::BeginFulfillment, 40, "day-two");
    let wf_fp = fp(Trigger::BeginFulfillment, Some(1), &doc);
    seed_refusal_audit(&pg, Some(1), "begin-fulfillment", 602).await;
    seed_marker(
        &pg,
        &wf,
        Some(1),
        wf_fp.as_str(),
        701,
        &settled_refused(602, "now()-interval '2 days'", "now()+interval '28 days'"),
    )
    .await?;
    let old = key(Trigger::DraftMutate, 40, "day-two");
    seed_refusal_audit(&pg, Some(1), "draft-mutate", 603).await;
    seed_marker(
        &pg,
        &old,
        Some(1),
        f.as_str(),
        702,
        &settled_refused(603, "now()-interval '25 hours'", "now()-interval '1 hour'"),
    )
    .await?;
    let results = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let a = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&wf),
                    &request(&wf, &wf_fp, Durability::Ordinary, None),
                )
                .await?;
                let a = seen(&a);
                let b = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&old),
                    &request(&old, &f, Durability::Ordinary, None),
                )
                .await?;
                let b_seen = seen(&b);
                settle_refused(tx, b, Some(1), "draft-mutate", 604).await?;
                anyhow::Ok((a, b_seen))
            })
        })
        .await?;
    assert!(matches!(results.0, Seen::Replay(_)));
    // Cleanup honours the persisted workflow deadline: the day-two receipt cannot be removed.
    assert!(
        pg.sql("DELETE FROM bss_orders__idempotency WHERE operation='begin-fulfillment' AND idempotency_key='day-two'")
            .await
            .is_err()
    );
    let Seen::Owned(ClaimOrigin::ReplacedExpired, fresh) = results.1 else {
        panic!("{results:?}")
    };
    assert_ne!(
        fresh,
        u(702),
        "reused key text never adopts the old execution"
    );
    Ok(())
}

/// Concurrent same-principal/key creates: one owner, one aggregate/version/audit; the loser
/// waits on the uniqueness check, re-reads the winner and replays it. If the winner rolls back
/// instead, the waiting contender becomes the owner of its own insert.
#[tokio::test]
async fn concurrent_same_key_creates_have_one_owner_and_one_effect() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let doc = json!({});
    for commit_winner in [true, false] {
        let text = if commit_winner { "commit" } else { "rollback" };
        let k = key(Trigger::Create, 40, text);
        let f = fp(Trigger::Create, None, &doc);
        let order_id: u128 = if commit_winner { 501 } else { 502 };
        let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel::<()>();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (ka, fa, dba) = (k.clone(), f.clone(), db.clone());
        let winner = tokio::spawn(async move {
            dba.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let gate = reg::resolve(GateTarget::Create(tx), &private_scope(&ka), &request(&ka, &fa, Durability::Ordinary, None)).await?;
                    let Gate::Owned(owned) = gate else { anyhow::bail!("winner did not own") };
                    assert_eq!(owned.origin(), ClaimOrigin::Inserted);
                    claimed_tx.send(()).ok();
                    go_rx.await.ok();
                    if !commit_winner {
                        anyhow::bail!("injected failure after claim");
                    }
                    create_in(tx, order_id, 800).await?;
                    owned
                        .settle(Settlement::Success {
                            order_id: Some(u(order_id)),
                            audit_id: u(800),
                            response: response(201, json!({"orderId": u(order_id), "orderNumber": format!("O-{order_id}")})),
                        })
                        .await?;
                    anyhow::Ok(())
                })
            })
            .await
        });
        claimed_rx.await?;
        let (kb, fb, dbb) = (k.clone(), f.clone(), db.clone());
        let loser = tokio::spawn(async move {
            dbb.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let gate = reg::resolve(
                        GateTarget::Create(tx),
                        &private_scope(&kb),
                        &request(&kb, &fb, Durability::Ordinary, None),
                    )
                    .await?;
                    let s = seen(&gate);
                    if let Gate::Owned(_) = gate {
                        settle_refused(tx, gate, None, "create", 801).await?;
                    }
                    anyhow::Ok(s)
                })
            })
            .await
        });
        await_lock_wait(&pg).await?;
        go_tx.send(()).ok();
        let winner = winner.await?;
        let loser = loser.await??;
        if commit_winner {
            winner?;
            let Seen::Replay(replay) = loser else {
                panic!("{loser:?}")
            };
            assert_eq!(replay.outcome, ReplayOutcome::Success);
            assert_eq!(replay.order_id, Some(u(501)));
            assert_eq!(replay.response.body["orderNumber"], "O-501");
            assert_eq!(count(&pg, "order").await, 1);
            assert_eq!(count(&pg, "order_version").await, 1);
            assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE outcome='committed'").await?, 1);
        } else {
            assert!(winner.is_err());
            assert!(
                matches!(loser, Seen::Owned(ClaimOrigin::Inserted, _)),
                "{loser:?}"
            );
            assert_eq!(
                count(&pg, "order").await,
                1,
                "the rolled-back winner created nothing"
            );
        }
    }
    Ok(())
}

/// Keys are partitioned by principal; one key binds one target; a different fingerprint is a
/// mismatch against every retained state and never changes the winner.
#[tokio::test]
async fn principals_targets_and_mismatch_preserve_every_winner() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    create(&db, 2).await?;
    let doc = json!({});
    // Documented limitation: two principals, same key text and request → two creates.
    for (subject, order_id, audit) in [(40u128, 511u128, 811u128), (41, 512, 812)] {
        let k = key(Trigger::Create, subject, "shared");
        let f = fp(Trigger::Create, None, &doc);
        let origin = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let gate = reg::resolve(
                        GateTarget::Create(tx),
                        &private_scope(&k),
                        &request(&k, &f, Durability::Ordinary, None),
                    )
                    .await?;
                    let Gate::Owned(owned) = gate else {
                        anyhow::bail!("{gate:?}")
                    };
                    let origin = owned.origin();
                    create_in(tx, order_id, audit).await?;
                    owned
                        .settle(Settlement::Success {
                            order_id: Some(u(order_id)),
                            audit_id: u(audit),
                            response: response(201, json!({"orderId": u(order_id)})),
                        })
                        .await?;
                    anyhow::Ok(origin)
                })
            })
            .await?;
        assert_eq!(origin, ClaimOrigin::Inserted);
    }
    assert_eq!(
        pg.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__order WHERE order_id IN ('{}','{}')",
            u(511),
            u(512)
        ))
        .await?,
        2,
        "cross-principal create duplication is the disclosed limitation"
    );
    // Another principal can neither read nor address the first principal's record.
    let foreign = PrivateScope::for_principal(principal(41).as_str());
    assert!(
        repo::private::find_idempotency(
            &db.conn()?,
            foreign.access_scope(),
            "create",
            principal(40).as_str(),
            "shared"
        )
        .await?
        .is_none()
    );

    // Retained states for principal 40 on order 1.
    let mutate = |text: &str| key(Trigger::DraftMutate, 40, text);
    let f1 = fp(Trigger::DraftMutate, Some(1), &doc);
    seed_refusal_audit(&pg, Some(1), "draft-mutate", 820).await;
    seed_marker(
        &pg,
        &mutate("settled"),
        Some(1),
        f1.as_str(),
        720,
        &settled_refused(820, "now()", "now()+interval '1 day'"),
    )
    .await?;
    seed_marker(
        &pg,
        &mutate("live"),
        Some(1),
        f1.as_str(),
        721,
        &in_flight("now()+interval '1 hour'", "now()", "now()+interval '1 day'"),
    )
    .await?;
    seed_marker(
        &pg,
        &mutate("lease-expired"),
        Some(1),
        f1.as_str(),
        722,
        &in_flight(
            "now()-interval '1 second'",
            "now()",
            "now()+interval '1 day'",
        ),
    )
    .await?;
    seed_marker(
        &pg,
        &mutate("past-window-live-lease"),
        Some(1),
        f1.as_str(),
        723,
        &in_flight(
            "now()+interval '1 hour'",
            "now()-interval '2 days'",
            "now()-interval '1 day'",
        ),
    )
    .await?;
    let before = registry(&pg).await;
    let different = fp(Trigger::DraftMutate, Some(1), &json!({"changed": "yes"}));
    let other_target = fp(Trigger::DraftMutate, Some(2), &doc);
    for text in ["settled", "live", "lease-expired", "past-window-live-lease"] {
        for (target, f) in [(1u128, different.clone()), (2, other_target.clone())] {
            let k = mutate(text);
            let got = db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let locked = lock(tx, target).await?;
                        let gate = reg::resolve(
                            GateTarget::Order(&locked),
                            &private_scope(&k),
                            &request(&k, &f, Durability::Ordinary, None),
                        )
                        .await?;
                        anyhow::Ok(seen(&gate))
                    })
                })
                .await?;
            assert_eq!(got, Seen::Mismatch, "{text} target {target}");
        }
    }
    assert_eq!(registry(&pg).await, before, "no mismatch changes a winner");
    // Matching fingerprints: settled replays, live lease is still-processing.
    for (text, expected_replay) in [
        ("settled", true),
        ("live", false),
        ("past-window-live-lease", false),
    ] {
        let (k, f) = (mutate(text), f1.clone());
        let got = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 1).await?;
                    let gate = reg::resolve(
                        GateTarget::Order(&locked),
                        &private_scope(&k),
                        &request(&k, &f, Durability::Ordinary, None),
                    )
                    .await?;
                    anyhow::Ok(seen(&gate))
                })
            })
            .await?;
        assert_eq!(matches!(got, Seen::Replay(_)), expected_replay, "{text}");
        assert_eq!(matches!(got, Seen::Still), !expected_replay, "{text}");
    }
    assert_eq!(registry(&pg).await, before);
    Ok(())
}

/// A matching expired lease has exactly one reclaimer: the second waits on the row lock (lease
/// expiry never bypasses it), then re-reads with fresh DB time and replays the settlement.
/// A reclaim whose transaction fails (injected audit failure) restores the prior deadline.
#[tokio::test]
async fn expired_lease_has_one_reclaimer_and_failed_reclaim_rolls_back() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let doc = json!({});
    let k = key(Trigger::Cancel, 40, "recover");
    let f = fp(Trigger::Cancel, Some(1), &doc);
    seed_marker(
        &pg,
        &k,
        Some(1),
        f.as_str(),
        730,
        &in_flight(
            "now()-interval '1 second'",
            "now()",
            "now()+interval '1 day'",
        ),
    )
    .await?;
    let seeded = registry(&pg).await;

    // Crash after reclaim but before commit: the deadline update rolls back.
    let (kc, fc) = (k.clone(), f.clone());
    let crashed = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kc),
                    &request(&kc, &fc, Durability::Ordinary, None),
                )
                .await?;
                assert!(matches!(
                    seen(&gate),
                    Seen::Owned(ClaimOrigin::Reclaimed, _)
                ));
                // Audit failure (unknown reason token violates the audit contract) aborts all.
                let mut bad = refusal_row(Some(1), "cancel", 830);
                bad.actor_class = "nobody".into();
                repo::private::insert_transition_audit(tx, &refusal_scope(Some(1)), bad).await?;
                anyhow::Ok(())
            })
        })
        .await;
    assert!(crashed.is_err());
    assert_eq!(
        registry(&pg).await,
        seeded,
        "failed reclaim restored the prior deadline"
    );

    let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let (ka, fa, dba) = (k.clone(), f.clone(), db.clone());
    let first = tokio::spawn(async move {
        dba.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&ka),
                    &request(&ka, &fa, Durability::Ordinary, None),
                )
                .await?;
                let s = seen(&gate);
                claimed_tx.send(()).ok();
                // Holds the registry lock past the new lease would not matter; settle on signal.
                go_rx.await.ok();
                settle_refused(tx, gate, Some(1), "cancel", 831).await?;
                anyhow::Ok(s)
            })
        })
        .await
    });
    claimed_rx.await?;
    let (kb, fb, dbb) = (k.clone(), f.clone(), db.clone());
    let second = tokio::spawn(async move {
        dbb.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kb),
                    &request(&kb, &fb, Durability::Ordinary, None),
                )
                .await?;
                anyhow::Ok(seen(&gate))
            })
        })
        .await
    });
    await_lock_wait(&pg).await?;
    go_tx.send(()).ok();
    assert_eq!(first.await??, Seen::Owned(ClaimOrigin::Reclaimed, u(730)));
    let Seen::Replay(replay) = second.await?? else {
        panic!("second recoverer did not replay")
    };
    assert_eq!(
        replay.outcome,
        ReplayOutcome::Refused("not-admissible".into())
    );
    assert_eq!(replay.audit_id, Some(u(831)));
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE trigger='cancel'")
            .await?,
        1,
        "one settlement; the crashed attempt left no audit"
    );
    Ok(())
}

/// Lost reply: the committed settlement replays verbatim (advisory probe and authoritative
/// gate), after the order's draft state moved and without any new audit or registry write.
/// A forced advisory miss followed by a competitor's settlement and a failed input resolution
/// returns the competitor's stored outcome unchanged.
#[tokio::test]
async fn lost_reply_and_advisory_miss_return_the_winning_settlement_once() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let doc = json!({"field": "x"});
    for (text, success) in [("won-success", true), ("won-refusal", false)] {
        let k = key(Trigger::DraftMutate, 40, text);
        let f = fp(Trigger::DraftMutate, Some(1), &doc);
        let order_scope = AccessScope::for_resources(vec![u(1)]);
        // 1. Advisory probe misses (nothing settled yet): the request would resolve inputs.
        let probe = reg::probe(
            &db.conn()?,
            &private_scope(&k),
            &k,
            &f,
            Some((u(1), &order_scope)),
        )
        .await?;
        assert_eq!(probe, Probe::Miss);
        // 2. A competitor with the same scoped key settles first.
        let (kw, fw) = (k.clone(), f.clone());
        let audit: u128 = if success { 840 } else { 841 };
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kw),
                    &request(&kw, &fw, Durability::Ordinary, None),
                )
                .await?;
                if success {
                    let Gate::Owned(owned) = gate else {
                        anyhow::bail!("{gate:?}")
                    };
                    committed_audit(&mut locked, tx, "draft-mutate", audit).await?;
                    owned
                        .settle(Settlement::Success {
                            order_id: Some(u(1)),
                            audit_id: u(audit),
                            response: response(200, json!({"draftRevision": "1"})),
                        })
                        .await?;
                } else {
                    settle_refused(tx, gate, Some(1), "draft-mutate", audit).await?;
                }
                anyhow::Ok(())
            })
        })
        .await?;
        let audits = count(&pg, "transition_audit").await;
        let winner = registry(&pg).await;
        // 3. The first request's port failed; its refusal transaction runs the gate and must
        //    return the competitor's exact outcome, appending nothing.
        let (kr, fr) = (k.clone(), f.clone());
        let got = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 1).await?;
                    let gate = reg::resolve(
                        GateTarget::Order(&locked),
                        &private_scope(&kr),
                        &request(&kr, &fr, Durability::Ordinary, None),
                    )
                    .await?;
                    anyhow::Ok(seen(&gate))
                })
            })
            .await?;
        let Seen::Replay(replay) = got else {
            panic!("{got:?}")
        };
        assert_eq!(replay.outcome == ReplayOutcome::Success, success);
        assert_eq!(replay.audit_id, Some(u(audit)));
        assert_eq!(count(&pg, "transition_audit").await, audits);
        assert_eq!(registry(&pg).await, winner);
        // 4. Lost reply after a later state change: the advisory probe now replays verbatim.
        pg.sql(&format!(
            "UPDATE bss_orders__order SET draft_revision=draft_revision+1 WHERE order_id='{}'",
            u(1)
        ))
        .await?;
        let Probe::Replay(again) = reg::probe(
            &db.conn()?,
            &private_scope(&k),
            &k,
            &f,
            Some((u(1), &order_scope)),
        )
        .await?
        else {
            panic!("probe did not replay")
        };
        assert_eq!(again, replay);
        // A probe for another order (or another fingerprint) never replays this record.
        let other_scope = AccessScope::for_resources(vec![u(2)]);
        assert_eq!(
            reg::probe(
                &db.conn()?,
                &private_scope(&k),
                &k,
                &f,
                Some((u(2), &other_scope))
            )
            .await?,
            Probe::Miss
        );
    }
    // An expired settled record is never replayed by the probe, even before the sweep.
    let k = key(Trigger::DraftMutate, 40, "expired");
    let f = fp(Trigger::DraftMutate, Some(1), &doc);
    seed_refusal_audit(&pg, Some(1), "draft-mutate", 842).await;
    seed_marker(
        &pg,
        &k,
        Some(1),
        f.as_str(),
        740,
        &settled_refused(842, "now()-interval '2 days'", "now()-interval '1 day'"),
    )
    .await?;
    let order_scope = AccessScope::for_resources(vec![u(1)]);
    assert_eq!(
        reg::probe(
            &db.conn()?,
            &private_scope(&k),
            &k,
            &f,
            Some((u(1), &order_scope))
        )
        .await?,
        Probe::Miss
    );
    Ok(())
}

/// Cleanup racing request-path replacement/reclaim: the sweep deletes only the exact discovered
/// generation, waits on a locked row, and can never remove a replacement or live marker or an
/// unresolved durable execution.
#[tokio::test]
async fn cleanup_races_never_remove_live_or_replacement_generations() -> anyhow::Result<()> {
    use crate::infra::maintenance::scope::discover_expired_idempotency;
    use crate::infra::maintenance::{
        MaintenanceAuthority, MaintenanceTask, ServiceActor, TargetScope,
    };
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let (maintenance, _) = pg.role("maintenance").await?;
    create(&db, 1).await?;
    let doc = json!({});
    let authority = MaintenanceAuthority::configured(
        ServiceActor::configured(u(900), u(901)).unwrap(),
        [MaintenanceTask::IdempotencyCleanup],
    );
    let grant = authority
        .grant(MaintenanceTask::IdempotencyCleanup)
        .unwrap();
    let k = key(Trigger::DraftMutate, 40, "swept");
    let f = fp(Trigger::DraftMutate, Some(1), &doc);
    seed_refusal_audit(&pg, Some(1), "draft-mutate", 850).await;
    seed_marker(
        &pg,
        &k,
        Some(1),
        f.as_str(),
        750,
        &settled_refused(850, "now()-interval '2 days'", "now()-interval '1 day'"),
    )
    .await?;
    let discovered =
        discover_expired_idempotency(&maintenance, &grant, time::OffsetDateTime::now_utc(), 10)
            .await?;
    assert_eq!(discovered.len(), 1);
    let stale_target = TargetScope::from_discovered_idempotency(&discovered[0]);

    // The request path replaces the expired generation and holds its lock; the sweep blocks.
    let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let (kr, fr, dbr) = (k.clone(), f.clone(), db.clone());
    let replacer = tokio::spawn(async move {
        dbr.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kr),
                    &request(&kr, &fr, Durability::Ordinary, None),
                )
                .await?;
                let s = seen(&gate);
                claimed_tx.send(()).ok();
                go_rx.await.ok();
                settle_refused(tx, gate, Some(1), "draft-mutate", 851).await?;
                anyhow::Ok(s)
            })
        })
        .await
    });
    claimed_rx.await?;
    let target = stale_target.clone();
    let sweep_db = maintenance.clone();
    let sweeper = tokio::spawn(async move {
        sweep_db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    Ok(repo::private::delete_expired_idempotency(tx, &target).await?)
                })
            })
            .await
            .map_err(|e: anyhow::Error| e)
    });
    await_lock_wait(&pg).await?;
    go_tx.send(()).ok();
    let Seen::Owned(ClaimOrigin::ReplacedExpired, fresh) = replacer.await?? else {
        panic!("not replaced")
    };
    assert!(!sweeper.await??, "the stale sweep deleted nothing");
    let rows = registry(&pg).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].execution_id, fresh);
    assert_ne!(fresh, u(750));

    // Sweep first: the request path then finds the key absent and claims a new generation.
    let k2 = key(Trigger::DraftMutate, 40, "swept-first");
    seed_refusal_audit(&pg, Some(1), "draft-mutate", 852).await;
    seed_marker(
        &pg,
        &k2,
        Some(1),
        f.as_str(),
        752,
        &settled_refused(852, "now()-interval '2 days'", "now()-interval '1 day'"),
    )
    .await?;
    let discovered =
        discover_expired_idempotency(&maintenance, &grant, time::OffsetDateTime::now_utc(), 10)
            .await?;
    assert_eq!(discovered.len(), 1);
    let target = TargetScope::from_discovered_idempotency(&discovered[0]);
    let removed = maintenance
        .transaction_ref_mapped(move |tx| {
            Box::pin(
                async move { Ok(repo::private::delete_expired_idempotency(tx, &target).await?) },
            )
        })
        .await
        .map_err(|e: anyhow::Error| e)?;
    assert!(removed);
    let (k3, f3) = (k2.clone(), f.clone());
    let origin = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&k3),
                    &request(&k3, &f3, Durability::Ordinary, None),
                )
                .await?;
                let s = seen(&gate);
                settle_refused(tx, gate, Some(1), "draft-mutate", 853).await?;
                anyhow::Ok(s)
            })
        })
        .await?;
    assert!(matches!(origin, Seen::Owned(ClaimOrigin::Inserted, e) if e != u(752)));
    // A live (unexpired) marker is never discovered and its direct deletion is refused.
    assert!(pg.sql("DELETE FROM bss_orders__idempotency").await.is_err());
    assert_eq!(registry(&pg).await.len(), 2);
    Ok(())
}

fn attempt_inputs() -> AttemptInputs {
    AttemptInputs {
        prepared_draft_revision: Some(0),
        proposed_arrangement: json!({"payer": u(30)}),
        authorization_fact_fingerprint: "facts".into(),
        original_principal: json!({"subject_id": u(40)}),
        proof_reference: None,
        date_policy_basis: json!({"policy": "platform"}),
        commercial_subject_id: u(50),
        commercial_subject_type: "service".into(),
        commercial_subject_tenant_id: u(20),
    }
}
/// Seed a historical marker + attempt pair atomically (deferred execution link), superuser.
async fn seed_attempt_pair(
    pg: &Pg,
    marker: entity::idempotency::Model,
    attempt: entity::commercial_attempt::Model,
) -> anyhow::Result<()> {
    use sea_orm::{EntityTrait, IntoActiveModel, TransactionTrait};
    let txn = pg.raw.begin().await?;
    entity::commercial_attempt::Entity::insert(attempt.into_active_model())
        .exec(&txn)
        .await?;
    entity::idempotency::Entity::insert(marker.into_active_model())
        .exec(&txn)
        .await?;
    txn.commit().await?;
    Ok(())
}
fn past(days: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() - time::Duration::days(days)
}
fn historical(
    k: &RegistryKey,
    order_id: u128,
    f: &Fingerprint,
    execution: u128,
    attempt_id: u128,
    unresolved: bool,
) -> (
    entity::idempotency::Model,
    entity::commercial_attempt::Model,
) {
    let marker = entity::idempotency::Model {
        operation: k.operation().token(),
        principal_scope: k.principal().as_str().to_owned(),
        idempotency_key: k.key_text(),
        order_id: Some(u(order_id)),
        request_fingerprint: f.as_str().to_owned(),
        status: if unresolved { "in_flight" } else { "settled" }.into(),
        execution_id: u(execution),
        attempt_id: Some(u(attempt_id)),
        fulfillment_control_id: None,
        owner_token: Some(u(execution + 1)),
        fencing_generation: 0,
        lease_expires_at: unresolved.then(|| past(1)),
        outcome: (!unresolved).then(|| "refused".into()),
        outcome_reason: (!unresolved).then(|| "not-admissible".into()),
        audit_id: (!unresolved).then(|| u(attempt_id + 1)),
        settled_response: (!unresolved)
            .then(|| json!({"formatVersion": 1, "status": 409, "body": {}})),
        created_at: past(3),
        expires_at: past(2),
    };
    let attempt = entity::commercial_attempt::Model {
        attempt_id: u(attempt_id),
        order_id: u(order_id),
        candidate_version: 2,
        previous_committed_version: 1,
        idempotency_execution_id: u(execution),
        operation: "submit".into(),
        principal_scope: marker.principal_scope.clone(),
        request_fingerprint: f.as_str().to_owned(),
        prepared_draft_revision: Some(0),
        proposed_arrangement: json!({}),
        authorization_fact_fingerprint: "facts".into(),
        original_principal: json!({"subject_id": u(40)}),
        proof_reference: None,
        line_requests: json!({"line-a": {"candidate": "2"}}),
        date_policy_basis: json!({}),
        commercial_subject_id: u(50),
        commercial_subject_type: "service".into(),
        commercial_subject_tenant_id: u(20),
        status: if unresolved { "running" } else { "refused" }.into(),
        owner_token: u(execution + 1),
        fencing_generation: 0,
        lease_until: unresolved.then(|| past(1)),
        receipt_results: json!({"line-a": {"receipt": "historical"}}),
        created_at: past(3),
        terminal_at: (!unresolved).then(|| past(2)),
    };
    (marker, attempt)
}

/// D-188: durable claim + candidate reservation commit before remote work; receipts are
/// recorded under the fence; an expired owner is reclaimed on the same attempt/candidate with
/// a new fence, resumed from frozen inputs, and the old worker can neither record nor settle.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One ordered fault scenario across several committed transactions"
)]
async fn commercial_execution_is_fenced_across_remote_calls() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let doc = json!({"lines": ["line-a"]});
    let k = key(Trigger::Submit, 40, "submit-1");
    let f = fp(Trigger::Submit, Some(1), &doc);
    // 1. Short transaction: claim, allocate candidate, stamp it into line requests, commit.
    let (k1, f1) = (k.clone(), f.clone());
    let worker_a: DurableExecution = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&k1),
                    &request(&k1, &f1, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                let (handle, attempt) = reg::begin_commercial_attempt(
                    owned,
                    &mut locked,
                    attempt_inputs(),
                    |c| json!({"line-a": {"candidate": c.to_string()}}),
                )
                .await?;
                assert_eq!(attempt.candidate_version, 2);
                assert_eq!(attempt.line_requests["line-a"]["candidate"], "2");
                anyhow::Ok(handle)
            })
        })
        .await?;
    assert_eq!(
        pg.scalar("SELECT version_allocation_high_water::bigint AS n FROM bss_orders__order")
            .await?,
        2
    );
    assert_eq!(
        count(&pg, "order_version").await,
        1,
        "no commercial version before final commit"
    );
    assert_eq!(worker_a.owner().fencing_generation, 0);
    // Resume loads the frozen attempt before any fresh assessment.
    let order_scope = AccessScope::for_resources(vec![u(1)]);
    let Probe::Existing(Frozen::Commercial(frozen)) = reg::probe(
        &db.conn()?,
        &private_scope(&k),
        &k,
        &f,
        Some((u(1), &order_scope)),
    )
    .await?
    else {
        panic!("probe did not find the unresolved execution")
    };
    assert_eq!(frozen.candidate_version, 2);

    // 2. Remote receipt recorded under the fence; a lost-response repeat is idempotent.
    let a = worker_a.clone();
    let recorded = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let first =
                    reg::record_receipt(&locked, &a, "line-a", json!({"receipt": "r1"})).await?;
                let again =
                    reg::record_receipt(&locked, &a, "line-a", json!({"receipt": "r1"})).await?;
                anyhow::Ok((first, again))
            })
        })
        .await?;
    assert_eq!(recorded, (Recorded::New, Recorded::AlreadyRecorded));
    let a = worker_a.clone();
    let conflict = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                Ok(reg::record_receipt(&locked, &a, "line-a", json!({"receipt": "r2"})).await?)
            })
        })
        .await;
    assert!(matches!(
        conflict.map_err(|e: anyhow::Error| e.downcast::<RegistryError>()),
        Err(Ok(RegistryError::FirstResultConflict))
    ));

    // 3. Worker A's lease expires; worker B (no presented owner) reclaims the same attempt.
    pg.sql("UPDATE bss_orders__idempotency SET lease_expires_at=now()-interval '1 second'")
        .await?;
    pg.sql("UPDATE bss_orders__commercial_attempt SET lease_until=now()-interval '1 second'")
        .await?;
    let (kb, fb) = (k.clone(), f.clone());
    let worker_b: DurableExecution = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kb),
                    &request(&kb, &fb, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                assert_eq!(owned.origin(), ClaimOrigin::Reclaimed);
                let (handle, frozen) = owned.durable()?.unwrap();
                let Frozen::Commercial(attempt) = frozen else {
                    anyhow::bail!("not commercial")
                };
                assert_eq!(attempt.candidate_version, 2, "same immutable candidate");
                assert_eq!(
                    attempt.receipt_results["line-a"]["receipt"], "r1",
                    "frozen receipts reused"
                );
                anyhow::Ok(handle)
            })
        })
        .await?;
    assert_eq!(worker_b.owner().fencing_generation, 1);
    assert_eq!(worker_b.owner().execution_id, worker_a.owner().execution_id);
    assert_ne!(worker_b.owner().owner_token, worker_a.owner().owner_token);
    assert_eq!(
        pg.scalar(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE fencing_generation=1"
        )
        .await?,
        1
    );

    // 4. The stale worker can neither record a result, settle, nor pass as the owner.
    for attempt_settle in [false, true] {
        let a = worker_a.clone();
        let stale = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let mut locked = lock(tx, 1).await?;
                    if attempt_settle {
                        reg::finish_execution(
                            &mut locked,
                            &a,
                            ExecutionTerminal::Abandoned,
                            refusal(860),
                        )
                        .await?;
                    } else {
                        reg::record_receipt(&locked, &a, "line-b", json!({"receipt": "late"}))
                            .await?;
                    }
                    anyhow::Ok(())
                })
            })
            .await;
        assert!(matches!(
            stale.map_err(|e: anyhow::Error| e.downcast::<RegistryError>()),
            Err(Ok(RegistryError::StaleOwner))
        ));
    }
    let (ka, fa, a_owner, b_owner) = (k.clone(), f.clone(), worker_a.owner(), worker_b.owner());
    let presented = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let old = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&ka),
                    &request(&ka, &fa, Durability::Durable, Some(&a_owner)),
                )
                .await?;
                let own = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&ka),
                    &request(&ka, &fa, Durability::Durable, Some(&b_owner)),
                )
                .await?;
                anyhow::Ok((seen(&old), seen(&own)))
            })
        })
        .await?;
    assert_eq!(presented.0, Seen::Still);
    assert!(matches!(
        presented.1,
        Seen::Owned(ClaimOrigin::CurrentOwner, _)
    ));

    // 5. The current owner settles a business refusal: attempt refused, candidate burned.
    let b = worker_b.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let mut locked = lock(tx, 1).await?;
            repo::private::insert_transition_audit(
                tx,
                &refusal_scope(Some(1)),
                refusal_row(Some(1), "submit", 861),
            )
            .await?;
            reg::finish_execution(&mut locked, &b, ExecutionTerminal::Refused, refusal(861))
                .await?;
            anyhow::Ok(())
        })
    })
    .await?;
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE status='refused' AND terminal_at IS NOT NULL AND lease_until IS NULL").await?, 1);
    assert_eq!(
        pg.scalar("SELECT version_allocation_high_water::bigint AS n FROM bss_orders__order")
            .await?,
        2
    );
    assert_eq!(
        pg.scalar("SELECT current_version::bigint AS n FROM bss_orders__order")
            .await?,
        1
    );
    let (kr, fr) = (k.clone(), f.clone());
    let replay = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kr),
                    &request(&kr, &fr, Durability::Durable, None),
                )
                .await?;
                anyhow::Ok(seen(&gate))
            })
        })
        .await?;
    assert!(matches!(
        replay,
        Seen::Replay(Replay {
            outcome: ReplayOutcome::Refused(_),
            ..
        })
    ));
    // The settled worker's handle is now stale too: no second settlement.
    let b = worker_b.clone();
    let again = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                reg::finish_execution(&mut locked, &b, ExecutionTerminal::Refused, refusal(862))
                    .await?;
                anyhow::Ok(())
            })
        })
        .await;
    assert!(again.is_err());
    Ok(())
}

/// Unresolved attempts survive response-key expiry (recovered on the same identity, never
/// swept); a resolved expired key's text becomes a fresh execution and candidate, and a late
/// worker of the old generation cannot touch the new marker.
#[tokio::test]
async fn unresolved_attempts_outlive_key_expiry_and_reused_keys_get_fresh_executions()
-> anyhow::Result<()> {
    use crate::infra::maintenance::scope::discover_expired_idempotency;
    use crate::infra::maintenance::{
        MaintenanceAuthority, MaintenanceTask, ServiceActor, TargetScope,
    };
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let (maintenance, _) = pg.role("maintenance").await?;
    create(&db, 2).await?;
    create(&db, 3).await?;
    pg.sql("UPDATE bss_orders__order SET version_allocation_high_water=2")
        .await?;
    let doc = json!({});
    // Order 3: unresolved attempt behind an expired key window and expired lease.
    let k3 = key(Trigger::Submit, 40, "unresolved");
    let f3 = fp(Trigger::Submit, Some(3), &doc);
    let (marker, attempt) = historical(&k3, 3, &f3, 760, 761, true);
    seed_attempt_pair(&pg, marker, attempt).await?;
    let authority = MaintenanceAuthority::configured(
        ServiceActor::configured(u(900), u(901)).unwrap(),
        [MaintenanceTask::IdempotencyCleanup],
    );
    let grant = authority
        .grant(MaintenanceTask::IdempotencyCleanup)
        .unwrap();
    let found =
        discover_expired_idempotency(&maintenance, &grant, time::OffsetDateTime::now_utc(), 10)
            .await?;
    let target = TargetScope::from_discovered_idempotency(&found[0]);
    let swept: anyhow::Result<bool> = maintenance
        .transaction_ref_mapped(move |tx| {
            Box::pin(
                async move { Ok(repo::private::delete_expired_idempotency(tx, &target).await?) },
            )
        })
        .await;
    assert!(
        swept.is_err(),
        "unresolved execution must survive retention"
    );
    let (kr, fr) = (k3.clone(), f3.clone());
    let recovered = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 3).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kr),
                    &request(&kr, &fr, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                let (handle, _) = owned.durable()?.unwrap();
                anyhow::Ok((owned.origin(), handle))
            })
        })
        .await?;
    assert_eq!(recovered.0, ClaimOrigin::Reclaimed);
    assert_eq!(recovered.1.owner().execution_id, u(760));
    assert_eq!(recovered.1.link(), ExecutionLink::Commercial(u(761)));
    assert_eq!(recovered.1.owner().fencing_generation, 1);

    // Order 2: resolved (refused) attempt behind an expired settled key.
    let k2 = key(Trigger::Submit, 40, "reused");
    let f2 = fp(Trigger::Submit, Some(2), &doc);
    seed_refusal_audit(&pg, Some(2), "submit", 771).await;
    let (marker, attempt) = historical(&k2, 2, &f2, 770, 770 + 1 - 1 + 100, false);
    let mut marker = marker;
    marker.audit_id = Some(u(771));
    seed_attempt_pair(&pg, marker, attempt).await?;
    let (kn, fn_) = (k2.clone(), f2.clone());
    let fresh = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 2).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kn),
                    &request(&kn, &fn_, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                assert_eq!(owned.origin(), ClaimOrigin::ReplacedExpired);
                let (handle, attempt) = reg::begin_commercial_attempt(
                    owned,
                    &mut locked,
                    attempt_inputs(),
                    |c| json!({"line-a": {"candidate": c.to_string()}}),
                )
                .await?;
                anyhow::Ok((handle, attempt.candidate_version))
            })
        })
        .await?;
    assert_ne!(fresh.0.owner().execution_id, u(770));
    assert_eq!(
        fresh.1, 3,
        "new execution reserves a new candidate; 2 stays burned"
    );
    let before = registry(&pg).await;
    let late = DurableExecution::stale_for_test(
        ("submit", principal(40).as_str(), "reused"),
        u(2),
        ExecutionOwner {
            execution_id: u(770),
            owner_token: u(771),
            fencing_generation: 0,
        },
        ExecutionLink::Commercial(u(870)),
    );
    let late_result = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 2).await?;
                reg::record_receipt(&locked, &late, "line-a", json!({"receipt": "late"})).await?;
                anyhow::Ok(())
            })
        })
        .await;
    assert!(matches!(
        late_result.map_err(|e: anyhow::Error| e.downcast::<RegistryError>()),
        Err(Ok(RegistryError::StaleOwner))
    ));
    assert_eq!(registry(&pg).await, before);
    Ok(())
}

fn control_inputs() -> ControlInputs {
    ControlInputs {
        expected_version: 1,
        fulfillment_attempt_id: u(99),
        generation: 1,
        original_actor: json!({"subject_id": u(40)}),
        proof_reference: None,
        authorization_fact_fingerprint: "facts".into(),
        roster: json!([{"receiver": "r1"}]),
        roster_digest: "digest".into(),
        receiver_commands: json!({"r1": {"command": "pause"}}),
    }
}

/// D-198: staged control intent sets the pending pointer, blocks a second control, records
/// immutable receiver evidence under the fence, refuses stale workers, and its final
/// settlement clears only its own pointer.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One ordered fault scenario across several committed transactions"
)]
async fn staged_control_is_fenced_and_clears_only_its_own_pointer() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let doc = json!({"reason": "pause"});
    let k = key(Trigger::Hold, 40, "hold-1");
    let f = fp(Trigger::Hold, Some(1), &doc);
    let begin = |k: RegistryKey, f: Fingerprint| {
        let db = db.clone();
        async move {
            db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let mut locked = lock(tx, 1).await?;
                    let gate = reg::resolve(
                        GateTarget::Order(&locked),
                        &private_scope(&k),
                        &request(&k, &f, Durability::Durable, None),
                    )
                    .await?;
                    let Gate::Owned(owned) = gate else {
                        anyhow::bail!("{gate:?}")
                    };
                    let (handle, _) =
                        reg::begin_fulfillment_control(owned, &mut locked, control_inputs())
                            .await?;
                    anyhow::Ok(handle)
                })
            })
            .await
        }
    };
    let first = begin(k.clone(), f.clone()).await?;
    let ExecutionLink::Control(control_id) = first.link() else {
        panic!("not a control")
    };
    let pending = || async {
        repo::find_order(
            &pg.db.conn().unwrap(),
            &AccessScope::for_resources(vec![u(1)]),
            u(1),
        )
        .await
        .unwrap()
        .unwrap()
        .fulfillment_control_pending
    };
    assert_eq!(pending().await, Some(control_id));
    let second_key = key(Trigger::Hold, 40, "hold-2");
    let blocked = begin(
        second_key.clone(),
        fp(Trigger::Hold, Some(1), &json!({"reason": "again"})),
    )
    .await;
    assert!(matches!(
        blocked.map_err(anyhow::Error::downcast::<RegistryError>),
        Err(Ok(RegistryError::ControlPending))
    ));
    assert_eq!(count(&pg, "fulfillment_control").await, 1);

    let h = first.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let locked = lock(tx, 1).await?;
            reg::record_control_progress(
                &locked,
                &h,
                &[],
                &[("r1".into(), json!({"paused": true}))],
                Some("awaiting"),
            )
            .await?;
            reg::record_control_progress(
                &locked,
                &h,
                &[],
                &[("r1".into(), json!({"paused": true}))],
                Some("barrier_ready"),
            )
            .await?;
            anyhow::Ok(())
        })
    })
    .await?;
    for (evidence, status) in [
        (json!({"paused": false}), None),
        (json!({"paused": true}), Some("prepared")),
    ] {
        let h = first.clone();
        let rewritten = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 1).await?;
                    reg::record_control_progress(
                        &locked,
                        &h,
                        &[],
                        &[("r1".into(), evidence)],
                        status,
                    )
                    .await?;
                    anyhow::Ok(())
                })
            })
            .await;
        let refused = rewritten.map_err(anyhow::Error::downcast::<RegistryError>);
        if status.is_some() {
            // Refused before any write, not only by the DB regression guard.
            assert!(
                matches!(refused, Err(Ok(RegistryError::StatusRegression))),
                "{refused:?}"
            );
        } else {
            assert!(
                matches!(refused, Err(Ok(RegistryError::FirstResultConflict))),
                "{refused:?}"
            );
        }
    }

    // Lease expiry and reclaim by another worker; the old worker cannot settle or clear.
    pg.sql("UPDATE bss_orders__idempotency SET lease_expires_at=now()-interval '1 second'")
        .await?;
    pg.sql("UPDATE bss_orders__fulfillment_control SET lease_until=now()-interval '1 second'")
        .await?;
    let (kb, fb) = (k.clone(), f.clone());
    let worker_b: DurableExecution = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&kb),
                    &request(&kb, &fb, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                let (handle, frozen) = owned.durable()?.unwrap();
                let Frozen::Control(control) = frozen else {
                    anyhow::bail!("not a control")
                };
                assert_eq!(control.status, "barrier_ready");
                assert_eq!(control.receiver_evidence["r1"]["paused"], true);
                anyhow::Ok(handle)
            })
        })
        .await?;
    let stale = first.clone();
    let refused = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                reg::finish_execution(
                    &mut locked,
                    &stale,
                    ExecutionTerminal::Abandoned,
                    refusal(880),
                )
                .await?;
                anyhow::Ok(())
            })
        })
        .await;
    assert!(matches!(
        refused.map_err(anyhow::Error::downcast::<RegistryError>),
        Err(Ok(RegistryError::StaleOwner))
    ));
    assert_eq!(pending().await, Some(control_id));

    // Final settlement by the current owner: transition audit, control settled, pointer cleared.
    let b = worker_b.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let mut locked = lock(tx, 1).await?;
            committed_audit(&mut locked, tx, "hold", 881).await?;
            reg::finish_execution(
                &mut locked,
                &b,
                ExecutionTerminal::Completed,
                Settlement::Success {
                    order_id: None,
                    audit_id: u(881),
                    response: response(200, json!({"state": "on_hold"})),
                },
            )
            .await?;
            anyhow::Ok(())
        })
    })
    .await?;
    assert_eq!(pending().await, None);
    assert_eq!(
        pg.scalar(
            "SELECT count(*) AS n FROM bss_orders__fulfillment_control WHERE status='settled'"
        )
        .await?,
        1
    );

    // A newer control's pointer is never cleared by the old generation.
    let second = begin(
        second_key,
        fp(Trigger::Hold, Some(1), &json!({"reason": "again"})),
    )
    .await?;
    let ExecutionLink::Control(second_id) = second.link() else {
        panic!("not a control")
    };
    let stale = worker_b.clone();
    let late = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                reg::finish_execution(
                    &mut locked,
                    &stale,
                    ExecutionTerminal::Abandoned,
                    refusal(882),
                )
                .await?;
                anyhow::Ok(())
            })
        })
        .await;
    assert!(late.is_err());
    assert_eq!(pending().await, Some(second_id));
    Ok(())
}

/// Fresh database time after a lock wait: the contender's transaction starts while the lease
/// is live, waits on the registry row lock past the deadline, and must then classify against
/// `clock_timestamp()` (expired → reclaim), not its transaction-start `now()` (still live).
#[tokio::test]
async fn deadline_is_evaluated_at_fresh_db_time_after_the_lock_wait() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let doc = json!({});
    let k = key(Trigger::Create, 40, "waited");
    let f = fp(Trigger::Create, None, &doc);
    seed_marker(
        &pg,
        &k,
        None,
        f.as_str(),
        790,
        &in_flight(
            "now()+interval '2 seconds'",
            "now()",
            "now()+interval '1 day'",
        ),
    )
    .await?;
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let (ka, fa, dba) = (k.clone(), f.clone(), db.clone());
    let holder = tokio::spawn(async move {
        dba.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let gate = reg::resolve(
                    GateTarget::Create(tx),
                    &private_scope(&ka),
                    &request(&ka, &fa, Durability::Ordinary, None),
                )
                .await?;
                let s = seen(&gate);
                held_tx.send(()).ok();
                // Keep the row lock until the seeded lease has certainly passed.
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                anyhow::Ok(s)
            })
        })
        .await
    });
    held_rx.await?;
    let (kb, fb, dbb) = (k.clone(), f.clone(), db.clone());
    let waiter = tokio::spawn(async move {
        dbb.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let gate = reg::resolve(
                    GateTarget::Create(tx),
                    &private_scope(&kb),
                    &request(&kb, &fb, Durability::Ordinary, None),
                )
                .await?;
                let s = seen(&gate);
                settle_refused(tx, gate, None, "create", 890).await?;
                anyhow::Ok(s)
            })
        })
        .await
    });
    await_lock_wait(&pg).await?;
    assert_eq!(
        holder.await??,
        Seen::Still,
        "the lease was live for the holder"
    );
    assert_eq!(waiter.await??, Seen::Owned(ClaimOrigin::Reclaimed, u(790)));
    let row = registry(&pg).await.remove(0);
    assert_eq!(
        (row.status.as_str(), row.outcome_reason.as_deref()),
        ("settled", Some("not-admissible"))
    );
    Ok(())
}

/// Two absent-key contenders of one principal targeting different aggregates (gap review): the
/// second waits on the uniqueness check behind the first. A committed winner binds the key to
/// its order and the other target is a mismatch that leaves the winner unchanged; a
/// rolled-back winner leaves the waiting contender as the owner of its own insert, bound to
/// its own order.
#[tokio::test]
async fn same_key_contenders_on_different_aggregates_bind_exactly_one_target() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    create(&db, 2).await?;
    let doc = json!({});
    for commit_winner in [true, false] {
        let text = if commit_winner {
            "targets-commit"
        } else {
            "targets-rollback"
        };
        let k = key(Trigger::DraftMutate, 40, text);
        let (fa, fb) = (
            fp(Trigger::DraftMutate, Some(1), &doc),
            fp(Trigger::DraftMutate, Some(2), &doc),
        );
        let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel::<()>();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (ka, dba) = (k.clone(), db.clone());
        let winner = tokio::spawn(async move {
            dba.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 1).await?;
                    let gate = reg::resolve(
                        GateTarget::Order(&locked),
                        &private_scope(&ka),
                        &request(&ka, &fa, Durability::Ordinary, None),
                    )
                    .await?;
                    assert!(matches!(seen(&gate), Seen::Owned(ClaimOrigin::Inserted, _)));
                    claimed_tx.send(()).ok();
                    go_rx.await.ok();
                    if !commit_winner {
                        anyhow::bail!("injected failure after claim");
                    }
                    settle_refused(tx, gate, Some(1), "draft-mutate", 900).await?;
                    anyhow::Ok(())
                })
            })
            .await
        });
        claimed_rx.await?;
        let (kb, dbb) = (k.clone(), db.clone());
        let contender = tokio::spawn(async move {
            dbb.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 2).await?;
                    let gate = reg::resolve(
                        GateTarget::Order(&locked),
                        &private_scope(&kb),
                        &request(&kb, &fb, Durability::Ordinary, None),
                    )
                    .await?;
                    let s = seen(&gate);
                    if let Gate::Owned(_) = gate {
                        settle_refused(tx, gate, Some(2), "draft-mutate", 901).await?;
                    }
                    anyhow::Ok(s)
                })
            })
            .await
        });
        await_lock_wait(&pg).await?;
        go_tx.send(()).ok();
        let winner = winner.await?;
        let contender = contender.await??;
        let row = registry(&pg)
            .await
            .into_iter()
            .find(|r| r.idempotency_key == text)
            .expect("one record for the key");
        if commit_winner {
            winner?;
            assert_eq!(contender, Seen::Mismatch);
            assert_eq!(row.order_id, Some(u(1)), "the winner's target is kept");
            assert_eq!(row.audit_id, Some(u(900)));
        } else {
            assert!(winner.is_err());
            assert!(
                matches!(contender, Seen::Owned(ClaimOrigin::Inserted, _)),
                "{contender:?}"
            );
            assert_eq!(
                row.order_id,
                Some(u(2)),
                "the surviving contender binds its own target"
            );
            assert_eq!(row.audit_id, Some(u(901)));
        }
    }
    Ok(())
}

/// Lease recovery on create, where the registry row lock is the only serialization (no
/// aggregate lock exists): the first recoverer reclaims with a short lease and keeps the lock
/// past its new deadline; the second must wait rather than reclaim the elapsed lease, then
/// replay the first's settlement. One settlement and one refusal audit result (gap review).
#[tokio::test]
async fn create_recoverer_past_its_new_lease_still_holds_the_registry_lock() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let doc = json!({});
    let k = key(Trigger::Create, 40, "short-lease");
    let f = fp(Trigger::Create, None, &doc);
    seed_marker(
        &pg,
        &k,
        None,
        f.as_str(),
        910,
        &in_flight(
            "now()-interval '1 second'",
            "now()",
            "now()+interval '1 day'",
        ),
    )
    .await?;
    let short = LeaseDuration::from_seconds(1).unwrap();
    let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel::<()>();
    let (ka, fa, dba) = (k.clone(), f.clone(), db.clone());
    let first = tokio::spawn(async move {
        dba.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let gate = reg::resolve(
                    GateTarget::Create(tx),
                    &private_scope(&ka),
                    &GateRequest {
                        lease: short,
                        ..request(&ka, &fa, Durability::Ordinary, None)
                    },
                )
                .await?;
                let s = seen(&gate);
                claimed_tx.send(()).ok();
                // Hold the registry lock well past the reclaimed one-second lease.
                tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
                settle_refused(tx, gate, None, "create", 911).await?;
                anyhow::Ok(s)
            })
        })
        .await
    });
    claimed_rx.await?;
    let (kb, fb, dbb) = (k.clone(), f.clone(), db.clone());
    let second = tokio::spawn(async move {
        dbb.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let gate = reg::resolve(
                    GateTarget::Create(tx),
                    &private_scope(&kb),
                    &GateRequest {
                        lease: short,
                        ..request(&kb, &fb, Durability::Ordinary, None)
                    },
                )
                .await?;
                let s = seen(&gate);
                if let Gate::Owned(_) = gate {
                    settle_refused(tx, gate, None, "create", 912).await?;
                }
                anyhow::Ok(s)
            })
        })
        .await
    });
    await_lock_wait(&pg).await?;
    assert_eq!(first.await??, Seen::Owned(ClaimOrigin::Reclaimed, u(910)));
    let Seen::Replay(replay) = second.await?? else {
        panic!("the second recoverer bypassed the registry lock")
    };
    assert_eq!(replay.audit_id, Some(u(911)));
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE trigger='create'")
            .await?,
        1
    );
    Ok(())
}

/// A durable marker committed without a linked attempt (no candidate reserved, no command
/// issued) is not stranded until retention expiry: its expired lease is reclaimed with a
/// rotated owner and checked fence, and that reclaimer starts the attempt under the new fence.
/// A linked reclaim (frozen attempt) still cannot begin a second attempt (gap review).
#[tokio::test]
async fn unlinked_durable_marker_is_reclaimed_into_one_fenced_attempt() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let doc = json!({"lines": ["line-a"]});
    let k = key(Trigger::Submit, 40, "unlinked");
    let f = fp(Trigger::Submit, Some(1), &doc);
    pg.sql(&format!(
        "INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,order_id,request_fingerprint,status,execution_id,owner_token,fencing_generation,lease_expires_at,created_at,expires_at) VALUES('{}','{}','{}','{}','{}','in_flight','{}','{}',0,now()-interval '1 second',now(),now()+interval '1 day')",
        k.operation().token(),
        k.principal().as_str(),
        k.key_text(),
        u(1),
        f.as_str(),
        u(920),
        u(921),
    ))
    .await?;
    let (k1, f1) = (k.clone(), f.clone());
    let (handle, attempt) = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&k1),
                    &request(&k1, &f1, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                assert_eq!(owned.origin(), ClaimOrigin::Reclaimed);
                assert!(owned.durable()?.is_none(), "nothing was linked");
                Ok(reg::begin_commercial_attempt(
                    owned,
                    &mut locked,
                    attempt_inputs(),
                    |c| json!({"line-a": {"candidate": c.to_string()}}),
                )
                .await?)
            })
        })
        .await?;
    assert_eq!(
        handle.owner().execution_id,
        u(920),
        "same registry generation"
    );
    assert_eq!(handle.owner().fencing_generation, 1);
    assert_ne!(
        handle.owner().owner_token,
        u(921),
        "the stale owner token is rotated"
    );
    assert_eq!(
        (attempt.fencing_generation, attempt.candidate_version),
        (1, 2)
    );
    // The stale pre-reclaim owner cannot act on the new attempt.
    let stale = DurableExecution::stale_for_test(
        (
            &k.operation().token(),
            k.principal().as_str(),
            &k.key_text(),
        ),
        u(1),
        ExecutionOwner {
            execution_id: u(920),
            owner_token: u(921),
            fencing_generation: 0,
        },
        ExecutionLink::Commercial(attempt.attempt_id),
    );
    let late = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                Ok(
                    reg::record_receipt(&locked, &stale, "line-a", json!({"receipt": "late"}))
                        .await?,
                )
            })
        })
        .await;
    assert!(matches!(
        late.map_err(anyhow::Error::downcast::<RegistryError>),
        Err(Ok(RegistryError::StaleOwner))
    ));
    // Once linked, an expired-lease reclaim recovers that attempt and cannot begin another.
    pg.sql("UPDATE bss_orders__idempotency SET lease_expires_at=now()-interval '1 second'")
        .await?;
    pg.sql("UPDATE bss_orders__commercial_attempt SET lease_until=now()-interval '1 second'")
        .await?;
    let (k2, f2) = (k.clone(), f.clone());
    let second = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let gate = reg::resolve(
                    GateTarget::Order(&locked),
                    &private_scope(&k2),
                    &request(&k2, &f2, Durability::Durable, None),
                )
                .await?;
                let Gate::Owned(owned) = gate else {
                    anyhow::bail!("{gate:?}")
                };
                assert_eq!(owned.origin(), ClaimOrigin::Reclaimed);
                Ok(reg::begin_commercial_attempt(
                    owned,
                    &mut locked,
                    attempt_inputs(),
                    |c| json!({"line-a": {"candidate": c.to_string()}}),
                )
                .await?)
            })
        })
        .await;
    assert!(matches!(
        second.map_err(anyhow::Error::downcast::<RegistryError>),
        Err(Ok(RegistryError::SettlementMismatch))
    ));
    assert_eq!(count(&pg, "commercial_attempt").await, 1);
    assert_eq!(
        pg.scalar("SELECT version_allocation_high_water::bigint AS n FROM bss_orders__order")
            .await?,
        2,
        "no second candidate"
    );
    Ok(())
}
