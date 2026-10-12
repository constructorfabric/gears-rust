#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::infra::storage::repo::{self, HeadWrite};
use crate::test_support::test_db;
use bss_approval::{ApprovalSubject, ApproveOutcome, Engine};
use bss_products_sdk::models::{Lifecycle, SkuContent, SkuType};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
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
    db.transaction_with_retry(TxConfig::default(), db_error, move |tx| {
        let future = f(tx);
        Box::pin(async move { future.await.map_err(TxErr::Approval) })
    })
    .await
}
fn fixture(tenant: Uuid) -> (Unit, Vec<ItemRef>) {
    let items = (0..2)
        .map(|n| ItemRef {
            item_type: "sku".into(),
            item_id: Uuid::new_v4(),
            created_by: Uuid::new_v4(),
            before: None,
            after: serde_json::json!({"name":format!("SKU {n}")}),
        })
        .collect::<Vec<_>>();
    let unit = Unit {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        kind: "sku_publish".into(),
        ref_type: "sku".into(),
        ref_id: items[0].item_id,
        state: UnitState::Pending,
        common_effective_date: Some(crate::test_support::at(9).date()),
        quorum_required: 1,
        generation: 1,
        submitted_by: Uuid::new_v4(),
        submitted_at: crate::test_support::at(9),
        // P-D-219: the store keeps the submitter's note as the unit carries it.
        submit_note: Some("first release".into()),
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
    write_policy(&conn, &scope, tenant, "sku_retire", 2)
        .await
        .unwrap();
    let p = read_policy(&conn, &scope, tenant).await.unwrap();
    assert_eq!(p.default_quorum, 0);
    assert_eq!(p.quorum_for("sku_retire"), 2);
    write_policy(&conn, &scope, tenant, "sku_retire", 3)
        .await
        .unwrap();
    assert_eq!(
        read_policy(&conn, &scope, tenant)
            .await
            .unwrap()
            .quorum_for("sku_retire"),
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
            let store = ProductsApprovalStore {
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
            // RS-32: the unique index's loser is the typed duplicate vote (409), not a store failure.
            assert!(matches!(
                store.insert_decision(tx, &vote).await,
                Err(ApprovalError::DuplicateVote)
            ));
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
                Some("first release"),
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
            ProductsApprovalStore {
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
            let store = ProductsApprovalStore {
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

// A minimal business subject uses real SKU writes. The store/engine race is the
// subject of this test; production publish validation belongs to Task 9.
#[derive(Clone)]
struct Subject {
    scope: AccessScope,
    tenant: Uuid,
    items: Vec<ItemRef>,
}
fn repo_error(e: crate::infra::storage::RepoError) -> ApprovalError {
    match e {
        crate::infra::storage::RepoError::Driver { source, .. } => ApprovalError::Db(source),
        e => ApprovalError::Store(e.to_string()),
    }
}
#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for Subject {
    fn kind(&self) -> &'static str {
        "sku_publish"
    }
    fn ref_type(&self) -> &'static str {
        "sku"
    }
    async fn collect(&self, _: &DbTx<'a>, _: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        Ok(self.items.clone())
    }
    async fn validate_submit(&self, _: &DbTx<'a>, _: &[ItemRef]) -> Result<(), ApprovalError> {
        Ok(())
    }
    async fn lock(&self, _: &DbTx<'a>, _: Uuid, _: &[ItemRef]) -> Result<(), ApprovalError> {
        Ok(())
    }
    fn snapshot(&self, items: &[ItemRef], _: Option<time::Date>) -> serde_json::Value {
        serde_json::json!(items)
    }
    async fn apply(&self, tx: &DbTx<'a>, _: &Unit, items: &[ItemRef]) -> Result<(), ApprovalError> {
        for i in items {
            let content: SkuContent = serde_json::from_value(i.after.clone()).unwrap();
            repo::write_sku_content(
                tx,
                &self.scope,
                self.tenant,
                i.item_id,
                &content,
                crate::test_support::at(12),
            )
            .await
            .map_err(repo_error)?;
            assert!(matches!(
                repo::set_lifecycle(
                    tx,
                    &self.scope,
                    self.tenant,
                    i.item_id,
                    &[Lifecycle::Draft],
                    Lifecycle::Published,
                    crate::test_support::at(12)
                )
                .await
                .map_err(repo_error)?,
                HeadWrite::Written(_)
            ));
        }
        Ok(())
    }
    async fn unlock(
        &self,
        tx: &DbTx<'a>,
        u: &Unit,
        items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        for i in items {
            repo::unlock_sku(
                tx,
                &self.scope,
                self.tenant,
                i.item_id,
                u.id,
                approved.then_some(u.id),
            )
            .await
            .map_err(repo_error)?;
        }
        Ok(())
    }
}
async fn writer(
    db: Db,
    scope: AccessScope,
    tenant: Uuid,
    id: Uuid,
    subject: Subject,
    barrier: Arc<tokio::sync::Barrier>,
    contentions: Arc<AtomicUsize>,
) -> Result<ApproveOutcome, TxErr> {
    let mut attempt = 0;
    db.transaction_with_retry(
        TxConfig::default(),
        move |e| {
            let db = db_error(e);
            if db.is_some_and(|e| {
                toolkit_db::contention::is_retryable_contention(sea_orm::DbBackend::Sqlite, e)
            }) {
                contentions.fetch_add(1, Ordering::SeqCst);
            }
            db
        },
        move |tx| {
            attempt += 1;
            let first = attempt == 1;
            let scope = scope.clone();
            let subject = subject.clone();
            let barrier = Arc::clone(&barrier);
            Box::pin(async move {
                let store = ProductsApprovalStore {
                    scope,
                    tenant_id: tenant,
                };
                if first {
                    assert_eq!(store.unit(tx, id).await?.unwrap().state, UnitState::Pending);
                    barrier.wait().await;
                }
                Ok(Engine::approve(
                    &store,
                    &subject,
                    tx,
                    id,
                    Uuid::new_v4(),
                    1,
                    None,
                    crate::test_support::at(12),
                )
                .await?)
            })
        },
    )
    .await
}
#[tokio::test]
async fn two_real_writers_apply_once_and_lock_errors_remain_typed_for_retry() {
    let (db, scope, tenant, dsn) = test_db().await;
    let other = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let conn = db.conn().unwrap();
    let cat = repo::insert_category(
        &conn,
        &scope,
        tenant,
        crate::domain::category::NewCategory {
            code: "c".into(),
            name: "c".into(),
            is_default: false,
            sort_order: 0,
        },
        crate::test_support::at(9),
    )
    .await
    .unwrap();
    let sku = repo::insert_sku(
        &conn,
        &scope,
        tenant,
        crate::domain::sku::NewSku {
            code: "a".into(),
            name: "a".into(),
            r#type: SkuType::OneTime,
            category_id: Some(cat.id),
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        Uuid::new_v4(),
        crate::test_support::at(9),
    )
    .await
    .unwrap();
    let items = vec![ItemRef {
        item_type: "sku".into(),
        item_id: sku.id,
        created_by: sku.created_by,
        before: None,
        after: serde_json::to_value(SkuContent::from(&sku)).unwrap(),
    }];
    let (mut unit, _) = fixture(tenant);
    unit.ref_id = sku.id;
    unit.common_effective_date = None;
    unit.snapshot_hash = bss_approval::hash::snapshot_hash(&items, None);
    let id = unit.id;
    let subject = Subject {
        scope: scope.clone(),
        tenant,
        items: items.clone(),
    };
    let seed_scope = scope.clone();
    in_tx(&db.db(), move |tx| {
        let scope = seed_scope.clone();
        let unit = unit.clone();
        let items = items.clone();
        Box::pin(async move {
            ProductsApprovalStore {
                scope: scope.clone(),
                tenant_id: tenant,
            }
            .insert_unit(tx, &unit, &items)
            .await?;
            assert!(
                repo::try_lock_sku(tx, &scope, tenant, unit.ref_id, id, 1)
                    .await
                    .map_err(repo_error)?
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let contentions = Arc::new(AtomicUsize::new(0));
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(
            writer(
                db.db(),
                scope.clone(),
                tenant,
                id,
                subject.clone(),
                Arc::clone(&barrier),
                Arc::clone(&contentions)
            ),
            writer(
                other,
                scope.clone(),
                tenant,
                id,
                subject,
                barrier,
                Arc::clone(&contentions)
            )
        )
    })
    .await
    .unwrap();
    let results = [a, b];
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Ok(ApproveOutcome::Applied)))
            .count(),
        1,
        "{results:?}"
    );
    // The loser is typed for retry: the unit already decided or contended, or a lock error the
    // toolkit's classifier calls contention, the one a door answers 409 `UNIT_CONTENDED` after its
    // retries (`tx_to_canonical_coded`). Which one depends on how long the winner holds its lock.
    assert_eq!(
        results
            .iter()
            .filter(|r| match r {
                Err(TxErr::Approval(ApprovalError::Contended | ApprovalError::AlreadyDecided)) =>
                    true,
                Err(e) => db_error(e).is_some_and(|e| {
                    toolkit_db::contention::is_retryable_contention(sea_orm::DbBackend::Sqlite, e)
                }),
                Ok(_) => false,
            })
            .count(),
        1,
        "{results:?}"
    );
    assert!(
        contentions.load(Ordering::SeqCst) > 0,
        "the lock-upgrade error reached the typed retry classifier"
    );
    assert_eq!(
        decisions_of(&conn, &scope, tenant, id).await.unwrap().len(),
        1
    );
    let sku = repo::find_sku(&conn, &scope, tenant, sku.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sku.published_version, 1);
    assert_eq!(sku.approved_by_unit_id, Some(id));
    assert_eq!(sku.pending_unit_id, None);
}
