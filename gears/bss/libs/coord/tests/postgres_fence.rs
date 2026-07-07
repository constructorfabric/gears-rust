//! Postgres-only: does [`LeaseGuard::with_ack_in_tx`]'s fence actually reject a
//! lease that a peer stole **after** the ack transaction took its snapshot?
//!
//! This is the one scenario SQLite cannot model. SQLite serializes write
//! transactions (a single writer), so a peer steal cannot interleave "inside"
//! an open ack transaction the way it can under Postgres MVCC. The concern is
//! specific to Postgres `SERIALIZABLE`:
//!
//! * The ack transaction's snapshot is taken at its **first statement**, and
//!   the fence predicate `locked_until > NOW()` reads `NOW()` as
//!   `transaction_timestamp()` — the transaction's *start* time.
//! * If the ack transaction reads the lease row (fixing its snapshot) while the
//!   lease is still live, then outlives the TTL, a peer can steal the lease and
//!   commit. Under the ack transaction's frozen snapshot the row still looks
//!   "mine" and still looks live, so the fence SELECT can return a row.
//! * The safety of the fence then rests entirely on `SERIALIZABLE` aborting the
//!   ack transaction at COMMIT (a `40001`, which the retry helper turns into a
//!   fresh attempt that observes the steal → `LeaseLost`). Whether Postgres SSI
//!   actually raises that abort for a single read-then-commit against a
//!   concurrently-updated row is exactly what this test settles empirically.
//!
//! The test asserts the **safety property**: after a peer steals the lease
//! mid-ack, the acknowledged commit must NOT succeed — it must surface
//! [`AckError::LeaseLost`]. An `Ok(_)` outcome means the fence accepted a stolen
//! lease (the reviewed concern is real). Ignored by default; run with
//! `cargo test -p coord --test postgres_fence -- --ignored`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown,
    clippy::let_underscore_must_use
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::{Notify, watch};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::AccessScope;

use coord::{AckError, LeaseManager};

use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const KEY: &str = "fence-job";

/// Test-local mirror of the crate-private `coord_leases` entity. The primitive
/// does not surface the row, and integration tests can only reach a `DbTx`
/// through `SecureEntityExt`, so the ack closure needs its own entity to issue
/// the snapshot-fixing read inside the ack transaction.
mod coord_leases {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "coord_leases")]
    #[secure(no_tenant, no_resource, no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub locked_by: Option<Uuid>,
        pub locked_until: DateTime<Utc>,
        pub attempts: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Fresh Postgres `Db` with the `coord_leases` table migrated in (unqualified —
/// resolved via the default `public` search_path).
async fn setup(url: &str) -> Db {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect postgres");
    run_migrations_for_testing(
        &db,
        vec![Box::new(coord::migration::Migration::unqualified())],
    )
    .await
    .expect("run coord_leases migration");
    db
}

/// Push the holder's `locked_until` into the past so the peer's `acquire` treats
/// the slot as expired and steals it — deterministic, unlike sleeping past a
/// second-precision TTL. Runs on its own connection (a committed UPDATE), so it
/// lands *after* the ack transaction's snapshot was fixed.
async fn force_expire(db: &Db) {
    let conn = db.conn().expect("conn");
    let past = Utc::now() - TimeDelta::seconds(30);
    coord_leases::Entity::update_many()
        .col_expr(coord_leases::Column::LockedUntil, Expr::value(past))
        .filter(coord_leases::Column::Key.eq(KEY))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("force-expire");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn fence_rejects_a_lease_stolen_after_the_ack_snapshot() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = setup(&url).await;

    let mgr = LeaseManager::new(db.clone());
    let guard_a = mgr
        .acquire(KEY, Duration::from_mins(5))
        .await
        .expect("worker A acquires the free slot");
    let holder_a = guard_a.locked_by();

    // Rendezvous: `snapshot_ready` fires once the ack tx has read the lease row
    // (fixing its SERIALIZABLE snapshot); `steal_done` gates the ack tx from
    // reaching its fence until the peer steal has committed.
    let snapshot_ready = Arc::new(Notify::new());
    let (steal_tx, steal_rx) = watch::channel(false);

    // Worker A's fenced ack, running concurrently. Its work closure fixes the
    // snapshot, signals the test, then blocks until the peer has stolen.
    let ack_task = {
        let snapshot_ready = snapshot_ready.clone();
        tokio::spawn(async move {
            guard_a
                .with_ack_in_tx::<_, (), sea_orm::DbErr, _>(
                    // A work-side read error is a plain DbErr; expose it to the
                    // retry classifier as-is.
                    |e: &sea_orm::DbErr| Some(e),
                    move |tx| {
                        let snapshot_ready = snapshot_ready.clone();
                        let mut steal_rx = steal_rx.clone();
                        Box::pin(async move {
                            // (1) First statement in the ack tx — fixes the
                            // SERIALIZABLE snapshot while the lease is still live
                            // and still ours, BEFORE the peer steal commits.
                            coord_leases::Entity::find()
                                .filter(coord_leases::Column::Key.eq(KEY))
                                .secure()
                                .scope_with(&AccessScope::allow_all())
                                .one(tx)
                                .await
                                .map_err(|e| sea_orm::DbErr::Custom(format!("ack read: {e:?}")))?;
                            // (2) Snapshot is fixed — let the test steal the lease.
                            snapshot_ready.notify_one();
                            // (3) Do not reach the fence until the steal committed.
                            //     Re-entrant across a 40001 retry: already-true
                            //     resolves immediately.
                            let _ = steal_rx.wait_for(|stolen| *stolen).await;
                            Ok(())
                        })
                    },
                )
                .await
        })
    };

    // Wait until A's ack tx has fixed its snapshot, then steal the lease from
    // under it: expire the row and let a peer LeaseManager acquire it. Both are
    // committed transactions that land after A's snapshot.
    snapshot_ready.notified().await;
    force_expire(&db).await;
    let peer_guard = LeaseManager::new(db.clone())
        .acquire(KEY, Duration::from_mins(5))
        .await
        .expect("peer steals the expired lease");
    assert_ne!(
        peer_guard.locked_by(),
        holder_a,
        "the peer must install a fresh holder"
    );

    // Release A's fence.
    steal_tx.send(true).expect("signal steal done");

    let ack = ack_task.await.expect("ack task join");

    assert!(
        matches!(ack, Err(AckError::LeaseLost)),
        "SAFETY: a lease stolen after the ack snapshot must fail the fenced \
         commit with LeaseLost; an Ok(_) means the fence accepted a stolen \
         lease (the fence `NOW()`/snapshot concern is real). Got: {ack:?}"
    );
}
