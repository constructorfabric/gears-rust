//! Commercial persistence uses the existing shared `PostgreSQL` harness.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod commercial_support;
mod pg_support;
use bss_pricing::infra::{
    commercial_terms::wire,
    storage::repo::{acceptance_repo, commercial_command_repo, hold_repo},
};
use commercial_support::{command, held, row};
use toolkit_db::{DBProvider, DbError, secure::AccessScope};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn postgres_commercial_keys_foreign_keys_and_restart_preserve_exact_receipts() {
    let pg = pg_support::Pg::applied().await;
    let db = DBProvider::<DbError>::new(pg.db().await);
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
    let mut receipt = wire::decode_acceptance(&a.receipt_json).unwrap();
    receipt.acceptance_id = Uuid::new_v4();
    assert!(
        acceptance_repo::insert(
            &db.conn().unwrap(),
            &scope,
            acceptance_repo::from_receipt(&receipt, a.created_by).unwrap()
        )
        .await
        .is_err()
    );
    let h = held(&a);
    hold_repo::insert(&db.conn().unwrap(), &scope, h.clone())
        .await
        .unwrap();
    assert!(
        hold_repo::insert(&db.conn().unwrap(), &scope, held(&a))
            .await
            .is_err()
    );
    let c = command(&a);
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, c.clone())
        .await
        .unwrap();
    let mut duplicate = c.clone();
    duplicate.id = Uuid::new_v4();
    assert!(
        commercial_command_repo::insert(&db.conn().unwrap(), &scope, duplicate.clone())
            .await
            .is_err()
    );
    duplicate.caller_tenant_id = Uuid::new_v4();
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, duplicate)
        .await
        .unwrap();
    let foreign = Uuid::new_v4();
    let denied = AccessScope::for_tenant(foreign);
    assert!(
        acceptance_repo::find(&db.conn().unwrap(), &denied, a.tenant_id, a.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        hold_repo::find_acceptance(&db.conn().unwrap(), &denied, a.tenant_id, a.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        commercial_command_repo::find_scope(
            &db.conn().unwrap(),
            &denied,
            &commercial_command_repo::CommandScope::from(&c)
        )
        .await
        .unwrap()
        .is_none()
    );
    let mut wrong = held(&a);
    wrong.tenant_id = foreign;
    assert!(
        hold_repo::insert(&db.conn().unwrap(), &denied, wrong)
            .await
            .is_err()
    );
    let mut wrong = c.clone();
    wrong.id = Uuid::new_v4();
    wrong.tenant_id = foreign;
    assert!(
        commercial_command_repo::insert(&db.conn().unwrap(), &denied, wrong)
            .await
            .is_err()
    );
    let mut hc = command(&a);
    hc.operation = "hold".into();
    hc.receipt_kind = "hold".into();
    hc.receipt_id = h.id;
    hc.acceptance_id = None;
    hc.hold_id = Some(h.id);
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, hc.clone())
        .await
        .unwrap();
    hc.tenant_id = foreign;
    hc.id = Uuid::new_v4();
    assert!(
        commercial_command_repo::insert(&db.conn().unwrap(), &denied, hc)
            .await
            .is_err()
    );
    drop(db);
    let db = DBProvider::<DbError>::new(pg.db().await);
    assert_eq!(
        acceptance_repo::find(&db.conn().unwrap(), &scope, a.tenant_id, a.id)
            .await
            .unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        hold_repo::find_acceptance(&db.conn().unwrap(), &scope, a.tenant_id, a.id)
            .await
            .unwrap(),
        Some(h)
    );
    assert_eq!(
        commercial_command_repo::find_scope(
            &db.conn().unwrap(),
            &scope,
            &commercial_command_repo::CommandScope::from(&c)
        )
        .await
        .unwrap(),
        Some(c)
    );
}
#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn postgres_competing_acceptances_reread_one_winner() {
    let pg = pg_support::Pg::applied().await;
    let db = DBProvider::<DbError>::new(pg.db().await);
    let other = DBProvider::<DbError>::new(pg.db().await);
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    let mut receipt = wire::decode_acceptance(&a.receipt_json).unwrap();
    receipt.acceptance_id = Uuid::new_v4();
    let b = acceptance_repo::from_receipt(&receipt, a.created_by).unwrap();
    let barrier = tokio::sync::Barrier::new(2);
    let left = async {
        barrier.wait().await;
        acceptance_repo::insert_or_get(&db.conn().unwrap(), &scope, a)
            .await
            .unwrap()
    };
    let right = async {
        barrier.wait().await;
        acceptance_repo::insert_or_get(&other.conn().unwrap(), &scope, b)
            .await
            .unwrap()
    };
    let (a, b) = tokio::join!(left, right);
    assert_eq!(a, b);
    let h = held(&a);
    let c = command(&a);
    assert_eq!(
        hold_repo::insert_or_get(&db.conn().unwrap(), &scope, h.clone())
            .await
            .unwrap(),
        h
    );
    assert_eq!(
        hold_repo::insert_or_get(&db.conn().unwrap(), &scope, held(&a))
            .await
            .unwrap(),
        h
    );
    assert_eq!(
        commercial_command_repo::insert_or_get(&db.conn().unwrap(), &scope, c.clone())
            .await
            .unwrap(),
        c
    );
    let mut another = c.clone();
    another.id = Uuid::new_v4();
    assert_eq!(
        commercial_command_repo::insert_or_get(&db.conn().unwrap(), &scope, another.clone())
            .await
            .unwrap(),
        c
    );
    another.request_digest = "ff".repeat(32);
    assert!(
        commercial_command_repo::insert_or_get(&db.conn().unwrap(), &scope, another)
            .await
            .is_err()
    );
}
