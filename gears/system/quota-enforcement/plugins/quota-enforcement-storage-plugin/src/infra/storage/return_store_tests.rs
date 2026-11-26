#![allow(clippy::expect_used)]

use quota_enforcement_sdk::{
    AttributionDigest, ChargeReturn, IdempotencyScope, MetricId, NotificationEventKind,
    OperationType, QuotaId, QuotaType, RollbackTarget, StorageError, TransitionOutcome,
};
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use time::Duration;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;

use super::super::consumption_store::tests::{DAY_ONE, Harness, TTL, authorized, partial, write};
use crate::domain::ports::{ConsumptionStore, LeaseStore, QuotaStore};
use crate::infra::storage::entity::{
    charge, charge_entry, charge_return, idempotency_retention_config, quota,
    quota_allocation_counter,
};
use crate::infra::storage::repo::charge_repo;
use crate::test_support::{METRIC_REQUESTS, METRIC_TOKENS, actor, scope_for, tenant};

fn tokens() -> MetricId {
    MetricId::parse(METRIC_TOKENS).expect("metric")
}

fn debit_scope(key: &str) -> IdempotencyScope {
    write(OperationType::Debit, key, 0).scope
}

fn ret(charge: &str, amount: u64, key: &str) -> ChargeReturn {
    ChargeReturn {
        charge: debit_scope(charge),
        metric: tokens(),
        amount,
        key: key.to_owned(),
    }
}

fn rollback(scope: IdempotencyScope) -> RollbackTarget {
    RollbackTarget {
        original: scope,
        authorized: authorized(),
    }
}

impl Harness {
    async fn give(
        &self,
        ret: &ChargeReturn,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::AppliedMutation>, StorageError> {
        self.store
            .apply_return(&actor(), &scope_for(tenant()), ret, &[])
            .await
    }

    async fn roll_back(
        &self,
        target: &RollbackTarget,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::AppliedMutation>, StorageError> {
        self.store
            .apply_rollback(&actor(), &scope_for(tenant()), target, &[])
            .await
    }

    /// The allocation counter, read directly: a snapshot skips a deactivated
    /// Quota.
    async fn in_flight(&self, id: QuotaId) -> i64 {
        quota_allocation_counter::Entity::find()
            .filter(quota_allocation_counter::Column::QuotaId.eq(id.as_uuid()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&self.db.conn().expect("conn"))
            .await
            .expect("read the allocation row")
            .expect("the allocation row exists")
            .in_flight
    }

    async fn deactivate(&self, id: QuotaId) {
        self.quotas
            .deactivate_quota(&actor(), &scope_for(tenant()), id, &[])
            .await
            .expect("deactivate");
    }

    /// Turn a Quota into a rate Quota, which no API can create yet.
    async fn make_rate(&self, id: QuotaId) {
        quota::Entity::update_many()
            .col_expr(
                quota::Column::QuotaType,
                Expr::value(QuotaType::Rate.as_gts_id()),
            )
            .filter(quota::Column::Id.eq(id.as_uuid()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&self.db.conn().expect("conn"))
            .await
            .expect("make the quota a rate quota");
    }

    async fn configure_retention(&self, seconds: i64) {
        toolkit_db::secure::secure_insert::<idempotency_retention_config::Entity>(
            idempotency_retention_config::ActiveModel {
                tenant_key: Set(tenant().as_uuid().to_string()),
                metric_key: Set(METRIC_TOKENS.to_owned()),
                retention_seconds: Set(seconds),
                updated_at: Set(DAY_ONE),
            },
            &AccessScope::allow_all(),
            &self.db.conn().expect("conn"),
        )
        .await
        .expect("configure retention");
    }

    /// Every charge row, live or deleted.
    async fn charge_rows(&self) -> Vec<charge::Model> {
        charge::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&self.db.conn().expect("conn"))
            .await
            .expect("read the charges")
    }

    async fn return_rows(&self) -> usize {
        charge_return::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&self.db.conn().expect("conn"))
            .await
            .expect("read the return records")
            .len()
    }

    /// Check every charge against the deletion rule computed from its rows
    /// here, and the stored columns against the invariant of `charge_repo`.
    async fn check_charges(&self, now: time::OffsetDateTime) {
        let conn = self.db.conn().expect("conn");
        let all = AccessScope::allow_all();
        let quotas = quota::Entity::find()
            .secure()
            .scope_with(&all)
            .all(&conn)
            .await
            .expect("read the quotas");
        let entries = charge_entry::Entity::find()
            .secure()
            .scope_with(&all)
            .all(&conn)
            .await
            .expect("read the entries");
        let quota_of = |entry: &charge_entry::Model| {
            quotas
                .iter()
                .find(|row| row.id == entry.quota_id)
                .expect("the entry's quota")
        };
        let allocation = QuotaType::Allocation.as_gts_id();
        for entry in entries.iter().filter(|entry| entry.holding) {
            assert_eq!(quota_of(entry).quota_type, allocation);
            assert!(entry.returned < entry.charged);
        }
        for row in self.charge_rows().await {
            let own: Vec<_> = entries.iter().filter(|e| e.charge_id == row.id).collect();
            let holds = own.iter().any(|entry| {
                let quota = quota_of(entry);
                quota.quota_type == allocation
                    && quota.status == "active"
                    && entry.returned < entry.charged
            });
            let live = row.return_window_end > now
                || row.kept_until.is_some_and(|kept| kept > now)
                || holds;
            let found = charge_repo::find_live_by_id(&conn, &all, row.id, now)
                .await
                .expect("read the charge");
            assert_eq!(found.is_some(), live, "charge {}", row.idem_key);
            let holding = own.iter().any(|entry| entry.holding);
            let later = row.kept_until.map_or(row.return_window_end, |kept| {
                kept.max(row.return_window_end)
            });
            assert_eq!(
                row.expires_at,
                (!holding).then_some(later),
                "charge {}",
                row.idem_key
            );
        }
    }

    async fn holds_pending(&self, id: QuotaId) -> bool {
        quota::Entity::find()
            .filter(quota::Column::Id.eq(id.as_uuid()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&self.db.conn().expect("conn"))
            .await
            .expect("read the quota")
            .expect("the quota exists")
            .holds_pending
    }

    async fn returned(&self, key: &str) -> Vec<i64> {
        self.charge(&debit_scope(key))
            .await
            .expect("the charge lives")
            .1
            .into_iter()
            .map(|entry| entry.returned)
            .collect()
    }
}

// --- return -----------------------------------------------------------------

#[tokio::test]
async fn partial_and_repeated_returns_give_back_once_per_key() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    let first = h.give(&ret("k1", 10, "r1")).await.expect("partial return");
    assert!(matches!(first, TransitionOutcome::Applied(_)));
    assert_eq!(h.consumed("u1", id).await, 20);

    let repeat = h.give(&ret("k1", 10, "r1")).await.expect("repeat");
    assert!(matches!(repeat, TransitionOutcome::NoOp(_)));
    assert_eq!(h.consumed("u1", id).await, 20, "a repeat changes nothing");

    h.give(&ret("k1", 5, "r2")).await.expect("another return");
    assert_eq!(h.consumed("u1", id).await, 15);
    assert_eq!(h.returned("k1").await, vec![15]);

    assert_eq!(
        h.give(&ret("k1", 11, "r1"))
            .await
            .expect_err("another amount"),
        StorageError::IdempotencyPayloadMismatch
    );
    assert_eq!(h.return_rows().await, 2);
    assert_eq!(
        h.events()
            .await
            .iter()
            .filter(|kind| *kind == NotificationEventKind::QuotaRollbackApplied.as_str())
            .count(),
        2,
        "one event per applied return"
    );
    h.down().await;
}

#[tokio::test]
async fn a_return_above_what_remains_fails_and_changes_nothing() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.give(&ret("k1", 20, "r1")).await.expect("return");

    assert_eq!(
        h.give(&ret("k1", 11, "r2"))
            .await
            .expect_err("above the rest"),
        StorageError::ReturnExceedsCharge {
            requested: 11,
            remaining: 10
        }
    );
    assert_eq!(h.consumed("u1", id).await, 10);
    assert_eq!(h.returned("k1").await, vec![20]);
    assert_eq!(h.return_rows().await, 1);
    h.down().await;
}

#[tokio::test]
async fn a_return_of_an_unknown_charge_or_another_metric_type_fails() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    assert!(matches!(
        h.give(&ret("nope", 1, "r1")).await,
        Err(StorageError::OperationNotFound { .. })
    ));
    let other_metric = ChargeReturn {
        metric: MetricId::parse(METRIC_REQUESTS).expect("metric"),
        ..ret("k1", 1, "r1")
    };
    assert!(matches!(
        h.give(&other_metric).await,
        Err(StorageError::MetricTypeMismatch { .. })
    ));
    assert_eq!(h.return_rows().await, 0);
    h.down().await;
}

#[tokio::test]
async fn a_repeated_return_naming_another_metric_type_fails_as_a_first_one_does() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.give(&ret("k1", 10, "r1")).await.expect("return");

    let other_metric = ChargeReturn {
        metric: MetricId::parse(METRIC_REQUESTS).expect("metric"),
        ..ret("k1", 10, "r1")
    };
    assert!(matches!(
        h.give(&other_metric).await,
        Err(StorageError::MetricTypeMismatch { .. })
    ));
    assert_eq!(h.consumed("u1", id).await, 20);
    h.down().await;
}

#[tokio::test]
async fn one_return_key_names_one_charge() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.debit("u1", 30, &write(OperationType::Debit, "k2", 2))
        .await
        .expect("debit");
    h.give(&ret("k1", 5, "r1")).await.expect("return");

    assert_eq!(
        h.give(&ret("k2", 5, "r1"))
            .await
            .expect_err("another charge"),
        StorageError::IdempotencyPayloadMismatch
    );
    assert_eq!(h.returned("k2").await, vec![0]);
    h.down().await;
}

#[tokio::test]
async fn a_repeated_return_after_the_replay_window_gets_the_first_result() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.give(&ret("k1", 10, "r1")).await.expect("return");

    h.set_now(DAY_ONE + Duration::days(3));
    let repeat = h.give(&ret("k1", 10, "r1")).await.expect("repeat");
    assert!(matches!(repeat, TransitionOutcome::NoOp(_)));
    assert_eq!(h.consumed("u1", id).await, 20);
    h.down().await;
}

#[tokio::test]
async fn a_repeat_of_the_return_that_took_the_last_amount_lives_one_replay_window() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    h.set_now(DAY_ONE + Duration::days(3));
    h.give(&ret("k1", 30, "r1")).await.expect("the last amount");
    assert_eq!(h.consumed("u1", id).await, 0);

    h.set_now(DAY_ONE + Duration::days(3) + Duration::hours(23));
    let repeat = h.give(&ret("k1", 30, "r1")).await.expect("repeat");
    assert!(matches!(repeat, TransitionOutcome::NoOp(_)));

    h.set_now(DAY_ONE + Duration::days(4) + Duration::hours(1));
    assert!(matches!(
        h.give(&ret("k1", 30, "r1")).await,
        Err(StorageError::OperationNotFound { .. })
    ));
    h.down().await;
}

#[tokio::test]
async fn a_consumption_charge_takes_a_return_only_inside_its_return_window() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    // Past the period boundary, inside the window: the charge's own period.
    h.set_now(DAY_ONE + Duration::hours(23));
    h.give(&ret("k1", 10, "r1"))
        .await
        .expect("inside the window");
    assert_eq!(h.period_consumed(id).await, 20);

    // Kept by that return, but past its return window.
    h.set_now(DAY_ONE + Duration::hours(25));
    assert_eq!(
        h.give(&ret("k1", 5, "r2"))
            .await
            .expect_err("past the window"),
        StorageError::ReturnNotApplicable
    );
    assert_eq!(h.period_consumed(id).await, 20);
    assert_eq!(h.returned("k1").await, vec![10]);
    h.down().await;
}

#[tokio::test]
async fn a_settled_period_takes_no_return() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    // A debit in the next period settles the charge's period.
    h.set_now(DAY_ONE + Duration::hours(15));
    h.debit("u1", 1, &write(OperationType::Debit, "k2", 2))
        .await
        .expect("debit in the next period");

    assert_eq!(
        h.give(&ret("k1", 5, "r1")).await.expect_err("settled"),
        StorageError::ReturnNotApplicable
    );
    let _ = id;
    h.down().await;
}

#[tokio::test]
async fn deactivated_quotas_take_a_return_as_active_ones_do() {
    let h = Harness::up().await;
    let allocation = h.allocation_quota("u1", Some(100)).await;
    let consumption = h.consumption_quota("u2", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "a", 1))
        .await
        .expect("debit");
    h.debit("u2", 30, &write(OperationType::Debit, "c", 2))
        .await
        .expect("debit");
    h.deactivate(allocation).await;
    h.deactivate(consumption).await;

    h.give(&ret("a", 10, "r1")).await.expect("allocation");
    h.give(&ret("c", 10, "r2")).await.expect("consumption");
    assert_eq!(h.in_flight(allocation).await, 20);
    assert_eq!(h.period_consumed(consumption).await, 20);
    h.down().await;
}

#[tokio::test]
async fn a_return_after_a_credit_to_zero_takes_the_counter_to_zero_not_below() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.store
        .apply_credit(
            &actor(),
            &scope_for(tenant()),
            id,
            30,
            &partial("c1", 2),
            &[],
        )
        .await
        .expect("credit to zero");

    h.give(&ret("k1", 10, "r1"))
        .await
        .expect("an allocation entry always takes a return");
    assert_eq!(h.consumed("u1", id).await, 0);
    assert_eq!(h.returned("k1").await, vec![10]);
    h.down().await;
}

#[tokio::test]
async fn a_return_after_a_credit_for_the_same_charge_floors_at_zero() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.store
        .apply_credit(
            &actor(),
            &scope_for(tenant()),
            id,
            20,
            &partial("c1", 2),
            &[],
        )
        .await
        .expect("a credit for the same work");

    h.give(&ret("k1", 30, "r1")).await.expect("return");
    assert_eq!(h.consumed("u1", id).await, 0, "the counter floors at zero");
    assert_eq!(h.returned("k1").await, vec![30], "the charge is spent");
    h.down().await;
}

#[tokio::test]
async fn the_return_window_keeps_the_length_configured_when_the_charge_was_written() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.configure_retention(3600).await;

    h.set_now(DAY_ONE + Duration::hours(2));
    h.give(&ret("k1", 10, "r1"))
        .await
        .expect("the 24 h window of the charge still runs");
    assert_eq!(h.period_consumed(id).await, 20);
    let kept = h.charge_rows().await[0].kept_until;
    assert_eq!(
        kept,
        Some(DAY_ONE + Duration::hours(3)),
        "the return keeps it for the window configured now"
    );
    h.down().await;
}

#[tokio::test]
async fn a_rate_entry_takes_no_return() {
    let h = Harness::under("cel").await;
    let rate = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "alone", 1))
        .await
        .expect("debit");
    h.make_rate(rate).await;
    assert_eq!(
        h.give(&ret("alone", 10, "r1"))
            .await
            .expect_err("rate only"),
        StorageError::ReturnNotApplicable
    );

    let allocation = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 5, &write(OperationType::Debit, "both", 2))
        .await
        .expect("debit");
    h.give(&ret("both", 5, "r2"))
        .await
        .expect("the allocation takes it");
    assert_eq!(h.in_flight(allocation).await, 0);
    assert_eq!(
        h.in_flight(rate).await,
        35,
        "the rate counter does not change"
    );
    assert_eq!(h.returned("both").await.iter().sum::<i64>(), 10);
    h.down().await;
}

#[tokio::test]
async fn a_return_unwinds_a_cascade_from_its_last_entry_and_every_chain_of_an_and_charge() {
    let h = Harness::under("cel").await;
    let bounded = h.allocation_quota("u1", Some(10)).await;
    let unbounded = h.allocation_quota("u1", None).await;
    h.evaluator
        .cascade
        .store(true, std::sync::atomic::Ordering::Relaxed);
    h.debit("u1", 25, &write(OperationType::Debit, "cascade", 1))
        .await
        .expect("debit");
    h.give(&ret("cascade", 20, "r1")).await.expect("return");
    assert_eq!(h.in_flight(unbounded).await, 0, "the last entry first");
    assert_eq!(h.in_flight(bounded).await, 5);

    h.evaluator
        .cascade
        .store(false, std::sync::atomic::Ordering::Relaxed);
    h.debit("u1", 4, &write(OperationType::Debit, "and", 2))
        .await
        .expect("debit");
    h.give(&ret("and", 3, "r2")).await.expect("return");
    assert_eq!(h.in_flight(bounded).await, 6);
    assert_eq!(h.in_flight(unbounded).await, 1);
    h.down().await;
}

// --- rollback ---------------------------------------------------------------

#[tokio::test]
async fn a_rollback_after_a_partial_return_gives_back_the_rest_once() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.give(&ret("k1", 10, "r1")).await.expect("return");

    let first = h
        .roll_back(&rollback(debit_scope("k1")))
        .await
        .expect("rollback");
    assert!(matches!(first, TransitionOutcome::Applied(_)));
    assert_eq!(h.consumed("u1", id).await, 0);

    let second = h
        .roll_back(&rollback(debit_scope("k1")))
        .await
        .expect("again");
    assert!(matches!(second, TransitionOutcome::NoOp(_)));
    assert_eq!(
        h.give(&ret("k1", 1, "r2"))
            .await
            .expect_err("a late return"),
        StorageError::ReturnExceedsCharge {
            requested: 1,
            remaining: 0
        }
    );
    h.down().await;
}

#[tokio::test]
async fn a_rollback_of_a_lease_commit_charge_gives_back_the_commit() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    let acquired = h
        .acquire("u1", 40, TTL, &write(OperationType::Reserve, "l1", 1))
        .await
        .expect("acquire");
    let token = acquired.get().token.expect("held");
    h.store
        .commit_lease(
            &actor(),
            &scope_for(tenant()),
            token,
            Some(25),
            None,
            &partial("c1", 2),
            &[],
        )
        .await
        .expect("commit");
    assert_eq!(h.consumed("u1", id).await, 25);

    h.roll_back(&rollback(IdempotencyScope {
        operation_type: OperationType::Commit,
        ..debit_scope("c1")
    }))
    .await
    .expect("rollback the commit");
    assert_eq!(h.consumed("u1", id).await, 0);
    h.down().await;
}

#[tokio::test]
async fn a_rollback_with_another_attribution_finds_nothing() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let foreign = RollbackTarget {
        original: debit_scope("k1"),
        authorized: AttributionDigest::from_bytes([9; 32]),
    };
    assert!(matches!(
        h.roll_back(&foreign).await,
        Err(StorageError::OperationNotFound { .. })
    ));
    assert_eq!(h.consumed("u1", id).await, 30);
    h.down().await;
}

#[tokio::test]
async fn a_rollback_with_another_attribution_discovers_no_charge_and_no_quota() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let discover = |authorized| super::GiveBack::Rollback {
        charge: debit_scope("k1"),
        authorized,
    };

    let own = h
        .store
        .discover(&scope_for(tenant()), &discover(authorized()))
        .await
        .expect("read");
    let foreign = h
        .store
        .discover(
            &scope_for(tenant()),
            &discover(AttributionDigest::from_bytes([9; 32])),
        )
        .await
        .expect("read");

    assert!(own.charge_id.is_some() && !own.quotas.is_empty());
    assert!(foreign.charge_id.is_none());
    assert!(foreign.metric.is_none());
    assert!(foreign.quotas.is_empty());
    h.down().await;
}

// --- retention --------------------------------------------------------------

#[tokio::test]
async fn a_charge_that_replaced_the_discovered_one_under_the_key_is_unknown() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let named = h.charge_rows().await.remove(0);
    let current = super::Discovered {
        charge_id: Some(named.id),
        ..super::Discovered::default()
    };
    let replaced = super::Discovered {
        charge_id: Some(uuid::Uuid::now_v7()),
        ..super::Discovered::default()
    };
    let what = super::GiveBack::Return(ret("k1", 1, "r1"));

    assert!(super::verified(&what, Some(named.clone()), &current).is_ok());
    assert!(matches!(
        super::verified(&what, Some(named), &replaced),
        Err(StorageError::OperationNotFound { .. })
    ));
    h.down().await;
}

#[tokio::test]
async fn a_debit_under_the_key_of_a_deleted_charge_is_a_new_operation() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let first = h.charge_rows().await[0].id;

    h.set_now(DAY_ONE + Duration::hours(25));
    assert!(matches!(
        h.give(&ret("k1", 1, "r1")).await,
        Err(StorageError::OperationNotFound { .. })
    ));
    let again = h
        .debit("u1", 7, &write(OperationType::Debit, "k1", 2))
        .await
        .expect("a new operation, not a mismatch");
    assert!(matches!(again, TransitionOutcome::Applied(_)));
    assert_eq!(h.consumed("u1", id).await, 7);
    let rows = h.charge_rows().await;
    assert_eq!(rows.len(), 1, "the new charge replaced the deleted one");
    assert_ne!(rows[0].id, first);
    h.check_charges(DAY_ONE + Duration::hours(25)).await;
    h.down().await;
}

#[tokio::test]
async fn the_sweeper_removes_deleted_charges_with_their_return_records() {
    let h = Harness::up().await;
    h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.allocation_quota("u2", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "used", 1))
        .await
        .expect("debit");
    h.debit("u2", 30, &write(OperationType::Debit, "held", 2))
        .await
        .expect("debit");
    h.set_now(DAY_ONE + Duration::hours(20));
    h.give(&ret("used", 5, "r1")).await.expect("return");

    h.set_now(DAY_ONE + Duration::hours(25));
    assert_eq!(
        h.store
            .reclaim_expired_charges(10, DAY_ONE + Duration::hours(25))
            .await
            .expect("sweep"),
        0,
        "the return keeps its charge one replay window"
    );
    let later = DAY_ONE + Duration::hours(50);
    h.set_now(later);
    assert_eq!(
        h.store
            .reclaim_expired_charges(10, later)
            .await
            .expect("sweep"),
        1
    );
    let rows = h.charge_rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].idem_key, "held", "a held allocation charge stays");
    assert_eq!(h.return_rows().await, 0, "the return record went with it");
    h.check_charges(later).await;
    h.down().await;
}

#[tokio::test]
async fn the_sweeper_never_removes_a_charge_that_replaced_a_deleted_one() {
    let h = Harness::up().await;
    h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let later = DAY_ONE + Duration::hours(25);
    h.set_now(later);
    let selected = charge_repo::select_expired(
        &h.db.conn().expect("conn"),
        &AccessScope::allow_all(),
        10,
        later,
    )
    .await
    .expect("select");
    assert_eq!(selected.len(), 1);

    h.debit("u1", 5, &write(OperationType::Debit, "k1", 2))
        .await
        .expect("the replacing debit");
    let removed = charge_repo::delete_if_deleted(
        &h.db.conn().expect("conn"),
        &AccessScope::allow_all(),
        &[selected[0].0],
        later,
    )
    .await
    .expect("delete");
    assert_eq!(removed, 0);
    assert_eq!(
        h.store
            .reclaim_expired_charges(10, later)
            .await
            .expect("sweep"),
        0
    );
    assert!(h.charge(&debit_scope("k1")).await.is_some());
    h.check_charges(later).await;
    h.down().await;
}

#[tokio::test]
async fn a_charge_whose_allocation_quotas_are_all_deactivated_is_deleted_after_the_window() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.deactivate(id).await;

    h.set_now(DAY_ONE + Duration::hours(23));
    h.give(&ret("k1", 10, "r1"))
        .await
        .expect("before the window ends");
    assert_eq!(h.in_flight(id).await, 20);

    h.set_now(DAY_ONE + Duration::hours(48));
    assert!(matches!(
        h.give(&ret("k1", 10, "r2")).await,
        Err(StorageError::OperationNotFound { .. })
    ));
    h.check_charges(DAY_ONE + Duration::hours(48)).await;
    h.down().await;
}

#[tokio::test]
async fn an_active_allocation_entry_keeps_a_charge_with_a_deactivated_one() {
    let h = Harness::under("cel").await;
    let deactivated = h.allocation_quota("u1", Some(100)).await;
    let active = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.deactivate(deactivated).await;

    h.set_now(DAY_ONE + Duration::days(10));
    h.give(&ret("k1", 10, "r1")).await.expect("still live");
    assert_eq!(h.in_flight(active).await, 20);
    assert_eq!(h.in_flight(deactivated).await, 20);
    h.check_charges(DAY_ONE + Duration::days(10)).await;
    h.down().await;
}

#[tokio::test]
async fn held_allocation_charges_past_their_window_are_counted_per_metric() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    h.consumption_quota("u2", Some(100), Vec::new()).await;
    h.debit("u1", 30, &write(OperationType::Debit, "old", 1))
        .await
        .expect("debit");
    h.debit("u2", 30, &write(OperationType::Debit, "used", 2))
        .await
        .expect("debit");
    h.set_now(DAY_ONE + Duration::hours(12));
    h.debit("u1", 1, &write(OperationType::Debit, "young", 3))
        .await
        .expect("debit");
    h.set_now(DAY_ONE + Duration::hours(30));
    h.debit("u1", 1, &write(OperationType::Debit, "spent", 4))
        .await
        .expect("debit");
    h.give(&ret("spent", 1, "r1")).await.expect("return");

    let stats = h
        .store
        .read_held_allocation_charge_stats(DAY_ONE + Duration::hours(30))
        .await
        .expect("stats");
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].metric, tokens());
    assert_eq!(stats[0].count, 1, "only the old one is past its window");
    assert_eq!(stats[0].oldest_created_at, DAY_ONE);
    h.check_charges(DAY_ONE + Duration::hours(30)).await;
    h.down().await;
}

#[tokio::test]
async fn a_hold_on_a_deactivated_quota_is_counted_until_the_sweeper_releases_it() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.deactivate(id).await;
    let later = DAY_ONE + Duration::hours(30);
    h.set_now(later);
    let held = |h: &Harness| {
        let store = h.store.clone();
        async move {
            let stats = store.read_held_allocation_charge_stats(later).await;
            stats
                .expect("stats")
                .iter()
                .map(|row| row.count)
                .sum::<u64>()
        }
    };
    assert_eq!(held(&h).await, 1, "not released yet");
    h.store
        .reclaim_expired_charges(10, later)
        .await
        .expect("sweep");
    assert_eq!(held(&h).await, 0, "released, and past its window deleted");
    h.down().await;
}

// --- stored lifetime ----------------------------------------------------------

#[tokio::test]
async fn a_held_allocation_charge_has_no_expiry_until_it_is_fully_returned() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let (held, entries) = h.charge(&debit_scope("k1")).await.expect("the charge");
    assert_eq!(held.expires_at, None);
    assert!(entries.iter().all(|entry| entry.holding));

    h.set_now(DAY_ONE + Duration::hours(2));
    h.give(&ret("k1", 30, "r1")).await.expect("return");
    let (returned, entries) = h.charge(&debit_scope("k1")).await.expect("the charge");
    assert!(entries.iter().all(|entry| !entry.holding));
    let kept = returned.kept_until.expect("kept by the return");
    assert_eq!(
        returned.expires_at,
        Some(kept.max(returned.return_window_end))
    );
    h.check_charges(DAY_ONE + Duration::hours(2)).await;
    h.check_charges(DAY_ONE + Duration::days(3)).await;
    h.down().await;
}

#[tokio::test]
async fn a_partly_returned_charge_stays_held_until_its_last_holding_entry_is_returned() {
    let h = Harness::under("cel").await;
    h.allocation_quota("u1", Some(10)).await;
    h.allocation_quota("u1", None).await;
    h.evaluator
        .cascade
        .store(true, std::sync::atomic::Ordering::Relaxed);
    h.debit("u1", 25, &write(OperationType::Debit, "cascade", 1))
        .await
        .expect("debit");

    h.give(&ret("cascade", 15, "r1")).await.expect("return");
    let (charge, entries) = h.charge(&debit_scope("cascade")).await.expect("the charge");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.holding)
            .collect::<Vec<_>>(),
        vec![true, false],
        "the last entry is returned in full"
    );
    assert_eq!(charge.expires_at, None);
    h.check_charges(DAY_ONE + Duration::days(10)).await;

    h.give(&ret("cascade", 10, "r2")).await.expect("return");
    let (charge, _) = h.charge(&debit_scope("cascade")).await.expect("the charge");
    assert!(charge.expires_at.is_some());
    h.check_charges(DAY_ONE).await;
    h.check_charges(DAY_ONE + Duration::days(10)).await;
    h.down().await;
}

#[tokio::test]
async fn the_sweeper_releases_the_holds_of_a_deactivated_quota_and_removes_its_charges() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.deactivate(id).await;
    assert!(h.holds_pending(id).await);

    let later = DAY_ONE + Duration::hours(48);
    h.set_now(later);
    assert!(
        h.charge(&debit_scope("k1")).await.is_none(),
        "deleted before any sweep"
    );
    h.check_charges(later).await;

    assert_eq!(
        h.store
            .reclaim_expired_charges(10, later)
            .await
            .expect("sweep"),
        1
    );
    assert!(!h.holds_pending(id).await);
    assert!(h.charge_rows().await.is_empty());
    h.down().await;
}

#[tokio::test]
async fn a_released_entry_on_a_deactivated_quota_leaves_a_charge_held_by_an_active_one() {
    let h = Harness::under("cel").await;
    let deactivated = h.allocation_quota("u1", Some(100)).await;
    h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.deactivate(deactivated).await;

    let later = DAY_ONE + Duration::days(10);
    h.set_now(later);
    h.check_charges(later).await;
    assert_eq!(
        h.store
            .reclaim_expired_charges(10, later)
            .await
            .expect("sweep"),
        0
    );
    assert!(!h.holds_pending(deactivated).await);
    let (charge, entries) = h.charge(&debit_scope("k1")).await.expect("still live");
    assert_eq!(charge.expires_at, None);
    assert_eq!(entries.iter().filter(|entry| entry.holding).count(), 1);
    h.check_charges(later).await;
    h.down().await;
}

#[tokio::test]
async fn the_sweeper_reaches_an_expired_charge_behind_many_older_held_ones() {
    let h = Harness::up().await;
    h.allocation_quota("u1", None).await;
    h.consumption_quota("u2", None, Vec::new()).await;
    for n in 0..20 {
        h.debit(
            "u1",
            1,
            &write(OperationType::Debit, &format!("held-{n}"), 1),
        )
        .await
        .expect("debit");
    }
    h.set_now(DAY_ONE + Duration::hours(1));
    h.debit("u2", 1, &write(OperationType::Debit, "used", 2))
        .await
        .expect("debit");

    let later = DAY_ONE + Duration::days(3);
    h.set_now(later);
    let selected = charge_repo::select_expired(
        &h.db.conn().expect("conn"),
        &AccessScope::allow_all(),
        1,
        later,
    )
    .await
    .expect("select");
    let used = h
        .charge_rows()
        .await
        .into_iter()
        .find(|row| row.idem_key == "used")
        .expect("the consumption charge");
    assert_eq!(
        selected.iter().map(|row| row.0).collect::<Vec<_>>(),
        vec![used.id]
    );
    assert_eq!(
        h.store
            .reclaim_expired_charges(1, later)
            .await
            .expect("sweep"),
        1
    );
    assert_eq!(h.charge_rows().await.len(), 20);
    h.check_charges(later).await;
    h.down().await;
}
