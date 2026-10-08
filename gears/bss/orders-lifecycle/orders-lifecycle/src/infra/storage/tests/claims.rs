//! S2-07: transactional in-flight overlap claims on real PostgreSQL through the restricted
//! runtime role (Foundation §3.6 step 17; DESIGN §3.7 `orders_inflight_overlap_claim`).
use super::*;
use crate::domain::audit::AuditTrigger;
use crate::domain::overlap::{
    BlockedTuple, ClaimDirective, ClaimOutcome, ClaimTuple, OverlapConflict, OverlapScopeKey,
    ProposedClaims, TERMINAL_STATES,
};
use crate::infra::storage::repo::TransactionRunner;
use crate::infra::storage::repo::audit as writer;
use crate::infra::storage::repo::claims::{ProposalAuthority, transition_tx_config};
use bss_orders_lifecycle_sdk::catalog::{OrderState, Trigger};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit_db::secure::{TxConfig, TxIsolationLevel};
use toolkit_security::AccessScope;

pub(super) fn scope(id: u128) -> AccessScope {
    AccessScope::for_resources(vec![u(id)])
}
pub(super) async fn lock<T: TransactionRunner>(
    tx: &T,
    id: u128,
) -> anyhow::Result<repo::LockedOrder<'_, T>> {
    repo::LockedOrder::lock_current(tx, &scope(id), u(id))
        .await?
        .ok_or_else(|| anyhow::anyhow!("order {id} not visible"))
}
fn keys(ks: &[String]) -> Vec<OverlapScopeKey> {
    ks.iter()
        .map(|k| OverlapScopeKey::try_from(k.clone()).unwrap())
        .collect()
}
fn tuple(payer: u128, resource: u128, k: &str) -> ClaimTuple {
    ClaimTuple {
        payer_tenant_id: u(payer),
        resource_tenant_id: u(resource),
        overlap_scope_key: OverlapScopeKey::try_from(k.to_owned()).unwrap(),
    }
}
fn owned(ks: &[&str]) -> Vec<String> {
    ks.iter().map(|k| (*k).to_owned()).collect()
}

/// An order with its own payer/resource axes and an empty version 1.
pub(super) async fn create_for(
    db: &Db,
    id: u128,
    payer: u128,
    resource: u128,
) -> anyhow::Result<()> {
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let mut row = order(id);
            row.payer_tenant_id = u(payer);
            row.resource_tenant_id = u(resource);
            let row = repo::insert_order(tx, &scope(id), row).await?;
            let locked = repo::LockedOrder::acquire(tx, &scope(id), &row).await?;
            let mut v = version(id, 1, None);
            v.payer_tenant_id = u(payer);
            locked.insert_order_version(v).await?;
            anyhow::Ok(())
        })
    })
    .await
}

/// Step 17 of a submit/amendment inside the caller's transaction. `disclose` is the caller's
/// order-read scope used to name a conflicting holder.
pub(super) async fn replace_in<T: TransactionRunner>(
    tx: &T,
    id: u128,
    payer: u128,
    ks: &[String],
    candidate: i32,
    disclose: &[u128],
) -> anyhow::Result<ClaimOutcome> {
    let locked = lock(tx, id).await?;
    let mut proposed = locked.row().clone();
    proposed.payer_tenant_id = u(payer);
    let proposal = ProposedClaims::new(u(payer), proposed.resource_tenant_id, keys(ks))?;
    let directive =
        ClaimDirective::for_transition(Trigger::Amendment, OrderState::Submitted, Some(proposal))?;
    let disclosure = AccessScope::for_resources(disclose.iter().map(|n| u(*n)).collect());
    let proposed_scope = scope(id);
    let authority = ProposalAuthority {
        proposed: &proposed,
        proposed_scope: &proposed_scope,
        disclosure_scope: &disclosure,
        proposed_version: candidate,
    };
    Ok(locked.maintain_claims(&directive, Some(&authority)).await?)
}
pub(super) async fn replace(
    db: &Db,
    id: u128,
    payer: u128,
    ks: &[&str],
    candidate: i32,
    disclose: &[u128],
) -> anyhow::Result<ClaimOutcome> {
    let ks = owned(ks);
    let disclose = disclose.to_vec();
    db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
        Box::pin(async move { replace_in(tx, id, payer, &ks, candidate, &disclose).await })
    })
    .await
}
async fn terminal(
    db: &Db,
    id: u128,
    trigger: Trigger,
    target: OrderState,
) -> anyhow::Result<ClaimOutcome> {
    db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
        Box::pin(async move {
            let locked = lock(tx, id).await?;
            let directive = ClaimDirective::for_transition(trigger, target, None)?;
            anyhow::Ok(locked.maintain_claims(&directive, None).await?)
        })
    })
    .await
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Row {
    pub claim_id: Uuid,
    pub payer: Uuid,
    pub resource: Uuid,
    pub key: String,
    pub version: i32,
    pub live: bool,
}
pub(super) async fn rows(pg: &Pg, id: u128) -> anyhow::Result<Vec<Row>> {
    let sql = format!(
        "SELECT claim_id, payer_tenant_id, resource_tenant_id, overlap_scope_key, version, \
         released_at IS NULL AS live FROM bss_orders__inflight_overlap_claim \
         WHERE order_id='{}' ORDER BY claimed_at, overlap_scope_key, payer_tenant_id",
        u(id)
    );
    let mut out = Vec::new();
    for r in pg
        .raw
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await?
    {
        out.push(Row {
            claim_id: r.try_get("", "claim_id")?,
            payer: r.try_get("", "payer_tenant_id")?,
            resource: r.try_get("", "resource_tenant_id")?,
            key: r.try_get("", "overlap_scope_key")?,
            version: r.try_get("", "version")?,
            live: r.try_get("", "live")?,
        });
    }
    Ok(out)
}
pub(super) async fn live(pg: &Pg, id: u128) -> anyhow::Result<Vec<(Uuid, String)>> {
    let mut v: Vec<_> = rows(pg, id)
        .await?
        .into_iter()
        .filter(|r| r.live)
        .map(|r| (r.payer, r.key))
        .collect();
    v.sort();
    Ok(v)
}
fn admitted(outcome: &ClaimOutcome) -> (usize, usize, usize) {
    match outcome {
        ClaimOutcome::Admitted {
            retained,
            acquired,
            superseded_released,
        } => (retained.len(), acquired.len(), superseded_released.len()),
        other => panic!("expected admission, got {other:?}"),
    }
}
fn conflict(outcome: ClaimOutcome) -> crate::domain::overlap::OverlapConflict {
    match outcome {
        ClaimOutcome::Conflict(c) => c,
        other => panic!("expected conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn full_tuples_with_database_time_duplicates_and_distinct_axes() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create_for(&db, 1, 30, 10).await?;
    create_for(&db, 2, 30, 11).await?;
    create_for(&db, 3, 31, 10).await?;
    // Duplicate line keys collapse to one claim per tuple, inserted in byte order.
    let out = replace(&db, 1, 30, &["k2", "k1", "k2", "k1"], 2, &[1]).await?;
    assert_eq!(admitted(&out), (0, 2, 0));
    let stored = rows(&pg, 1).await?;
    assert_eq!(stored.len(), 2);
    assert!(
        stored
            .iter()
            .all(|r| r.live && r.version == 2 && r.payer == u(30))
    );
    // The reservation instant is the database clock inside the transaction: the caller has no
    // time parameter, and the fixture clock (2026-09-21) can never appear.
    assert_eq!(
        pg.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{}' \
             AND claimed_at > now() - interval '5 minutes' AND claimed_at <= clock_timestamp() \
             AND claimed_at <> '{}'",
            u(1),
            "2026-09-21 13:46:40+00"
        ))
        .await?,
        2
    );
    // Same payer and key, other resource tenant: a distinct live claim (D-179).
    assert_eq!(
        admitted(&replace(&db, 2, 30, &["k1"], 2, &[2]).await?),
        (0, 1, 0)
    );
    // Same key, other payer: distinct as well.
    assert_eq!(
        admitted(&replace(&db, 3, 31, &["k1"], 2, &[3]).await?),
        (0, 1, 0)
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE overlap_scope_key='k1' AND released_at IS NULL")
            .await?,
        3
    );
    // An unchanged held tuple is never re-offered: no new row, nothing released.
    let before = rows(&pg, 1).await?;
    assert_eq!(
        admitted(&replace(&db, 1, 30, &["k1", "k2"], 3, &[1]).await?),
        (2, 0, 0)
    );
    assert_eq!(rows(&pg, 1).await?, before);
    Ok(())
}

#[tokio::test]
async fn payer_amendment_replaces_tuples_and_frees_the_old_pair() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for id in 1..=3 {
        create_for(&db, id, 30, 10).await?;
    }
    replace(&db, 1, 30, &["k"], 2, &[1]).await?;
    // Payer-only change: acquire (31,10,k), then release (30,10,k) in the same transaction.
    assert_eq!(
        admitted(&replace(&db, 1, 31, &["k"], 3, &[1]).await?),
        (0, 1, 1)
    );
    assert_eq!(live(&pg, 1).await?, vec![(u(31), "k".to_owned())]);
    // The old pair is reusable by another order after commit.
    assert_eq!(
        admitted(&replace(&db, 2, 30, &["k"], 2, &[2]).await?),
        (0, 1, 0)
    );
    // Payer-plus-key change.
    assert_eq!(
        admitted(&replace(&db, 1, 32, &["k2"], 4, &[1]).await?),
        (0, 1, 1)
    );
    assert_eq!(live(&pg, 1).await?, vec![(u(32), "k2".to_owned())]);
    // The new pair is occupied: the refusal keeps the old payer's claims and leaves no
    // provisional live claim; the conflicting order is named only when readable.
    replace(&db, 3, 33, &["k"], 2, &[3]).await?;
    let before = rows(&pg, 1).await?;
    let c = conflict(replace(&db, 1, 33, &["k", "k3"], 5, &[1, 3]).await?);
    assert_eq!(
        c.blocked,
        vec![BlockedTuple {
            tuple: tuple(33, 10, "k"),
            visible_holder: Some(u(3)),
        }]
    );
    assert_eq!(c.provisional_released.len(), 1);
    assert_eq!(live(&pg, 1).await?, vec![(u(32), "k2".to_owned())]);
    let after = rows(&pg, 1).await?;
    assert!(before.iter().all(|r| after.contains(r)));
    assert_eq!(after.len(), before.len() + 1);
    // Hidden holder: same refusal, no order named.
    let c = conflict(replace(&db, 1, 33, &["k"], 5, &[1]).await?);
    assert_eq!(c.blocked[0].visible_holder, None);
    Ok(())
}

#[tokio::test]
async fn shortfall_releases_only_returned_ids_and_keeps_preexisting_claims() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for id in 1..=3 {
        create_for(&db, id, 30, 10).await?;
    }
    replace(&db, 2, 30, &["m"], 2, &[2]).await?;
    replace(&db, 1, 30, &["a"], 2, &[1]).await?;
    let held_before = rows(&pg, 1).await?;
    let blocker_before = rows(&pg, 2).await?;
    // b and c are inserted before the collision on m, z after it; all three are provisional.
    let c = conflict(replace(&db, 1, 30, &["z", "m", "c", "a", "b"], 3, &[1, 2]).await?);
    assert_eq!(c.blocked.len(), 1);
    assert_eq!(c.blocked[0].tuple, tuple(30, 10, "m"));
    assert_eq!(c.blocked[0].visible_holder, Some(u(2)));
    assert_eq!(c.provisional_released.len(), 3);
    let after = rows(&pg, 1).await?;
    // The pre-existing claim is byte-for-byte unchanged and still live.
    assert!(held_before.iter().all(|r| after.contains(r)));
    // Exactly the returned IDs are released, for the proposed version.
    let mut released: Vec<_> = after
        .iter()
        .filter(|r| !r.live)
        .map(|r| r.claim_id)
        .collect();
    let mut expected = c.provisional_released.clone();
    released.sort();
    expected.sort();
    assert_eq!(released, expected);
    assert!(after.iter().filter(|r| !r.live).all(|r| r.version == 3));
    let mut keys: Vec<_> = after
        .iter()
        .filter(|r| !r.live)
        .map(|r| r.key.clone())
        .collect();
    keys.sort();
    assert_eq!(keys, ["b", "c", "z"]);
    // The blocked tuple is untouched.
    assert_eq!(rows(&pg, 2).await?, blocker_before);
    // Released provisional tuples are available to another order after commit.
    assert_eq!(
        admitted(&replace(&db, 3, 30, &["b", "c", "z"], 2, &[3]).await?),
        (0, 3, 0)
    );
    // Zero inserted rows: every missing tuple collides, so no release statement runs.
    let rows_before = pg
        .scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
        .await?;
    let c = conflict(replace(&db, 1, 30, &["a", "m"], 3, &[1]).await?);
    assert!(c.provisional_released.is_empty());
    assert_eq!(c.blocked[0].visible_holder, None);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
            .await?,
        rows_before
    );
    Ok(())
}

#[tokio::test]
async fn contract_violations_abort_instead_of_refusing() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create_for(&db, 1, 30, 10).await?;
    replace(&db, 1, 30, &["k"], 2, &[1]).await?;
    let result: anyhow::Result<()> = db
        .transaction_ref_mapped_with_config(transition_tx_config(), |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let proposed = locked.row().clone();
                let s = scope(1);
                let proposal =
                    |payer| ProposedClaims::new(u(payer), u(10), keys(&owned(&["k2"]))).unwrap();
                let authority = |version| ProposalAuthority {
                    proposed: &proposed,
                    proposed_scope: &s,
                    disclosure_scope: &s,
                    proposed_version: version,
                };
                let replace = ClaimDirective::Replace(proposal(30));
                // Replacement without authority, authority on a non-replacing row.
                assert!(locked.maintain_claims(&replace, None).await.is_err());
                for d in [ClaimDirective::Retain, ClaimDirective::ReleaseAll] {
                    assert!(
                        locked
                            .maintain_claims(&d, Some(&authority(2)))
                            .await
                            .is_err()
                    );
                }
                // A proposal under a payer the authorized arrangement does not carry.
                let foreign = ClaimDirective::Replace(proposal(999));
                assert!(
                    locked
                        .maintain_claims(&foreign, Some(&authority(2)))
                        .await
                        .is_err()
                );
                // Claims are taken for a candidate above the current version only.
                assert!(
                    locked
                        .maintain_claims(&replace, Some(&authority(1)))
                        .await
                        .is_err()
                );
                anyhow::Ok(())
            })
        })
        .await;
    result?;
    assert_eq!(live(&pg, 1).await?, vec![(u(30), "k".to_owned())]);
    assert_eq!(rows(&pg, 1).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn refusal_settles_and_audits_without_phantom_version_and_failures_roll_back()
-> anyhow::Result<()> {
    use super::audit::{ctx, evidence};
    use crate::domain::idempotency::{
        DraftRevisionInput, FingerprintAxes, FingerprintInput, FingerprintTarget, LeaseDuration,
        PrincipalScope, RegistryKey, RegistryOperation, Settlement, StoredResponse,
    };
    use crate::infra::storage::repo::idempotency::{
        self as reg, Durability, Gate, GateRequest, GateTarget, ReplayOutcome,
    };
    use crate::infra::storage::scoped::PrivateScope;
    use bss_orders_lifecycle_sdk::models::IdempotencyKey;
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for id in 1..=2 {
        create_for(&db, id, 30, 10).await?;
    }
    replace(&db, 2, 30, &["m"], 2, &[2]).await?;
    replace(&db, 1, 30, &["a"], 2, &[1]).await?;
    let principal = PrincipalScope::from_context(&ctx(40))?;
    let request = move |key: &str| {
        let k = RegistryKey::new(
            RegistryOperation::Trigger(Trigger::Amendment),
            principal.clone(),
            IdempotencyKey::try_from(key.to_owned()).unwrap(),
        );
        let doc = serde_json::json!({"overlap_scope_keys": ["a", "b", "m"]});
        let f = FingerprintInput {
            operation: "amend",
            trigger: Trigger::Amendment,
            target: FingerprintTarget::Order {
                order_id: u(1),
                expected_version: 1,
            },
            axes: FingerprintAxes {
                seller_tenant_id: u(20),
                resource_tenant_id: u(10),
                payer_tenant_id: u(30),
            },
            draft_revision: DraftRevisionInput::NotApplicable,
            contribution: &doc,
        }
        .fingerprint()
        .unwrap();
        (k, f)
    };
    let response = || {
        StoredResponse::new(
            409,
            serde_json::json!({"reason": "order-in-flight-for-key"}),
            std::collections::BTreeMap::new(),
            None,
        )
        .unwrap()
    };
    // One attempt: step 17 refuses, the refusal is audited and settled, then the transaction
    // commits as a business outcome. `fail` injects a failure after each later boundary.
    let attempt = |key: &'static str, fail: u8| {
        let db = db.clone();
        let (k, f) = request(key);
        async move {
            db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 1).await?;
                    let scope_p = PrivateScope::for_principal(k.principal().as_str());
                    let req = GateRequest {
                        key: &k,
                        fingerprint: &f,
                        lease: LeaseDuration::from_seconds(30)?,
                        durability: Durability::Ordinary,
                        presented: None,
                    };
                    let owned =
                        match reg::resolve(GateTarget::Order(&locked), &scope_p, &req).await? {
                            Gate::Owned(owned) => owned,
                            Gate::Replay(replay) => return anyhow::Ok(Err(replay)),
                            other => anyhow::bail!("unexpected gate {other:?}"),
                        };
                    let mut proposed = locked.row().clone();
                    proposed.payer_tenant_id = u(30);
                    let proposal =
                        ProposedClaims::new(u(30), u(10), keys(&owned_keys(&["a", "b", "m"])))?;
                    let directive = ClaimDirective::for_transition(
                        Trigger::Amendment,
                        OrderState::Submitted,
                        Some(proposal),
                    )?;
                    let s = scope(1);
                    let authority = ProposalAuthority {
                        proposed: &proposed,
                        proposed_scope: &s,
                        disclosure_scope: &s,
                        proposed_version: 2,
                    };
                    let ClaimOutcome::Conflict(conflict) =
                        locked.maintain_claims(&directive, Some(&authority)).await?
                    else {
                        anyhow::bail!("expected a collision");
                    };
                    if fail == 1 {
                        anyhow::bail!("injected failure after provisional release");
                    }
                    let sealed = evidence(40).resolved_refusal(
                        Uuid::new_v4(),
                        AuditTrigger::Public(Trigger::Amendment),
                        &locked.audit_facts()?,
                        OverlapConflict::REASON,
                    )?;
                    let sealed = writer::append_resolved_refusal(&locked, sealed).await?;
                    if fail == 2 {
                        anyhow::bail!("injected failure after refusal audit");
                    }
                    let settled = owned
                        .settle(Settlement::Refused {
                            reason: OverlapConflict::REASON,
                            audit_id: sealed.row().audit_id,
                            response: response(),
                        })
                        .await?;
                    if fail == 3 {
                        anyhow::bail!("injected failure after settlement");
                    }
                    anyhow::Ok(Ok((conflict, sealed.row().audit_id, settled.audit_id)))
                })
            })
            .await
        }
    };
    let start = baseline(&pg).await?;
    for fail in 1..=3 {
        assert!(attempt("amend-fail", fail).await.is_err());
        // Provisional inserts, their release, audit and settlement all vanish together.
        assert_eq!(baseline(&pg).await?, start, "failure point {fail}");
    }
    let (c, audit_id, settled_audit) = attempt("amend-1", 0).await?.unwrap();
    assert_eq!(settled_audit, Some(audit_id));
    assert_eq!(c.provisional_released.len(), 1);
    // No phantom version, pointer move, line, total or event contribution.
    assert_eq!(
        pg.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{}'",
            u(1)
        ))
        .await?,
        1
    );
    assert_eq!(
        pg.scalar(&format!(
            "SELECT current_version::bigint AS n FROM bss_orders__order WHERE order_id='{}'",
            u(1)
        ))
        .await?,
        1
    );
    for table in ["order_line", "resolved_total", "gate_outcome"] {
        assert_eq!(
            pg.scalar(&format!("SELECT count(*) AS n FROM bss_orders__{table}"))
                .await?,
            0
        );
    }
    assert_eq!(
        pg.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE audit_id='{audit_id}' \
             AND outcome='refused' AND reason='order-in-flight-for-key' AND sequence IS NULL"
        ))
        .await?,
        1
    );
    // Prior claims intact; the provisional b is released history, not a live reservation.
    assert_eq!(live(&pg, 1).await?, vec![(u(30), "a".to_owned())]);
    // Same-key replay returns the settled refusal without touching claims.
    let claims_before = pg
        .scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
        .await?;
    let replay = attempt("amend-1", 0).await?.unwrap_err();
    assert_eq!(
        replay.outcome,
        ReplayOutcome::Refused("order-in-flight-for-key".into())
    );
    assert_eq!(replay.audit_id, Some(audit_id));
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
            .await?,
        claims_before
    );
    Ok(())
}
fn owned_keys(ks: &[&str]) -> Vec<String> {
    owned(ks)
}
async fn baseline(pg: &Pg) -> anyhow::Result<(Vec<Row>, i64, i64)> {
    Ok((
        rows(pg, 1).await?,
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await?,
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE outcome='refused'")
            .await?,
    ))
}

#[tokio::test]
async fn failure_after_acquisition_and_release_leaves_no_partial_amendment() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create_for(&db, 1, 30, 10).await?;
    replace(&db, 1, 30, &["k"], 2, &[1]).await?;
    let before = rows(&pg, 1).await?;
    let failed: anyhow::Result<()> = db
        .transaction_ref_mapped_with_config(transition_tx_config(), |tx| {
            Box::pin(async move {
                let out = replace_in(tx, 1, 31, &owned(&["k", "k2"]), 3, &[1]).await?;
                assert_eq!(admitted(&out), (0, 2, 1));
                anyhow::bail!("version append failed after step 17")
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(rows(&pg, 1).await?, before);
    Ok(())
}

#[tokio::test]
async fn every_terminal_transition_releases_all_live_claims_across_payer_history()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let cases = [
        (Trigger::AcknowledgeCompleted, OrderState::Completed),
        (Trigger::ReflectApprovalDenied, OrderState::Rejected),
        (Trigger::Cancel, OrderState::Cancelled),
        (Trigger::CancelWorkflowMediated, OrderState::Cancelled),
        (Trigger::AcknowledgeFailed, OrderState::FulfillmentFailed),
        // D-182: the forced terminal releases Orders claims through the ordinary rule.
        (
            Trigger::ForceFailUnreconciled,
            OrderState::FulfillmentFailed,
        ),
        (Trigger::Expire, OrderState::Expired),
        (Trigger::AutoVoid, OrderState::Expired),
    ];
    assert!(
        TERMINAL_STATES
            .iter()
            .all(|s| cases.iter().any(|(_, t)| t == s))
    );
    for (n, (trigger, target)) in cases.into_iter().enumerate() {
        let id = 100 + n as u128;
        create_for(&db, id, 30, 10).await?;
        let k1 = format!("t{n}-a");
        let k2 = format!("t{n}-b");
        replace(&db, id, 30, &[k1.as_str(), k2.as_str()], 2, &[id]).await?;
        // Payer history: the old payer's tuples are released history, the new ones live.
        replace(&db, id, 31, &[k1.as_str(), k2.as_str()], 3, &[id]).await?;
        let history: Vec<_> = rows(&pg, id)
            .await?
            .into_iter()
            .filter(|r| !r.live)
            .collect();
        assert_eq!(history.len(), 2);
        // Non-terminal rows retain.
        let held = rows(&pg, id).await?;
        assert_eq!(
            terminal(&db, id, Trigger::Hold, OrderState::OnHold).await?,
            ClaimOutcome::Retained
        );
        assert_eq!(rows(&pg, id).await?, held);
        let ClaimOutcome::Released(mut released) = terminal(&db, id, trigger, target).await? else {
            panic!("terminal {trigger:?} did not release");
        };
        let mut expected: Vec<_> = held.iter().filter(|r| r.live).map(|r| r.claim_id).collect();
        released.sort();
        expected.sort();
        assert_eq!(released, expected, "{trigger:?}");
        assert!(
            live(&pg, id).await?.is_empty(),
            "{trigger:?} leaked a live claim"
        );
        // History rows are untouched (released_at is set exactly once).
        let after = rows(&pg, id).await?;
        assert!(history.iter().all(|r| after.contains(r)));
        assert_eq!(
            pg.scalar(&format!(
                "SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{}' \
                 AND released_at >= claimed_at",
                u(id)
            ))
            .await?,
            4
        );
        // A second terminal pass has nothing live and releases nothing.
        assert_eq!(
            terminal(&db, id, trigger, target).await?,
            ClaimOutcome::Released(vec![])
        );
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_isolation_is_refused_before_any_claim_and_read_committed_reports_a_shortfall()
-> anyhow::Result<()> {
    use sea_orm::{IsolationLevel, TransactionTrait};
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for id in 1..=4 {
        create_for(&db, id, 30, 10).await?;
    }
    // Control on the owner connection, outside the product path: why step 17 needs READ
    // COMMITTED. A snapshot transaction offering a tuple committed after its snapshot gets a
    // serialization failure, so the collision could not be reported as a shortfall.
    let snapshot = pg
        .raw
        .begin_with_config(Some(IsolationLevel::RepeatableRead), None)
        .await?;
    snapshot.execute_unprepared("SELECT 1").await?;
    replace(&db, 2, 30, &["k"], 2, &[2]).await?;
    let raw = snapshot
        .execute_unprepared(&format!(
            "INSERT INTO bss_orders__inflight_overlap_claim VALUES (gen_random_uuid(),'{}','{}','k','{}',2,clock_timestamp(),NULL) \
             ON CONFLICT (payer_tenant_id,resource_tenant_id,overlap_scope_key) WHERE released_at IS NULL DO NOTHING",
            u(30),
            u(10),
            u(1)
        ))
        .await;
    assert!(
        raw.as_ref()
            .is_err_and(|e| e.to_string().contains("could not serialize")),
        "{raw:?}"
    );
    snapshot.rollback().await?;
    // Product path: the repository refuses every snapshot level before its first claim
    // statement, for acquisition and terminal release alike, and writes nothing.
    let before = pg
        .scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
        .await?;
    for level in [
        TxIsolationLevel::RepeatableRead,
        TxIsolationLevel::Serializable,
    ] {
        let acquire = db
            .transaction_ref_mapped_with_config(TxConfig::with_isolation(level), |tx| {
                Box::pin(async move { replace_in(tx, 1, 30, &owned(&["fresh"]), 2, &[1, 2]).await })
            })
            .await;
        let err = format!("{:#}", acquire.unwrap_err());
        assert!(err.contains("READ COMMITTED"), "{level:?}: {err}");
        let release = db
            .transaction_ref_mapped_with_config(TxConfig::with_isolation(level), |tx| {
                Box::pin(async move {
                    let locked = lock(tx, 2).await?;
                    anyhow::Ok(
                        locked
                            .maintain_claims(&ClaimDirective::ReleaseAll, None)
                            .await?,
                    )
                })
            })
            .await;
        let err = format!("{:#}", release.unwrap_err());
        assert!(err.contains("READ COMMITTED"), "{level:?}: {err}");
    }
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
            .await?,
        before
    );
    assert_eq!(live(&pg, 2).await?, vec![(u(30), "k".to_owned())]);
    // The same race at the provided READ COMMITTED config: A locks, B commits a competing
    // claim, then A offers and gets an authoritative shortfall naming the visible holder.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let a = {
        let db = db.clone();
        tokio::spawn(async move {
            db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                Box::pin(async move {
                    lock(tx, 3).await?;
                    ready_tx.send(()).ok();
                    go_rx.await?;
                    replace_in(tx, 3, 30, &owned(&["k2"]), 2, &[3, 4]).await
                })
            })
            .await
        })
    };
    ready_rx.await?;
    replace(&db, 4, 30, &["k2"], 2, &[4]).await?;
    go_tx.send(()).ok();
    let c = conflict(a.await??);
    assert_eq!(c.blocked[0].visible_holder, Some(u(4)));
    assert!(live(&pg, 1).await?.is_empty() && live(&pg, 3).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn shortfall_cleanup_error_or_count_mismatch_aborts_instead_of_refusing() -> anyhow::Result<()>
{
    // Fault seam on the owner connection: a BEFORE UPDATE trigger that raises for one key and
    // silently skips the row for another, so the exact-ID release inside step 17.5 fails or
    // affects fewer rows than this attempt inserted.
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for id in 1..=2 {
        create_for(&db, id, 30, 10).await?;
    }
    replace(&db, 2, 30, &["m"], 2, &[2]).await?;
    replace(&db, 1, 30, &["a"], 2, &[1]).await?;
    pg.sql(
        "CREATE FUNCTION s207_release_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.overlap_scope_key = 'raise' THEN RAISE EXCEPTION 'injected release error'; END IF; \
         IF NEW.overlap_scope_key = 'skip' THEN RETURN NULL; END IF; RETURN NEW; END $$; \
         CREATE TRIGGER s207_release_fault BEFORE UPDATE ON bss_orders__inflight_overlap_claim \
         FOR EACH ROW EXECUTE FUNCTION s207_release_fault();",
    )
    .await?;
    let before = (
        rows(&pg, 1).await?,
        rows(&pg, 2).await?,
        pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
            .await?,
    );
    for (poison, expected) in [
        ("raise", "injected release error"),
        ("skip", "claim release count mismatch"),
    ] {
        // `a` is held, `b` and the poisoned key are provisional, `m` is blocked by order 2.
        let out = replace(&db, 1, 30, &["a", "b", "m", poison], 3, &[1, 2]).await;
        let err = format!("{:#}", out.unwrap_err());
        assert!(err.contains(expected), "{poison}: {err}");
        // Rolled back as a whole: no provisional row, no release, held and blocking claims
        // unchanged. The failure is never reported as the business refusal.
        assert_eq!(
            (
                rows(&pg, 1).await?,
                rows(&pg, 2).await?,
                pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
                    .await?,
            ),
            before,
            "{poison}"
        );
    }
    pg.sql("DROP TRIGGER s207_release_fault ON bss_orders__inflight_overlap_claim")
        .await?;
    // Without the fault the same proposal is the ordinary refusal.
    let c = conflict(replace(&db, 1, 30, &["a", "b", "m", "skip"], 3, &[1, 2]).await?);
    assert_eq!(c.provisional_released.len(), 2);
    assert_eq!(live(&pg, 1).await?, vec![(u(30), "a".to_owned())]);
    Ok(())
}

#[tokio::test]
async fn three_way_rotated_baskets_complete_without_deadlock_and_one_winner() -> anyhow::Result<()>
{
    // Three orders offering the same three tuples in rotated caller order form a wait cycle if
    // acquisition were unsorted; the total order makes every round finish without any error.
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for round in 0..8u128 {
        let ids = [2000 + 3 * round, 2001 + 3 * round, 2002 + 3 * round];
        for id in ids {
            create_for(&db, id, 30, 10).await?;
        }
        let k = |s: &str| format!("w{round}-{s}");
        let baskets = [
            vec![k("x"), k("y"), k("z")],
            vec![k("y"), k("z"), k("x")],
            vec![k("z"), k("x"), k("y")],
        ];
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let run = |id: u128, ks: Vec<String>| {
            let db = db.clone();
            let barrier = barrier.clone();
            async move {
                db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                    Box::pin(async move {
                        lock(tx, id).await?;
                        barrier.wait().await;
                        replace_in(tx, id, 30, &ks, 2, &[id]).await
                    })
                })
                .await
            }
        };
        let [b0, b1, b2] = baskets;
        let (x, y, z) = tokio::join!(run(ids[0], b0), run(ids[1], b1), run(ids[2], b2));
        let outcomes = [x?, y?, z?];
        let winners: Vec<usize> = outcomes
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, ClaimOutcome::Admitted { .. }))
            .map(|(n, _)| n)
            .collect();
        assert_eq!(winners.len(), 1, "round {round}: {outcomes:?}");
        let mut expected: Vec<_> = ["x", "y", "z"].iter().map(|s| (u(30), k(s))).collect();
        expected.sort();
        for (n, id) in ids.iter().enumerate() {
            let held = live(&pg, *id).await?;
            if n == winners[0] {
                assert_eq!(held, expected, "round {round}");
            } else {
                assert!(held.is_empty(), "round {round}: loser {id} holds {held:?}");
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn concurrent_amendments_of_one_order_serialize_without_leaking_claims() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create_for(&db, 1, 30, 10).await?;
    replace(&db, 1, 30, &["k"], 2, &[1]).await?;
    for round in 0..5u128 {
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let amend = |payer: u128, ks: Vec<String>| {
            let db = db.clone();
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                    Box::pin(async move {
                        let locked = lock(tx, 1).await?;
                        let candidate = locked.row().current_version + 1;
                        drop(locked);
                        replace_in(tx, 1, payer, &ks, candidate, &[1]).await
                    })
                })
                .await
            }
        };
        let (a, b) = tokio::join!(
            amend(40 + round, owned(&["k", "x"])),
            amend(50 + round, owned(&["y"]))
        );
        a?;
        b?;
        // Exactly one complete proposal is live: the later committer's.
        let live_now = live(&pg, 1).await?;
        assert!(
            live_now
                == vec![
                    (u(40 + round), "k".to_owned()),
                    (u(40 + round), "x".to_owned())
                ]
                || live_now == vec![(u(50 + round), "y".to_owned())],
            "{live_now:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn unsorted_acquisition_deadlocks_which_the_total_order_prevents() -> anyhow::Result<()> {
    use sea_orm::TransactionTrait;
    // Control on the owner connection (not the product path): opposite-order raw inserts of
    // the same two tuples form a lock cycle that only PostgreSQL's detector breaks.
    let pg = Pg::new().await?;
    create(&pg.db, 1).await?;
    create(&pg.db, 2).await?;
    let insert = |order: u128, key: &str| {
        format!(
            "INSERT INTO bss_orders__inflight_overlap_claim VALUES (gen_random_uuid(),'{}','{}','{key}','{}',2,clock_timestamp(),NULL) \
             ON CONFLICT (payer_tenant_id,resource_tenant_id,overlap_scope_key) WHERE released_at IS NULL DO NOTHING",
            u(30),
            u(10),
            u(order)
        )
    };
    let t1 = pg.raw.begin().await?;
    let t2 = pg.raw.begin().await?;
    t1.execute_unprepared(&insert(1, "a")).await?;
    t2.execute_unprepared(&insert(2, "b")).await?;
    let (first, second) = (insert(1, "b"), insert(2, "a"));
    let (r1, r2) = tokio::join!(t1.execute_unprepared(&first), async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        t2.execute_unprepared(&second).await
    });
    let errors: Vec<String> = [r1.err(), r2.err()]
        .into_iter()
        .flatten()
        .map(|e| e.to_string())
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("deadlock"), "{errors:?}");
    t1.rollback().await?;
    t2.rollback().await?;
    Ok(())
}

/// Shared with `constraints::concurrent_overlap_claims_have_exactly_one_winner`.
pub(super) async fn contend(
    db: &Db,
    a: (u128, Vec<String>),
    b: (u128, Vec<String>),
) -> anyhow::Result<(ClaimOutcome, ClaimOutcome)> {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let run = |(id, ks): (u128, Vec<String>)| {
        let db = db.clone();
        let barrier = barrier.clone();
        async move {
            db.transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                Box::pin(async move {
                    lock(tx, id).await?;
                    barrier.wait().await;
                    replace_in(tx, id, 30, &ks, 2, &[id]).await
                })
            })
            .await
        }
    };
    let (x, y) = tokio::join!(run(a), run(b));
    Ok((x?, y?))
}
