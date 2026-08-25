//! The driver-message classifier, without a database.
//!
//! [`duplicate_bundle_or_db`] answers a **specific, caller-fixable refusal** —
//! "that plan already carries a bundle, go and edit the one you have" — and it
//! decides it from text the driver prints. `storage::policy_guard_or_contention`'s
//! doc argues at length why message matching is the narrow case rather than a
//! precedent, and the argument rests on two things these cases pin: the typed
//! unique class is asked **first**, and the literals are DDL this chain owns.
//!
//! The conjunct is the half no database suite can testify about. A real collision
//! is both unique *and* named, so `tests/sqlite_bundle_repo.rs`'s
//! `a_second_bundle_on_one_plan_is_refused` stays green whether the conjunct is
//! there or not. What needs a case is the pair the database cannot easily be made
//! to produce: a failure that is **named but not unique**, and one that is
//! **unique but not named**.
//!
//! This is the third of three such sites; the two in `overlay_repo` are already
//! corrected, and `overlay_repo_tests.rs` is the file these cases follow.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DbErr;
use toolkit_db::secure::ScopeError;

use super::{duplicate_bundle_or_db, names_the_bundle_plan_slot};
use crate::domain::scope_key::PlanId;
use crate::infra::storage::RepoError;

/// A driver error carrying exactly this message and nothing the typed class can
/// be read from.
///
/// `DbErr::Custom` has no SQLSTATE, so `is_unique_violation` falls back to the
/// toolkit's own message test — the shared one, not a substring this crate
/// invented — which is precisely the seam these cases exercise.
fn driver_says(message: &str) -> ScopeError {
    ScopeError::Db(DbErr::Custom(message.to_owned()))
}

/// The plan the refusal has to name.
fn plan() -> PlanId {
    PlanId::new(uuid::Uuid::from_u128(0x_b0_11_d1_e0))
}

/// Both renderings of the real refusal, one per backend.
///
/// Captured rather than invented: the `SQLite` form is what the mirror prints for
/// `uq_pricing_bundle_plan` (which names the indexed column, not the index), the
/// Postgres form is that server's documented `unique_violation` message. A rename
/// of the index that forgot this function reddens here.
///
/// The refusal must also still name the **plan**, because that is the whole of
/// what the caller does next.
#[test]
fn a_real_second_bundle_on_one_plan_is_the_duplicate_refusal_on_both_backends() {
    for message in [
        "database error: Execution Error: error returned from database: duplicate key value \
         violates unique constraint \"uq_pricing_bundle_plan\"",
        "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
         constraint failed: pricing_bundle.plan_id",
    ] {
        let refused =
            duplicate_bundle_or_db(&driver_says(message), plan(), "insert pricing_bundle");
        assert!(
            matches!(
                refused,
                RepoError::DuplicateBundleOnPlan { ref plan_id } if *plan_id == plan().get().to_string()
            ),
            "the real refusal must survive the conjunct and keep naming the plan: {refused:?} \
             from {message}"
        );
    }
}

/// **Named but not unique**: the conjunct's whole purpose.
///
/// A deadlock report naming the index is a storage fault the caller can do nothing
/// about, and answering it with "this plan already carries a bundle" sends an
/// operator to look for a bundle that may not exist — the refusal's own remedy is
/// to go and edit the one they have. Without `is_unique_violation()` this was
/// classified by prose the driver is free to reword.
#[test]
fn a_failure_that_merely_names_the_bundle_plan_index_is_not_the_duplicate_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: deadlock detected; \
         process 8123 waits for ShareLock on index \"uq_pricing_bundle_plan\"",
    );
    assert!(
        !err.is_unique_violation(),
        "the premise of this case: this failure is not in the unique class"
    );
    let refused = duplicate_bundle_or_db(&err, plan(), "insert pricing_bundle");
    assert!(
        matches!(
            refused,
            RepoError::Db(ref detail) if detail.starts_with("insert pricing_bundle: ")
        ),
        "a non-unique failure is a storage failure, whatever index its text names: {refused:?}"
    );
}

/// Named but not unique, in the second rendering.
///
/// The `SQLite` arm matches a **column list**, which is the form a great many
/// messages quote — a planner or constraint report naming `pricing_bundle.plan_id`
/// is not a uniqueness refusal on it.
#[test]
fn a_column_level_fault_naming_plan_id_is_not_the_duplicate_refusal_either() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 1299) NOT NULL \
         constraint failed: pricing_bundle.plan_id",
    );
    assert!(!err.is_unique_violation(), "the premise of this case");
    assert!(
        matches!(
            duplicate_bundle_or_db(&err, plan(), "insert pricing_bundle"),
            RepoError::Db(_)
        ),
        "a NOT NULL violation quoting the same column is a storage failure"
    );
}

/// **Unique but not named**: this table's other unique constraint.
///
/// `pricing_bundle`'s primary key is on `bundle_id`, so "a unique index refused"
/// cannot on its own mean the plan slot — and its loser is a minted id colliding,
/// which is not something the caller fixes by finding their existing bundle.
#[test]
fn a_unique_violation_on_the_bundle_primary_key_is_not_the_duplicate_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 1555) UNIQUE \
         constraint failed: pricing_bundle.bundle_id",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        matches!(
            duplicate_bundle_or_db(&err, plan(), "insert pricing_bundle"),
            RepoError::Db(_)
        ),
        "the identity key is not the plan slot"
    );
}

/// Unique but not named: a neighbouring table whose column **ends** the same.
///
/// `pricing_bundle_component.component_plan_id` is the column a bare `plan_id`
/// substring would also match, which is why the predicate matches the **qualified**
/// `pricing_bundle.plan_id`. This comment claimed `pricing_bundle_component.plan_id`
/// "exists" until 2026-08-20, and it never did — on any of the three bundle child
/// tables; the message the case actually feeds the matcher names
/// `component_plan_id`, which is the real collision.
#[test]
fn a_unique_violation_on_a_neighbouring_tables_plan_id_is_not_this_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
         constraint failed: pricing_bundle_component.bundle_id, \
         pricing_bundle_component.component_plan_id",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        !names_the_bundle_plan_slot(&err.to_string()),
        "another table's key is a different constraint, and the qualified table name is what \
         tells them apart"
    );
}
