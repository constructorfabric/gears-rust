//! The driver-message classifier, without a database (**D-340**).
//!
//! [`phase_id_in_use_or_db`] answers a **specific, caller-fixable refusal** —
//! "this revision already holds that phase id, name another" — and it decides it
//! from text the driver prints. `storage::policy_guard_or_contention`'s doc argues
//! at length why message matching is the narrow case rather than a precedent, and
//! the argument rests on two things these cases pin: the typed unique class is
//! asked **first**, and the literals are DDL this chain owns
//! (`pricing_plan_phase`).
//!
//! The conjunct is the half no database suite can testify about. A real collision
//! is both unique *and* named, so `tests/rest_plans.rs`'s
//! `a_phases_payload_naming_one_id_twice_answers_the_conflict_and_names_it` stays
//! green whether the conjunct is there or not. What needs a case is the pair the
//! database cannot easily be made to produce: a failure that is **named but not
//! unique**, and one that is **unique but not named**.
//!
//! The second of those is not hypothetical here, which is what makes the name
//! match load-bearing rather than defensive: this table carries a second unique
//! constraint, `uq_pricing_plan_phase_terminal`, and a `phases` payload naming two
//! terminal phases trips it through the very same statement. Its remedy is to give
//! one of them a `convertsToPhaseId`, not to rename it, so answering it with
//! `PHASE_ID_IN_USE` would send an author to change the one thing that is not
//! wrong.
//!
//! `bundle_repo_tests.rs` is the file these cases follow, and the shape is
//! deliberately identical: three classifiers in this crate now read a driver
//! message, and a fourth spelling of the discipline would be a fourth thing to
//! keep true.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DbErr;
use toolkit_db::secure::ScopeError;
use uuid::Uuid;

use super::{names_the_phase_key, phase_id_in_use_or_db};
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

/// The phase the refusal has to name.
fn phase() -> Uuid {
    Uuid::from_u128(0x_fa_5e_1d)
}

/// Both renderings of the real refusal, one per backend.
///
/// Captured rather than invented: the `SQLite` form is what the engine prints for
/// a composite `PRIMARY KEY` (which names the indexed columns, since the key is
/// declared inline and has no name of its own), and the Postgres form is that
/// server's documented `unique_violation` message quoting the constraint
/// `pricing_plan_phase` re-adds by name. A rename that forgot this function reddens
/// here.
///
/// The refusal must also still name the **phase id**, because that is the whole of
/// what the caller does next — and the fact the `500` it replaced withheld.
#[test]
fn a_real_repeated_phase_id_is_the_conflict_on_both_backends() {
    for message in [
        "database error: Execution Error: error returned from database: duplicate key value \
         violates unique constraint \"pricing_plan_phase_pkey\"",
        "database error: Execution Error: error returned from database: (code: 1555) UNIQUE \
         constraint failed: pricing_plan_phase.tenant_id, pricing_plan_phase.plan_id, \
         pricing_plan_phase.plan_revision, pricing_plan_phase.phase_id",
    ] {
        let refused = phase_id_in_use_or_db(
            &driver_says(message),
            Some(phase()),
            "insert pricing_plan_phase",
        );
        assert!(
            matches!(
                refused,
                RepoError::PhaseIdInUse(ref id) if *id == phase().to_string()
            ),
            "the real refusal must survive the conjunct and keep naming the phase: {refused:?} \
             from {message}"
        );
    }
}

/// A refusal that cannot name the id is not this refusal.
///
/// `phase_id_of` is total where `ActiveValue::unwrap` panics, and this is the arm
/// that makes the totality mean something: a model carrying no id is unreachable
/// through either producer in the module, and if it ever were reachable the caller
/// would get the storage failure it got before D-340 rather than a conflict naming
/// nothing.
#[test]
fn a_model_with_no_id_takes_the_storage_failure_arm() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: duplicate key value \
         violates unique constraint \"pricing_plan_phase_pkey\"",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        matches!(
            phase_id_in_use_or_db(&err, None, "insert pricing_plan_phase"),
            RepoError::Db(_)
        ),
        "the conflict's entire value is the id it names"
    );
}

/// **Unique but not named**: this table's *other* unique constraint, and the one
/// the same statement can trip on the same request.
///
/// `uq_pricing_plan_phase_terminal` is partial on `(plan_id, plan_revision) WHERE
/// converts_to_phase_id IS NULL`, so a payload carrying two terminal phases loses
/// here. Its column list cannot contain `pricing_plan_phase.phase_id`, which is
/// exactly why the qualified column is what the predicate matches.
#[test]
fn a_unique_violation_on_the_terminal_index_is_not_this_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
         constraint failed: pricing_plan_phase.plan_id, pricing_plan_phase.plan_revision",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        !names_the_phase_key(&err.to_string()),
        "two terminal phases is a different constraint with a different remedy"
    );
    assert!(
        matches!(
            phase_id_in_use_or_db(&err, Some(phase()), "insert pricing_plan_phase"),
            RepoError::Db(_)
        ),
        "and it keeps the answer it had"
    );
}

/// Unique but not named, in the Postgres rendering of the same index.
#[test]
fn the_terminal_index_named_as_a_constraint_is_not_this_refusal_either() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: duplicate key value \
         violates unique constraint \"uq_pricing_plan_phase_terminal\"",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(!names_the_phase_key(&err.to_string()));
}

/// **Named but not unique**: the conjunct's whole purpose.
///
/// A deadlock report naming the key is a storage fault the caller can do nothing
/// about, and answering it with "that phase id is taken" sends an author to rename
/// a phase that is not in anybody's way. Without `is_unique_violation()` this was
/// classified by prose the driver is free to reword.
#[test]
fn a_failure_that_merely_names_the_phase_key_is_not_the_conflict() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: deadlock detected; \
         process 8123 waits for ShareLock on index \"pricing_plan_phase_pkey\"",
    );
    assert!(
        !err.is_unique_violation(),
        "the premise of this case: this failure is not in the unique class"
    );
    let refused = phase_id_in_use_or_db(&err, Some(phase()), "insert pricing_plan_phase");
    assert!(
        matches!(
            refused,
            RepoError::Db(ref detail) if detail.starts_with("insert pricing_plan_phase: ")
        ),
        "a non-unique failure is a storage failure, whatever key its text names: {refused:?}"
    );
}

/// Named but not unique, in the second rendering.
///
/// The `SQLite` arm matches a **column list**, which is the form a great many
/// messages quote — and the append-only triggers this table carries raise a bare
/// `RAISE(ABORT, …)` that a substring test must not mistake for a key.
#[test]
fn a_column_level_fault_naming_phase_id_is_not_the_conflict_either() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 1299) NOT NULL \
         constraint failed: pricing_plan_phase.phase_id",
    );
    assert!(!err.is_unique_violation(), "the premise of this case");
    assert!(
        matches!(
            phase_id_in_use_or_db(&err, Some(phase()), "insert pricing_plan_phase"),
            RepoError::Db(_)
        ),
        "a NOT NULL violation quoting the same column is a storage failure"
    );
}

/// Unique but not named: the successor column, whose name **contains** the one
/// matched.
///
/// `converts_to_phase_id` is why the predicate matches `pricing_plan_phase.phase_id`
/// with its dot rather than the bare `phase_id`: the qualified form cannot be a
/// substring of `pricing_plan_phase.converts_to_phase_id`, and the bare form is a
/// substring of both.
#[test]
fn a_unique_violation_naming_the_successor_column_is_not_this_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
         constraint failed: pricing_plan_phase.converts_to_phase_id",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        !names_the_phase_key(&err.to_string()),
        "the successor edge is not the key, and the qualified column is what tells them apart"
    );
}
