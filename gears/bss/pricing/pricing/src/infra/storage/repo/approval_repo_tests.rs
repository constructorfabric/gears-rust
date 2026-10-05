#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::test_support::test_db;
use toolkit_db::secure::TxConfig;
use toolkit_db::{Db, DbError};

#[derive(Debug, thiserror::Error)]
enum TxErr {
    #[error(transparent)]
    Approval(#[from] ApprovalError),
    #[error(transparent)]
    Db(#[from] DbError),
}
fn db_error(e: &TxErr) -> Option<&sea_orm::DbErr> {
    match e {
        TxErr::Approval(e) => e.db_err(),
        TxErr::Db(DbError::Sea(e)) => Some(e),
        TxErr::Db(_) => None,
    }
}
async fn in_tx<T: Send + 'static>(
    db: &Db,
    mut f: impl for<'a> FnMut(
        &'a DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, ApprovalError>> + Send + 'a>,
    > + Send,
) -> Result<T, TxErr> {
    db.transaction_with_retry(TxConfig::serializable(), db_error, move |tx| {
        let future = f(tx);
        Box::pin(async move { future.await.map_err(TxErr::Approval) })
    })
    .await
}
fn fixture(tenant: Uuid) -> (Unit, Vec<ItemRef>) {
    let items = (0..2)
        .map(|n| ItemRef {
            item_type: "price".into(),
            item_id: Uuid::new_v4(),
            created_by: Uuid::new_v4(),
            before: None,
            after: serde_json::json!({"name":format!("Row {n}")}),
        })
        .collect::<Vec<_>>();
    let unit = Unit {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        kind: "prices".into(),
        ref_type: "price".into(),
        ref_id: items[0].item_id,
        state: UnitState::Pending,
        common_effective_date: Some(crate::test_support::at(9).date()),
        quorum_required: 1,
        generation: 1,
        submitted_by: Uuid::new_v4(),
        submitted_at: crate::test_support::at(9),
        // D-445: pricing's doors send no note, but the store keeps what the unit carries.
        submit_note: Some("why the prices move".into()),
        decided_at: None,
        decided_note: None,
        snapshot: serde_json::json!({"items":items}),
        snapshot_hash: bss_approval::hash::snapshot_hash(&items, None),
        version: 1,
    };
    (unit, items)
}
fn decision(id: Uuid) -> Decision {
    Decision {
        unit_id: id,
        actor: Uuid::new_v4(),
        generation: 1,
        verdict: Verdict::Approve,
        note: Some("reviewed".into()),
        at: crate::test_support::at(10),
        stale: false,
    }
}
#[tokio::test]
async fn policy_defaults_to_one_and_zero_is_an_explicit_override() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    assert_eq!(
        read_policy(&conn, &scope, tenant)
            .await
            .unwrap()
            .default_quorum,
        1
    );
    write_policy(&conn, &scope, tenant, "*", 0).await.unwrap();
    write_policy(&conn, &scope, tenant, "plan_revision", 2)
        .await
        .unwrap();
    let p = read_policy(&conn, &scope, tenant).await.unwrap();
    assert_eq!(p.default_quorum, 0);
    assert_eq!(p.quorum_for("plan_revision"), 2);
    write_policy(&conn, &scope, tenant, "plan_revision", 3)
        .await
        .unwrap();
    assert_eq!(
        read_policy(&conn, &scope, tenant)
            .await
            .unwrap()
            .quorum_for("plan_revision"),
        3
    );
}
#[tokio::test]
async fn store_round_trip_cas_duplicate_refresh_and_terminal_state() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let (unit, items) = fixture(tenant);
    let id = unit.id;
    in_tx(&db.db(), move |tx| {
        let scope = scope.clone();
        let unit = unit.clone();
        let items = items.clone();
        Box::pin(async move {
            let store = PricingApprovalStore {
                scope,
                tenant_id: tenant,
            };
            store.insert_unit(tx, &unit, &items).await?;
            assert_eq!(store.unit(tx, id).await?, Some(unit));
            assert_eq!(store.items(tx, id).await?.len(), 2);
            assert!(store.bump_version(tx, id, 1).await?);
            assert!(!store.bump_version(tx, id, 1).await?);
            let vote = decision(id);
            store.insert_decision(tx, &vote).await?;
            // The racing second vote is the engine's typed refusal (PS-31, PT-15).
            assert!(
                matches!(
                    store.insert_decision(tx, &vote).await,
                    Err(ApprovalError::DuplicateVote)
                ),
                "a second vote of one actor in one generation is DuplicateVote"
            );
            let refreshed = vec![ItemRef {
                after: serde_json::json!({"name":"updated"}),
                ..items[0].clone()
            }];
            store
                .refresh(
                    tx,
                    id,
                    &refreshed,
                    &serde_json::json!({"generation":2}),
                    "new hash",
                    2,
                )
                .await?;
            assert_eq!(store.items(tx, id).await?, refreshed);
            assert!(store.decisions(tx, id).await?[0].stale);
            let mut next = vote;
            next.generation = 2;
            store.insert_decision(tx, &next).await?;
            let votes = store.decisions(tx, id).await?;
            assert_eq!(votes.len(), 2);
            assert_eq!(votes.iter().filter(|d| d.stale).count(), 1);
            store
                .set_state(
                    tx,
                    id,
                    UnitState::Approved,
                    Some(crate::test_support::at(11)),
                    Some("approved"),
                )
                .await?;
            let got = store.unit(tx, id).await?.unwrap();
            assert_eq!(got.state, UnitState::Approved);
            assert_eq!(got.generation, 2);
            assert_eq!(got.version, 2);
            assert_eq!(got.snapshot_hash, "new hash");
            assert_eq!(got.decided_note.as_deref(), Some("approved"));
            assert_eq!(
                got.submit_note.as_deref(),
                Some("why the prices move"),
                "a refresh and a decision keep the submitter's note"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn a_decision_and_version_bump_roll_back_on_error() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let (unit, items) = fixture(tenant);
    let id = unit.id;
    let seed_scope = scope.clone();
    in_tx(&db.db(), move |tx| {
        let scope = seed_scope.clone();
        let unit = unit.clone();
        let items = items.clone();
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(tx, &unit, &items)
            .await
        })
    })
    .await
    .unwrap();
    let tx_scope = scope.clone();
    let result: Result<(), _> = in_tx(&db.db(), move |tx| {
        let scope = tx_scope.clone();
        Box::pin(async move {
            let store = PricingApprovalStore {
                scope,
                tenant_id: tenant,
            };
            store.bump_version(tx, id, 1).await?;
            store.insert_decision(tx, &decision(id)).await?;
            Err(ApprovalError::ApplyRefused {
                code: "APPLY_REFUSED",
                detail: "rollback probe".into(),
            })
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(TxErr::Approval(ApprovalError::ApplyRefused { .. }))
    ));
    assert!(
        decisions_of(&db.conn().unwrap(), &scope, tenant, id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        list_units(&db.conn().unwrap(), &scope, tenant, None, None, None)
            .await
            .unwrap()[0]
            .version,
        1
    );
}
#[tokio::test]
async fn foreign_unit_cannot_receive_decisions_or_refreshed_items() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let (unit, items) = fixture(tenant);
    let id = unit.id;
    in_tx(&db.db(), move |tx| {
        let scope = scope.clone();
        let unit = unit.clone();
        let items = items.clone();
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(tx, &unit, &items)
            .await
        })
    })
    .await
    .unwrap();
    let foreign = Uuid::new_v4();
    in_tx(&db.db(), move |tx| {
        Box::pin(async move {
            let store = PricingApprovalStore {
                scope: AccessScope::for_tenant(foreign),
                tenant_id: foreign,
            };
            assert!(store.unit(tx, id).await?.is_none());
            // Another tenant's unit is not found: not a vote on it, not a refresh (PT-15).
            assert!(matches!(
                store.insert_decision(tx, &decision(id)).await,
                Err(ApprovalError::UnitNotFound { unit_id }) if unit_id == id
            ));
            let (_, items) = fixture(foreign);
            assert!(matches!(
                store
                    .refresh(tx, id, &items, &serde_json::json!({}), "hash", 2)
                    .await,
                Err(ApprovalError::UnitNotFound { unit_id }) if unit_id == id
            ));
            Ok(())
        })
    })
    .await
    .unwrap();
}
