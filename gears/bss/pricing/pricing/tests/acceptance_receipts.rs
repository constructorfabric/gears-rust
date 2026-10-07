//! Durable commercial receipts and the authorized acceptance transaction.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_pricing::infra::{
    commercial_terms::wire,
    storage::repo::{acceptance_repo, commercial_command_repo, hold_repo},
};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{ConnectOpts, DBProvider, DbError, secure::AccessScope};
use uuid::Uuid;

async fn pool(dsn: &str) -> DBProvider<DbError> {
    DBProvider::new(
        toolkit_db::connect_db(
            dsn,
            ConnectOpts {
                max_conns: Some(2),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    )
}
async fn database() -> (DBProvider<DbError>, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("receipts.db").display()
    );
    let db = toolkit_db::connect_db(&dsn, ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    (DBProvider::new(db), dir, dsn)
}
mod commercial_support;
use commercial_support::{command, held, row};
#[test]
fn golden_v1_decodes_and_reencodes_byte_identically_without_rehashing() {
    let raw = include_str!("commercial_receipts/acceptance-v1.json");
    let receipt = wire::decode_acceptance(raw).unwrap();
    assert_eq!(wire::encode_acceptance(&receipt).unwrap(), raw);
    assert_eq!(receipt.query.order_version, u64::MAX);
    assert_eq!(receipt.accepted_at.nanosecond(), 123_456_789);
    let mut stored = receipt;
    stored.request_digest = [0xab; 32];
    stored.terms_digest = [0xcd; 32];
    stored.query.billing_terms.digest = [0xef; 32];
    assert_eq!(
        wire::decode_acceptance(&wire::encode_acceptance(&stored).unwrap()).unwrap(),
        stored,
        "decoding preserves issued digests rather than reinterpreting them"
    );
    assert!(
        wire::decode_acceptance(&raw.replacen(
            "\"schema_version\":\"1\"",
            "\"schema_version\":\"2\"",
            1
        ))
        .is_err()
    );
}
#[tokio::test]
async fn unique_business_identity_rejects_a_second_insert() {
    let (db, _dir, _) = database().await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
    let mut duplicate = a.clone();
    duplicate.id = Uuid::new_v4();
    let mut receipt = wire::decode_acceptance(&duplicate.receipt_json).unwrap();
    receipt.acceptance_id = duplicate.id;
    duplicate.receipt_json = wire::encode_acceptance(&receipt).unwrap();
    assert!(
        acceptance_repo::insert(&db.conn().unwrap(), &scope, duplicate)
            .await
            .is_err()
    );
    assert_eq!(
        acceptance_repo::find_business(
            &db.conn().unwrap(),
            &scope,
            a.tenant_id,
            a.order_id,
            &a.order_version,
            a.line_id
        )
        .await
        .unwrap(),
        Some(a)
    );
}
#[tokio::test]
async fn command_scope_is_unique_and_another_caller_has_an_independent_key() {
    let (db, _dir, _) = database().await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
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
    duplicate.caller_id = Uuid::new_v4();
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, duplicate.clone())
        .await
        .unwrap();
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
    assert_eq!(
        commercial_command_repo::find_scope(
            &db.conn().unwrap(),
            &scope,
            &commercial_command_repo::CommandScope::from(&duplicate)
        )
        .await
        .unwrap(),
        Some(duplicate)
    );
}
#[tokio::test]
async fn every_lookup_and_insert_enforces_tenant_scope() {
    let (db, _dir, _) = database().await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
    let h = held(&a);
    hold_repo::insert(&db.conn().unwrap(), &scope, h.clone())
        .await
        .unwrap();
    let c = command(&a);
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, c.clone())
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
        acceptance_repo::find(&db.conn().unwrap(), &scope, foreign, a.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        acceptance_repo::find_business(
            &db.conn().unwrap(),
            &denied,
            a.tenant_id,
            a.order_id,
            &a.order_version,
            a.line_id
        )
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
    assert!(
        acceptance_repo::insert(&db.conn().unwrap(), &denied, a)
            .await
            .is_err()
    );
    assert!(
        hold_repo::insert(&db.conn().unwrap(), &denied, h)
            .await
            .is_err()
    );
    assert!(
        commercial_command_repo::insert(&db.conn().unwrap(), &denied, c)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn racing_business_inserts_reread_one_committed_winner() {
    let (db, _dir, dsn) = database().await;
    let other = pool(&dsn).await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    let mut receipt = wire::decode_acceptance(&a.receipt_json).unwrap();
    receipt.acceptance_id = Uuid::new_v4();
    let b = acceptance_repo::from_receipt(&receipt, a.created_by).unwrap();
    let barrier = tokio::sync::Barrier::new(2);
    let first = async {
        barrier.wait().await;
        acceptance_repo::insert_or_get(&db.conn().unwrap(), &scope, a.clone())
            .await
            .unwrap()
    };
    let second = async {
        barrier.wait().await;
        acceptance_repo::insert_or_get(&other.conn().unwrap(), &scope, b)
            .await
            .unwrap()
    };
    let (left, right) = tokio::join!(first, second);
    assert_eq!(left, right);
    assert_eq!(
        acceptance_repo::find_business(
            &db.conn().unwrap(),
            &scope,
            a.tenant_id,
            a.order_id,
            &a.order_version,
            a.line_id
        )
        .await
        .unwrap(),
        Some(left)
    );
    let mut changed = receipt;
    changed.request_digest = [0xff; 32];
    assert!(
        acceptance_repo::insert_or_get(
            &db.conn().unwrap(),
            &scope,
            acceptance_repo::from_receipt(&changed, a.created_by).unwrap()
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn fresh_pool_reads_committed_acceptance_hold_and_command_byte_identically() {
    let (db, _dir, dsn) = database().await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
    let h = held(&a);
    hold_repo::insert(&db.conn().unwrap(), &scope, h.clone())
        .await
        .unwrap();
    let c = command(&a);
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, c.clone())
        .await
        .unwrap();
    drop(db);
    let reopened = pool(&dsn).await;
    assert_eq!(
        acceptance_repo::find(&reopened.conn().unwrap(), &scope, a.tenant_id, a.id)
            .await
            .unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        hold_repo::find_acceptance(&reopened.conn().unwrap(), &scope, a.tenant_id, a.id)
            .await
            .unwrap(),
        Some(h.clone())
    );
    assert_eq!(
        wire::encode_hold(&wire::decode_hold(&h.receipt_json).unwrap()).unwrap(),
        h.receipt_json
    );
    assert_eq!(
        commercial_command_repo::find_scope(
            &reopened.conn().unwrap(),
            &scope,
            &commercial_command_repo::CommandScope::from(&c)
        )
        .await
        .unwrap(),
        Some(c)
    );
}
#[tokio::test]
async fn holds_are_unique_and_receipt_foreign_keys_are_tenant_qualified() {
    let (db, _dir, _) = database().await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
    let h = held(&a);
    hold_repo::insert(&db.conn().unwrap(), &scope, h.clone())
        .await
        .unwrap();
    assert!(
        hold_repo::insert(&db.conn().unwrap(), &scope, held(&a))
            .await
            .is_err()
    );
    let foreign = Uuid::new_v4();
    let foreign_scope = AccessScope::for_tenant(foreign);
    let mut wrong = held(&a);
    wrong.tenant_id = foreign;
    assert!(
        hold_repo::insert(&db.conn().unwrap(), &foreign_scope, wrong)
            .await
            .is_err()
    );
    let mut c = command(&a);
    c.tenant_id = foreign;
    assert!(
        commercial_command_repo::insert(&db.conn().unwrap(), &foreign_scope, c)
            .await
            .is_err()
    );
    let mut c = command(&a);
    c.operation = "hold".into();
    c.receipt_kind = "hold".into();
    c.receipt_id = h.id;
    c.acceptance_id = None;
    c.hold_id = Some(h.id);
    commercial_command_repo::insert(&db.conn().unwrap(), &scope, c.clone())
        .await
        .unwrap();
    c.id = Uuid::new_v4();
    c.tenant_id = foreign;
    assert!(
        commercial_command_repo::insert(&db.conn().unwrap(), &foreign_scope, c)
            .await
            .is_err()
    );
}

#[test]
fn versioned_decode_refuses_lossy_or_ambiguous_wire_values() {
    let raw = include_str!("commercial_receipts/acceptance-v1.json");
    for bad in [
        raw.replacen(
            "\"schema_version\":\"1\",",
            "\"schema_version\":\"1\",\"schema_version\":\"1\",",
            1,
        ),
        raw.replace(
            "\"billing_terms\":{\"schema_version\":\"1\"",
            "\"billing_terms\":{\"schema_version\":\"2\"",
        ),
        raw.replace("\"quantity\":\"1\"", "\"quantity\":1.0"),
        raw.replace(
            "\"quantity\":\"1\"",
            "\"quantity\":\"1.00000000000000000000000000001\"",
        ),
        raw.replace("\"quantity\":\"1\"", "\"quantity\":\"1e0\""),
        raw.replace(
            "\"order_version\":\"18446744073709551615\"",
            "\"order_version\":\"18446744073709551616\"",
        ),
        raw.replace(".123456789Z", ".123456789+00:00"),
        raw.replace("\"dimension_key\":\"region\",", ""),
        raw.replace(
            "\"quantity\":\"1\"",
            "\"quantity\":\"1\",\"quantity\":\"2\"",
        ),
        raw.replace(
            "\"quantity\":\"1\"",
            "\"quantity\":\"1\",\"future_addition\":true",
        ),
    ] {
        assert!(wire::decode_acceptance(&bad).is_err(), "{bad}");
    }
    let good = wire::decode_acceptance(raw).unwrap();
    let again = wire::decode_acceptance(&wire::encode_acceptance(&good).unwrap()).unwrap();
    assert_eq!(again, good);
}

proptest::proptest! {
    #[test]
    fn decoders_do_not_panic(raw in "\\PC{0,80}") {
        match bss_pricing::infra::commercial_terms::wire::decode_acceptance(&raw) {
            Ok(_) | Err(_) => {}
        }
        match bss_pricing::infra::commercial_terms::wire::decode_hold(&raw) {
            Ok(_) | Err(_) => {}
        }
    }
}
#[tokio::test]
async fn hold_and_command_rereads_preserve_winners_and_refuse_changed_intent() {
    let (db, _dir, _) = database().await;
    let a = row();
    let scope = AccessScope::for_tenant(a.tenant_id);
    acceptance_repo::insert(&db.conn().unwrap(), &scope, a.clone())
        .await
        .unwrap();
    let h = held(&a);
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
    let mut receipt = wire::decode_hold(&h.receipt_json).unwrap();
    receipt.hold_id = Uuid::new_v4();
    receipt.activation_at += time::Duration::minutes(1);
    let changed = hold_repo::from_receipt(
        a.tenant_id,
        &receipt,
        a.created_by,
        wire::decode_acceptance(&a.receipt_json)
            .unwrap()
            .accepted_at,
    )
    .unwrap();
    assert!(
        hold_repo::insert_or_get(&db.conn().unwrap(), &scope, changed)
            .await
            .is_err()
    );
    let c = command(&a);
    assert_eq!(
        commercial_command_repo::insert_or_get(&db.conn().unwrap(), &scope, c.clone())
            .await
            .unwrap(),
        c
    );
    let mut retry = c.clone();
    retry.id = Uuid::new_v4();
    assert_eq!(
        commercial_command_repo::insert_or_get(&db.conn().unwrap(), &scope, retry.clone())
            .await
            .unwrap(),
        c
    );
    retry.request_digest = "ff".repeat(32);
    assert!(
        commercial_command_repo::insert_or_get(&db.conn().unwrap(), &scope, retry)
            .await
            .is_err()
    );
}

#[path = "acceptance_boundary/mod.rs"]
mod boundary;

mod acceptance_support;
mod plan_support;
mod seam_support;
use acceptance_support::AcceptanceFixture;
use bss_pricing_sdk::acceptance::SellabilityV1;

#[tokio::test]
async fn acceptance_retry_does_not_refresh_the_deadline() {
    let f = AcceptanceFixture::new().await;
    let first = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    f.clock.advance(time::Duration::hours(25));
    let replay = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    assert_eq!(replay.acceptance_id, first.acceptance_id);
    assert_eq!(replay.hold_until, first.hold_until);
    let mut changed = f.query.clone();
    changed.quantity = "2".parse().unwrap();
    let changed = f
        .sellability
        .check(&f.ctx, changed, f.meta.clone())
        .await
        .unwrap_err();
    assert_eq!(
        bss_pricing::infra::commercial_terms::errors::commercial_reason(&changed).as_deref(),
        Some("IdempotencyConflict"),
        "{changed:?}"
    );
}
mod acceptance_transaction;

/// A restart test drops this runtime to cancel all old outbox work before reopening storage.
fn restart_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
