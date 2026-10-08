//! S2-03 on real PostgreSQL: the shared PEP, the real rules provider (and a recording PDP for
//! outage/invalid-response cases) and the authorization-bound storage adapters.
use super::*;
use crate::authz::test_pdp::{RulesProvider, ScriptedPdp, allow, eq, path, pep, service, user};
use crate::authz::{
    Action, Arrangement, AuthzFailure, Caller, Pep, Prefetch, ProofDenial, TargetAuthorization,
};
use crate::infra::storage::scoped::{self, AuthorizedLock, AuthorizedRead};
use bss_orders_lifecycle_sdk::catalog::Reason;
use serde_json::json;

const ORDER_RT: &str = "gts.cf.bss.orders.order.v1~";

fn s(n: u128) -> String {
    u(n).to_string()
}
type Predicates<'a> = &'a [(&'a str, &'a [u128])];
fn rule(id: &str, subject: u128, actions: &[&str], paths: &[Predicates<'_>]) -> serde_json::Value {
    json!({
        "id": id,
        "subject": {"id": s(subject)},
        "resource_type": ORDER_RT,
        "actions": actions,
        "paths": paths.iter().map(|p| json!({"predicates": p.iter().map(|(property, values)| json!({
            "property": property, "values": values.iter().map(|v| s(*v)).collect::<Vec<_>>()
        })).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    })
}
fn with(mut rule: serde_json::Value, key: &str, value: serde_json::Value) -> serde_json::Value {
    rule[key] = value;
    rule
}

// Subjects (tenant in parentheses): customer C 102 (10), same-tenant C2 103 (10), partner P
// 101 (10), delegated-only partner D 109 (60), seller S 104 (20), payer reader R 105 (30),
// Workflow W 106 (99, service), misconfigured service M 107 (99), member-only U 108 (10).
fn policy(accepted_proofs: &[&str]) -> serde_json::Value {
    let delegation = json!({"proof_property": "delegation_proof_ref", "accepted": accepted_proofs});
    let mut rules = vec![
        with(
            rule(
                "customer",
                102,
                &["create", "write", "read", "cancel"],
                &[&[("resource_tenant_id", &[10])]],
            ),
            "payer_use",
            json!({"property": "payer_tenant_id", "values": [s(30)]}),
        ),
        rule(
            "customer-read-only",
            103,
            &["read"],
            &[&[("resource_tenant_id", &[10])]],
        ),
        with(
            rule(
                "partner-direct",
                101,
                &["create", "write", "read"],
                &[&[("resource_tenant_id", &[10])]],
            ),
            "payer_use",
            json!({"property": "payer_tenant_id", "values": [s(30), s(31)]}),
        ),
        rule(
            "seller",
            104,
            &["read", "hold", "resume", "cancel"],
            &[&[("seller_tenant_id", &[20])]],
        ),
        rule(
            "payer-reader",
            105,
            &["read"],
            &[&[("payer_tenant_id", &[30])]],
        ),
        rule(
            "workflow",
            106,
            &["read", "begin_fulfillment"],
            &[&[("id", &[1]), ("seller_tenant_id", &[20])]],
        ),
        rule(
            "tenant-only-service",
            107,
            &["read"],
            &[&[("seller_tenant_id", &[20])]],
        ),
    ];
    if !accepted_proofs.is_empty() {
        rules.push(with(
            rule(
                "partner-delegated",
                101,
                &["read", "write"],
                &[&[("resource_tenant_id", &[11])]],
            ),
            "delegation",
            delegation.clone(),
        ));
        rules.push(with(
            rule(
                "delegated-only",
                109,
                &["create", "read", "write"],
                &[&[("resource_tenant_id", &[11])]],
            ),
            "delegation",
            delegation,
        ));
    }
    json!({"vendor": "constructorfabric", "priority": 10, "policy_revision": "s2-03-matrix-1", "rules": rules})
}
fn rules_pep(accepted_proofs: &[&str]) -> Pep {
    pep(RulesProvider::from_policy(policy(accepted_proofs)))
}
fn proof(caller: &Caller, value: &str) -> Caller {
    crate::authz::test_pdp::with_proof(caller, value)
}

fn arrangement_of(r: u128, s: u128, p: u128) -> Arrangement {
    Arrangement {
        resource_tenant_id: u(r),
        seller_tenant_id: u(s),
        payer_tenant_id: u(p),
    }
}
/// O1 (10,20,30), O2 (11,20,31), O3 (12,21,30) through the restricted runtime role.
async fn seed(pg: &Pg) -> anyhow::Result<Db> {
    let (runtime, _) = pg.role("runtime").await?;
    for (id, a) in [
        (1, arrangement_of(10, 20, 30)),
        (2, arrangement_of(11, 20, 31)),
        (3, arrangement_of(12, 21, 30)),
    ] {
        let mut row = order(id);
        row.resource_tenant_id = a.resource_tenant_id;
        row.audit_tenant_id = a.resource_tenant_id;
        row.seller_tenant_id = a.seller_tenant_id;
        row.payer_tenant_id = a.payer_tenant_id;
        let mut first = version(id, 1, None);
        first.payer_tenant_id = a.payer_tenant_id;
        let scope = toolkit_security::AccessScope::for_resources(vec![u(id)]);
        runtime
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let row = repo::insert_order(tx, &scope, row).await?;
                    let locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                    locked.insert_order_version(first).await?;
                    anyhow::Ok(())
                })
            })
            .await?;
    }
    Ok(runtime)
}
async fn visible(db: &Db, pep: &Pep, caller: &Caller) -> Result<Vec<u128>, AuthzFailure> {
    let scope = pep.authorize_collection(caller, Action::OrderRead).await?;
    let conn = db.conn().unwrap();
    let mut found = vec![];
    for id in [1, 2, 3] {
        if scoped::find_in_collection(&conn, &scope, u(id))
            .await
            .unwrap()
            .is_some()
        {
            found.push(id);
        }
    }
    Ok(found)
}
async fn target(
    db: &Db,
    pep: &Pep,
    caller: &Caller,
    action: Action,
    id: u128,
) -> Result<TargetAuthorization, AuthzFailure> {
    let prefetch = scoped::prefetch(db, u(id)).await.unwrap();
    pep.authorize_target(caller, action, &prefetch).await
}
fn reason<T: std::fmt::Debug>(result: Result<T, AuthzFailure>) -> (Reason, Option<ProofDenial>) {
    match result {
        Err(AuthzFailure::Refused { reason, proof }) => (reason, proof),
        other => panic!("expected refusal, got {other:?}"),
    }
}
async fn row(pg: &Pg, id: u128) -> String {
    let sql = format!(
        "SELECT row_to_json(o)::text AS j FROM bss_orders__order o WHERE order_id='{}'",
        u(id)
    );
    pg.raw
        .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "j")
        .unwrap()
}

/// Actor/axis matrix against the real rules provider and real PostgreSQL scopes.
#[tokio::test]
async fn actor_axis_matrix_on_real_provider_and_postgres() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&["proof-p"]);
    // Collections: each complete path applies to the current aggregate axes.
    assert_eq!(visible(&runtime, &pep, &user(102, 10)).await, Ok(vec![1]));
    assert_eq!(visible(&runtime, &pep, &user(103, 10)).await, Ok(vec![1]));
    assert_eq!(visible(&runtime, &pep, &user(101, 10)).await, Ok(vec![1]));
    assert_eq!(
        visible(&runtime, &pep, &proof(&user(101, 10), "proof-p")).await,
        Ok(vec![1, 2])
    );
    assert_eq!(
        visible(&runtime, &pep, &user(104, 20)).await,
        Ok(vec![1, 2])
    );
    assert_eq!(
        visible(&runtime, &pep, &user(105, 30)).await,
        Ok(vec![1, 3])
    );
    assert_eq!(
        visible(&runtime, &pep, &service(106, 99)).await,
        Ok(vec![1])
    );
    // A tenant-only service grant is insufficient: integration failure, never a broad read.
    assert_eq!(
        visible(&runtime, &pep, &service(107, 99)).await,
        Err(AuthzFailure::Integration)
    );
    // Tenant membership alone grants nothing.
    assert_eq!(
        reason(visible(&runtime, &pep, &user(108, 10)).await).0,
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(
        reason(target(&runtime, &pep, &user(108, 10), Action::OrderRead, 1).await).0,
        Reason::OrderNotFound
    );

    // Targeted actions: complete paths only, 403 only where the caller can read the target.
    let seller = user(104, 20);
    assert!(
        target(&runtime, &pep, &seller, Action::OrderHold, 1)
            .await
            .is_ok()
    );
    assert_eq!(
        reason(target(&runtime, &pep, &seller, Action::OrderWrite, 1).await).0,
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(
        reason(target(&runtime, &pep, &seller, Action::OrderPreview, 1).await).0,
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(
        reason(target(&runtime, &pep, &seller, Action::OrderWrite, 3).await).0,
        Reason::OrderNotFound
    );
    // Same tenant, different grants.
    assert!(
        target(&runtime, &pep, &user(102, 10), Action::OrderWrite, 1)
            .await
            .is_ok()
    );
    assert_eq!(
        reason(target(&runtime, &pep, &user(103, 10), Action::OrderWrite, 1).await).0,
        Reason::OperationNotPermittedForActor
    );
    // Payer reader: reads only; no cancel, no audit.
    assert!(
        target(&runtime, &pep, &user(105, 30), Action::OrderRead, 3)
            .await
            .is_ok()
    );
    assert_eq!(
        reason(target(&runtime, &pep, &user(105, 30), Action::OrderCancel, 1).await).0,
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(
        reason(target(&runtime, &pep, &user(105, 30), Action::AuditRead, 1).await).0,
        Reason::OperationNotPermittedForActor
    );
    // Workflow: only its finite granted order; another order of the same seller is hidden.
    assert!(
        target(
            &runtime,
            &pep,
            &service(106, 99),
            Action::OrderBeginFulfillment,
            1
        )
        .await
        .is_ok()
    );
    assert_eq!(
        reason(
            target(
                &runtime,
                &pep,
                &service(106, 99),
                Action::OrderBeginFulfillment,
                2
            )
            .await
        )
        .0,
        Reason::OrderNotFound
    );
    // Missing order: the same not-found as a hidden one.
    assert_eq!(
        reason(target(&runtime, &pep, &seller, Action::OrderHold, 77).await).0,
        Reason::OrderNotFound
    );

    // An authorized point read discloses only the current row under the compiled scope.
    let granted = target(&runtime, &pep, &user(105, 30), Action::OrderRead, 1)
        .await
        .unwrap();
    assert!(matches!(
        scoped::read_authorized(&runtime.conn()?, &granted).await?,
        AuthorizedRead::Current(_)
    ));
    Ok(())
}

/// Check-both, payer use, immutable seller, create proposal and stale facts on PostgreSQL.
#[tokio::test]
async fn old_new_arrangement_payer_use_and_stale_facts() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&["proof-p"]);
    let before = row(&pg, 1).await;

    // Old allowed, new payer denied (customer has payer use for 30 only): 403, no change.
    let customer = user(102, 10);
    let current = target(&runtime, &pep, &customer, Action::OrderWrite, 1)
        .await
        .unwrap();
    let proposed = current.facts().arrangement().with_delta(None, Some(u(31)));
    assert_eq!(
        reason(pep.authorize_proposed(&customer, &current, proposed).await).0,
        Reason::OperationNotPermittedForActor
    );
    // Old denied (payer reader has no write): the proposal is never reached.
    assert_eq!(
        reason(target(&runtime, &pep, &user(105, 30), Action::OrderWrite, 1).await).0,
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(row(&pg, 1).await, before);

    // Both allowed (partner with payer use 30/31): the authorized arrangement is persisted.
    let partner = user(101, 10);
    let current = target(&runtime, &pep, &partner, Action::OrderWrite, 1)
        .await
        .unwrap();
    let new_side = pep
        .authorize_proposed(&partner, &current, proposed)
        .await
        .unwrap();
    // A seller change is never part of a delta and is refused even with both grants.
    let mut seller_changed = order(1);
    seller_changed.seller_tenant_id = u(21);
    seller_changed.payer_tenant_id = u(31);
    let (c, n) = (current.clone(), new_side.clone());
    let refused: anyhow::Result<()> = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let AuthorizedLock::Locked(mut locked) = scoped::lock_authorized(tx, &c).await?
                else {
                    anyhow::bail!("expected lock");
                };
                scoped::apply_authorized_arrangement(&mut locked, &n, seller_changed).await?;
                Ok(())
            })
        })
        .await;
    assert!(refused.is_err());
    assert_eq!(row(&pg, 1).await, before);
    let mut accepted = order(1);
    accepted.payer_tenant_id = u(31);
    let (c, n) = (current.clone(), new_side.clone());
    runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let AuthorizedLock::Locked(mut locked) = scoped::lock_authorized(tx, &c).await?
                else {
                    anyhow::bail!("expected lock");
                };
                scoped::apply_authorized_arrangement(&mut locked, &n, accepted).await?;
                Ok(())
            })
        })
        .await?;
    assert_eq!(
        scoped::prefetch(&runtime, u(1)).await?,
        Prefetch::Found(crate::authz::AuthorizationFacts::observed(
            u(1),
            arrangement_of(10, 20, 31)
        ))
    );

    // The earlier customer decision is now stale: conflict without mutation or key change.
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,order_id,request_fingerprint,status,execution_id,fencing_generation,lease_expires_at,created_at,expires_at) VALUES('draft-mutate','{}','k1','{}','fp','in_flight','{}',0,now()+interval '1 hour',now(),now()+interval '1 day')", u(102), u(1), u(700))).await?;
    let registry = |pg: &Pg| {
        let sql = "SELECT row_to_json(i)::text AS j FROM bss_orders__idempotency i".to_owned();
        let raw = pg.raw.clone();
        async move {
            raw.query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
                .await
                .unwrap()
                .unwrap()
                .try_get::<String>("", "j")
                .unwrap()
        }
    };
    let registry_before = registry(&pg).await;
    let order_before = row(&pg, 1).await;
    let stale = current; // partner decision taken on payer 30
    let outcome = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(match scoped::lock_authorized(tx, &stale).await? {
                    AuthorizedLock::Stale(s) => Some((s.reason(Some(1)), s.reason(Some(2)))),
                    _ => None,
                })
            })
        })
        .await?;
    assert_eq!(
        outcome,
        Some((Reason::AuthorizationContextChanged, Reason::VersionConflict))
    );
    assert_eq!(registry(&pg).await, registry_before);
    assert_eq!(row(&pg, 1).await, order_before);

    // Former payer loses access to the current order after reassignment (fresh request).
    let reader = user(105, 30);
    assert_eq!(
        reason(target(&runtime, &pep, &reader, Action::OrderRead, 1).await).0,
        Reason::OrderNotFound
    );
    Ok(())
}

#[tokio::test]
async fn create_proposals_require_the_complete_authorized_arrangement() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&["proof-p"]);
    let customer = user(102, 10);
    // Unauthorized payer on create: refused before any insert.
    assert_eq!(
        reason(
            pep.authorize_new_arrangement(
                &customer,
                Action::OrderCreate,
                arrangement_of(10, 20, 31)
            )
            .await
        )
        .0,
        Reason::OperationNotPermittedForActor
    );
    let granted = pep
        .authorize_new_arrangement(&customer, Action::OrderCreate, arrangement_of(10, 20, 30))
        .await
        .unwrap();
    let mut wrong_payer = order(9);
    wrong_payer.payer_tenant_id = u(31);
    assert!(scoped::admit_proposed(&granted, &wrong_payer).is_err());
    let created = order(9);
    scoped::admit_proposed(&granted, &created)?;
    let (g, w) = (granted.clone(), wrong_payer);
    let refused: anyhow::Result<()> = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                scoped::insert_authorized_order(tx, &g, w).await?;
                Ok(())
            })
        })
        .await;
    assert!(refused.is_err());
    let mut first = version(9, 1, None);
    first.payer_tenant_id = u(30);
    runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let row = scoped::insert_authorized_order(tx, &granted, created).await?;
                let scope = toolkit_security::AccessScope::for_resources(vec![row.order_id]);
                repo::LockedOrder::acquire(tx, &scope, &row)
                    .await?
                    .insert_order_version(first)
                    .await?;
                anyhow::Ok(())
            })
        })
        .await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        4
    );
    Ok(())
}

/// Proof absence/invalidity/revocation: disclosed only on untargeted requests (D-141).
#[tokio::test]
async fn delegation_proof_denial_and_revocation() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&["proof-p"]);
    let delegate = user(109, 60);
    let with_proof = proof(&delegate, "proof-p");
    assert_eq!(visible(&runtime, &pep, &with_proof).await, Ok(vec![2]));
    assert!(
        target(&runtime, &pep, &with_proof, Action::OrderRead, 2)
            .await
            .is_ok()
    );
    // Untargeted: proof reasons are disclosed.
    assert_eq!(
        reason(visible(&runtime, &pep, &delegate).await),
        (Reason::DelegationProofRequired, Some(ProofDenial::Required))
    );
    assert_eq!(
        reason(
            pep.authorize_new_arrangement(
                &delegate,
                Action::OrderCreate,
                arrangement_of(11, 20, 31)
            )
            .await
        )
        .0,
        Reason::DelegationProofRequired
    );
    // Targeted: always not-found, with the classified detail kept operational-only.
    assert_eq!(
        reason(target(&runtime, &pep, &delegate, Action::OrderWrite, 2).await),
        (Reason::OrderNotFound, Some(ProofDenial::Required))
    );
    // Revocation is observed at the next decision.
    let revoked = rules_pep(&["other-proof"]);
    assert_eq!(
        reason(visible(&runtime, &revoked, &with_proof).await),
        (Reason::DelegationProofInvalid, Some(ProofDenial::Invalid))
    );
    assert_eq!(
        reason(target(&runtime, &revoked, &with_proof, Action::OrderRead, 2).await),
        (Reason::OrderNotFound, Some(ProofDenial::Invalid))
    );
    // An independently complete direct path still authorizes despite an invalid proof.
    assert_eq!(
        visible(&runtime, &revoked, &proof(&user(101, 10), "proof-p")).await,
        Ok(vec![1])
    );
    Ok(())
}

/// PDP outage and invalid constraints fail closed with no business or registry effects.
#[tokio::test]
async fn pdp_outage_and_invalid_constraints_have_no_effects() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let before = row(&pg, 1).await;
    let down = pep(ScriptedPdp::new(|_| None));
    for id in [1, 77] {
        for action in [Action::OrderRead, Action::OrderWrite, Action::OrderCancel] {
            assert!(matches!(
                target(&runtime, &down, &user(102, 10), action, id).await,
                Err(AuthzFailure::Unavailable)
            ));
        }
    }
    assert!(matches!(
        down.authorize_new_arrangement(
            &user(102, 10),
            Action::OrderCreate,
            arrangement_of(10, 20, 30)
        )
        .await,
        Err(AuthzFailure::Unavailable)
    ));
    let invalid = pep(ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![eq("owner_tenant_id", u(10))])]))
    }));
    assert!(matches!(
        target(&runtime, &invalid, &user(102, 10), Action::OrderWrite, 1).await,
        Err(AuthzFailure::Integration)
    ));
    assert_eq!(row(&pg, 1).await, before);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        3
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await?,
        0
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await?,
        0
    );
    Ok(())
}

/// Facts that changed between prefetch and the scoped re-read restart authorization.
#[tokio::test]
async fn prefetch_returns_facts_only_and_changed_facts_are_not_disclosed() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    assert_eq!(
        scoped::prefetch(&runtime, u(77)).await?,
        Prefetch::Missing(u(77))
    );
    let pep = rules_pep(&[]);
    let reader = user(104, 20);
    let granted = target(&runtime, &pep, &reader, Action::OrderRead, 1)
        .await
        .unwrap();
    // A concurrent payer change commits between decision and re-read.
    let partner = user(101, 10);
    let current = target(&runtime, &pep, &partner, Action::OrderWrite, 1)
        .await
        .unwrap();
    let proposal = pep
        .authorize_proposed(&partner, &current, arrangement_of(10, 20, 31))
        .await
        .unwrap();
    let mut changed = order(1);
    changed.payer_tenant_id = u(31);
    runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let AuthorizedLock::Locked(mut locked) =
                    scoped::lock_authorized(tx, &current).await?
                else {
                    anyhow::bail!("expected lock");
                };
                scoped::apply_authorized_arrangement(&mut locked, &proposal, changed).await?;
                Ok(())
            })
        })
        .await?;
    assert!(matches!(
        scoped::read_authorized(&runtime.conn()?, &granted).await?,
        AuthorizedRead::FactsChanged
    ));
    Ok(())
}

/// S2-02 hand-off: a lock under the decided scope distinguishes lost access (non-disclosing
/// not-found) from changed facts (conflict), instead of conflating both as a denial.
#[tokio::test]
async fn lock_distinguishes_lost_access_from_stale_facts() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&["proof-p"]);
    // The customer's write decision is scoped to resource tenant 10.
    let customer = target(&runtime, &pep, &user(102, 10), Action::OrderWrite, 1)
        .await
        .unwrap();
    // A delegated partner moves the order's resource tenant 10 -> 11 (both sides authorized).
    let partner = proof(&user(101, 10), "proof-p");
    let current = target(&runtime, &pep, &partner, Action::OrderWrite, 1)
        .await
        .unwrap();
    let moved = arrangement_of(11, 20, 30);
    let proposal = pep
        .authorize_proposed(&partner, &current, moved)
        .await
        .unwrap();
    let mut changed = order(1);
    changed.resource_tenant_id = u(11);
    runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let AuthorizedLock::Locked(mut locked) =
                    scoped::lock_authorized(tx, &current).await?
                else {
                    anyhow::bail!("expected lock");
                };
                scoped::apply_authorized_arrangement(&mut locked, &proposal, changed).await?;
                Ok(())
            })
        })
        .await?;
    let before = row(&pg, 1).await;
    let outcome = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(match scoped::lock_authorized(tx, &customer).await? {
                    AuthorizedLock::AccessLost => "lost",
                    AuthorizedLock::Stale(_) => "stale",
                    AuthorizedLock::Locked(_) => "locked",
                })
            })
        })
        .await?;
    assert_eq!(outcome, "lost");
    assert_eq!(row(&pg, 1).await, before);
    // A fresh request by the customer is now the non-disclosing not-found.
    assert_eq!(
        reason(target(&runtime, &pep, &user(102, 10), Action::OrderWrite, 1).await).0,
        Reason::OrderNotFound
    );
    Ok(())
}

/// Refusal evidence uses the bounded private writer under the authenticated subject tenant,
/// without a target lookup or a caller audit grant; another subject tenant cannot be written.
#[tokio::test]
async fn refusal_evidence_is_bound_to_the_authenticated_subject_tenant() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (private, _) = pg.role("private").await?;
    let caller = user(102, 10);
    let own = scoped::PrivateScope::for_refusal(&caller);
    let foreign = scoped::PrivateScope::for_refusal(&user(102, 11));
    let row = super::constraints::audit(301); // refused, subject tenant 10, no order
    let attempt = |scope: scoped::PrivateScope, row: entity::transition_audit::Model| {
        let private = private.clone();
        async move {
            private
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        repo::private::insert_transition_audit(tx, scope.access_scope(), row)
                            .await?;
                        anyhow::Ok(())
                    })
                })
                .await
        }
    };
    assert!(attempt(foreign, row.clone()).await.is_err());
    attempt(own, row).await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE outcome='refused' AND order_id IS NULL")
            .await?,
        1
    );
    assert_eq!(
        scoped::principal_scope(&caller)?.as_str(),
        format!("{}/{}", u(10), u(102))
    );
    Ok(())
}

/// Gap review (S2-03): a real concurrent axis change. The writer holding the aggregate lock
/// blocks a request authorized on the old facts; once it commits, that request observes the
/// changed facts under its lock and conflicts, with no mutation and no registry change.
#[tokio::test]
async fn concurrent_axis_change_blocks_then_conflicts_without_mutation() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&[]);
    let partner = user(101, 10);
    // Both decisions are taken on payer 30 before either transaction starts.
    let stale = target(&runtime, &pep, &partner, Action::OrderWrite, 1)
        .await
        .unwrap();
    let mover = target(&runtime, &pep, &partner, Action::OrderWrite, 1)
        .await
        .unwrap();
    let proposal = pep
        .authorize_proposed(&partner, &mover, arrangement_of(10, 20, 31))
        .await
        .unwrap();
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,order_id,request_fingerprint,status,execution_id,fencing_generation,lease_expires_at,created_at,expires_at) VALUES('draft-mutate','{}','k1','{}','fp','in_flight','{}',0,now()+interval '1 hour',now(),now()+interval '1 day')", u(101), u(1), u(700))).await?;
    let registry_before = pg
        .scalar("SELECT count(*) AS n FROM bss_orders__idempotency WHERE status='in_flight' AND fencing_generation=0")
        .await?;

    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
    let raw = pg.raw.clone();
    let writer_db = runtime.clone();
    let writer = tokio::spawn(async move {
        writer_db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let AuthorizedLock::Locked(mut locked) =
                        scoped::lock_authorized(tx, &mover).await?
                    else {
                        anyhow::bail!("expected lock");
                    };
                    locked_tx.send(()).ok();
                    // Hold the lock until the stale request is observably waiting on it.
                    let mut waited = 0;
                    while waited < 200 {
                        let n: i64 = raw
                            .query_one_raw(Statement::from_string(
                                DbBackend::Postgres,
                                "SELECT count(*) AS n FROM pg_stat_activity WHERE wait_event_type='Lock'",
                            ))
                            .await?
                            .unwrap()
                            .try_get("", "n")?;
                        if n > 0 {
                            break;
                        }
                        waited += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    anyhow::ensure!(waited < 200, "the stale request never blocked on the lock");
                    let mut changed = order(1);
                    changed.payer_tenant_id = u(31);
                    scoped::apply_authorized_arrangement(&mut locked, &proposal, changed).await?;
                    Ok(())
                })
            })
            .await
    });
    locked_rx.await?;
    let after_writer = row(&pg, 1).await; // still the pre-change row: the writer has not committed
    let outcome = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(match scoped::lock_authorized(tx, &stale).await? {
                    AuthorizedLock::Stale(s) => Some(s.reason(None)),
                    AuthorizedLock::AccessLost => None,
                    AuthorizedLock::Locked(_) => anyhow::bail!("stale facts were locked"),
                })
            })
        })
        .await?;
    writer.await??;
    assert_eq!(outcome, Some(Reason::AuthorizationContextChanged));
    assert_ne!(
        row(&pg, 1).await,
        after_writer,
        "the writer committed first"
    );
    assert_eq!(
        scoped::prefetch(&runtime, u(1)).await?,
        Prefetch::Found(crate::authz::AuthorizationFacts::observed(
            u(1),
            arrangement_of(10, 20, 31)
        ))
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency WHERE status='in_flight' AND fencing_generation=0")
            .await?,
        registry_before
    );
    Ok(())
}

/// Gap review (S2-03): a PDP may answer with constraints only. On PostgreSQL, an allow whose
/// constraints exclude the prefetched order is a denial with the same calls as a missing order,
/// and a follow-up read scope that excludes it never yields a disclosing 403.
#[tokio::test]
async fn constraint_only_answers_never_disclose_hidden_orders() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let tenant_10 = pep(ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![eq("resource_tenant_id", u(10))])]))
    }));
    // O1 is inside the constraints: authorized and lockable.
    let granted = target(&runtime, &tenant_10, &user(102, 10), Action::OrderWrite, 1)
        .await
        .unwrap();
    let locked = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(matches!(
                    scoped::lock_authorized(tx, &granted).await?,
                    AuthorizedLock::Locked(_)
                ))
            })
        })
        .await?;
    assert!(locked);
    // O3 (resource tenant 12) and a nonexistent order answer identically.
    let recording =
        ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq("resource_tenant_id", u(10))])])));
    let tenant_10 = pep(recording.clone());
    assert_eq!(
        reason(target(&runtime, &tenant_10, &user(102, 10), Action::OrderWrite, 3).await),
        (Reason::OrderNotFound, None)
    );
    let hidden_calls = recording.calls();
    recording.requests.lock().clear();
    assert_eq!(
        reason(target(&runtime, &tenant_10, &user(102, 10), Action::OrderWrite, 77).await),
        (Reason::OrderNotFound, None)
    );
    assert_eq!(recording.calls().len(), hidden_calls.len());
    assert_eq!(hidden_calls.len(), 2, "write decision plus follow-up read");
    // A read scope that admits O3 (its seller) makes the excluded write a 403.
    let seller_reader = pep(ScriptedPdp::new(|r| {
        Some(if r.action.name == "read" {
            allow(vec![path(vec![eq("seller_tenant_id", u(21))])])
        } else {
            allow(vec![path(vec![eq("resource_tenant_id", u(10))])])
        })
    }));
    assert_eq!(
        reason(
            target(
                &runtime,
                &seller_reader,
                &user(104, 21),
                Action::OrderWrite,
                3
            )
            .await
        ),
        (Reason::OperationNotPermittedForActor, None)
    );
    Ok(())
}

/// S2-05: a stored outcome is disclosed only after the PEP rechecks current authority. A
/// create replay needs a fresh `order × read` on the created order; lost access is the
/// non-disclosing not-found and never a second create. Targeted replays recheck their own
/// action; a record bound to another order is never disclosed.
#[tokio::test]
async fn replay_disclosure_rechecks_current_authority_on_the_real_provider() -> anyhow::Result<()> {
    use crate::domain::idempotency::StoredResponse;
    use crate::infra::execution::disclose_replay;
    use crate::infra::storage::repo::idempotency::{Replay, ReplayOutcome};
    let pg = Pg::new().await?;
    let runtime = seed(&pg).await?;
    let pep = rules_pep(&[]);
    let stored = |order: Option<u128>| Replay {
        response: StoredResponse::new(
            201,
            json!({"orderId": order.map(u)}),
            std::collections::BTreeMap::new(),
            None,
        )
        .unwrap(),
        outcome: ReplayOutcome::Success,
        order_id: order.map(u),
        audit_id: Some(u(900)),
    };
    let customer = user(102, 10);
    // Create replay of a readable order discloses the stored body verbatim.
    let body = disclose_replay(
        &pep,
        &runtime,
        &customer,
        Action::OrderCreate,
        None,
        stored(Some(1)),
    )
    .await
    .unwrap();
    assert_eq!(body.body["orderId"], json!(u(1)));
    // Access lost (order no longer readable by this caller): not found, nothing disclosed.
    assert_eq!(
        reason(
            disclose_replay(
                &pep,
                &runtime,
                &customer,
                Action::OrderCreate,
                None,
                stored(Some(3))
            )
            .await
        )
        .0,
        Reason::OrderNotFound
    );
    // A refused create stored no order; its fresh create authorization was the check.
    let mut refused = stored(None);
    refused.outcome = ReplayOutcome::Refused("not-admissible".into());
    assert!(
        disclose_replay(
            &pep,
            &runtime,
            &customer,
            Action::OrderCreate,
            None,
            refused
        )
        .await
        .is_ok()
    );
    // Targeted: own action rechecked on current facts.
    let o1 = scoped::prefetch(&runtime, u(1)).await?;
    assert!(
        disclose_replay(
            &pep,
            &runtime,
            &customer,
            Action::OrderCancel,
            Some(&o1),
            stored(Some(1))
        )
        .await
        .is_ok()
    );
    let payer_reader = user(105, 30);
    assert_eq!(
        reason(
            disclose_replay(
                &pep,
                &runtime,
                &payer_reader,
                Action::OrderCancel,
                Some(&o1),
                stored(Some(1))
            )
            .await
        )
        .0,
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(
        reason(
            disclose_replay(
                &pep,
                &runtime,
                &customer,
                Action::OrderCancel,
                Some(&o1),
                stored(Some(2))
            )
            .await
        )
        .0,
        Reason::OrderNotFound
    );
    // A targeted replay without its prefetch is an integration failure, never a disclosure.
    assert!(matches!(
        disclose_replay(
            &pep,
            &runtime,
            &customer,
            Action::OrderCancel,
            None,
            stored(Some(1))
        )
        .await,
        Err(AuthzFailure::Integration)
    ));
    Ok(())
}
