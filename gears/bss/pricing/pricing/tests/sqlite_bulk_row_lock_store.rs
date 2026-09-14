//! What the **schema** refuses of a bulk row lock — Slice 12's side table
//! (`design/12-operator-efficiency.md` §6, `inst-bk-lock`, `inst-bs-commit`,
//! `inst-bs-approval`, D-37), driven as raw SQL because what is under test is
//! the store's own guarantee and not any repository's use of it.
//!
//! Two of this table's three rules are about *when* a lock may exist, and the
//! third is the absence of a rule: `DELETE` is unguarded. §6 orders the operator
//! abort as one act — `committing → completed_with_conflicts` with the lock
//! cleared — so a guard admitting `DELETE` only under a `committing` run would
//! refuse the clear inside the transaction §6 specifies; and a crash between the
//! terminal transition and the release would strand the locks permanently, the
//! run being terminal. That is the freeze D-37's release path exists to prevent.
//!
//! A fourth rule this table must **not** have is pinned here too: a lock is not
//! restricted to a repricing run.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const RUN: &str = "11111111-1111-1111-1111-111111111111";
const TENANT: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "33333333-3333-3333-3333-333333333333";
const PRICE: &str = "44444444-4444-4444-4444-444444444444";
const OTHER_RUN: &str = "55555555-5555-5555-5555-555555555555";
const OTHER_TENANT: &str = "66666666-6666-6666-6666-666666666666";
const PLAN: &str = "77777777-7777-7777-7777-777777777777";
const PHASE: &str = "88888888-8888-8888-8888-888888888888";

async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .expect_err(&format!("this statement must be rejected: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

fn seed_run(op: &str, tenant: &str) -> String {
    seed_run_of_kind(op, tenant, "repricing")
}

fn seed_run_of_kind(op: &str, tenant: &str, kind: &str) -> String {
    format!(
        "INSERT INTO pricing_bulk_operation \
         (operation_id, tenant_id, kind, state, client_key, report, submitted_by, submitted_at) \
         VALUES ('{op}', '{tenant}', '{kind}', 'validating', 'ck-{op}', '{{}}', '{ACTOR}', \
         '2026-08-08T00:00:00Z')"
    )
}

/// Move a run, stamping the end instant exactly on the states that end it.
///
/// **The terminal set is `chk_pricing_bulk_operation_completed_at`'s**, and
/// `rejected` joined it with D-267. A helper that had not been widened with the
/// migration sends `completed_at = NULL` on that move, the `CHECK` refuses it,
/// and the case fails while *arranging* its fixture -- so a run this suite meant
/// to find holding a lock in `rejected` never reaches `rejected` at all, and the
/// rule under test becomes one the fixture made unreachable rather than one the
/// store enforces.
fn advance(op: &str, state: &str) -> String {
    let completed = if matches!(
        state,
        "validation_failed" | "completed" | "completed_with_conflicts" | "rejected"
    ) {
        "'2026-08-08T02:00:00Z'"
    } else {
        "NULL"
    };
    format!(
        "UPDATE pricing_bulk_operation SET state = '{state}', completed_at = {completed} \
         WHERE operation_id = '{op}'"
    )
}

fn take_lock(op: &str, tenant: &str, price: &str) -> String {
    format!(
        "INSERT INTO pricing_bulk_row_lock \
         (tenant_id, price_id, bulk_operation_id, locked_at) \
         VALUES ('{tenant}', '{price}', '{op}', '2026-08-08T01:00:00Z')"
    )
}

/// The held row, as a real `pricing_price` row — the lock keys it.
fn seed_price(tenant: &str) -> String {
    format!(
        "INSERT INTO pricing_price ( \
             price_id, tenant_id, plan_id, currency, region, phase, \
             charge_kind, amount_minor, model_kind, lifecycle_state, \
             created_by, created_at_utc) \
         VALUES ('{PRICE}', '{tenant}', '{PLAN}', 'EUR', 'eu', '{PHASE}', \
             'recurring', 1000, 'flat', 'published', '{ACTOR}', \
             '2026-08-08T00:00:00Z')"
    )
}

/// A run that has reached `committing`, which is the only state a lock may be
/// taken under, and the row it is about to hold.
async fn committing_run(conn: &DatabaseConnection) {
    must_succeed(conn, &seed_price(TENANT)).await;
    must_succeed(conn, &seed_run(RUN, TENANT)).await;
    must_succeed(conn, &advance(RUN, "committing")).await;
}

/// A lock is taken while the run commits and released when it is done — the
/// whitelist half, and the shape every other case here departs from.
#[tokio::test]
async fn a_committing_run_takes_and_releases_a_lock() {
    let conn = migrated_db().await;
    committing_run(&conn).await;
    must_succeed(&conn, &take_lock(RUN, TENANT, PRICE)).await;
    assert_eq!(
        scalar(
            &conn,
            &format!(
                "SELECT bulk_operation_id AS v FROM pricing_bulk_row_lock \
                 WHERE tenant_id = '{TENANT}' AND price_id = '{PRICE}'"
            )
        )
        .await,
        RUN,
        "the lock must name the operation holding it -- that is what the \
         concurrent-edit conflict reports"
    );
    must_succeed(
        &conn,
        &format!(
            "DELETE FROM pricing_bulk_row_lock \
             WHERE tenant_id = '{TENANT}' AND bulk_operation_id = '{RUN}'"
        ),
    )
    .await;
    assert_eq!(
        locks_held(&conn).await,
        "0",
        "and the release must have taken effect, not merely not errored -- this \
         is the sweep's own query shape and the only place either suite runs it"
    );
}

/// How many locks the tenant holds on the fixture's row.
async fn locks_held(conn: &DatabaseConnection) -> String {
    scalar(
        conn,
        &format!(
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_bulk_row_lock \
             WHERE tenant_id = '{TENANT}' AND price_id = '{PRICE}'"
        ),
    )
    .await
}

/// **The bulk lock takes effect only on entry to `committing`.** §4 is explicit
/// in both directions, and `awaiting_approval` is the state that matters:
/// `inst-bs-approval` says rows are *not* locked while a run awaits its approval,
/// so a lock taken there would bounce interactive supersessions on keys the
/// design leaves free.
#[tokio::test]
async fn a_lock_may_only_be_taken_while_its_run_commits() {
    for state in [
        "validating",
        "awaiting_approval",
        "validation_failed",
        "completed",
        "completed_with_conflicts",
        // D-267's state. The custody arm keys on `run_state <> 'committing'`, so
        // this disjunct is already covered by the rule as written -- but a rule
        // and the set of states it is *asked* about are two different things, and
        // an arm narrowed to an explicit whitelist would leave `rejected` the one
        // state a lock could be taken in with nothing to say so.
        "rejected",
    ] {
        let conn = migrated_db().await;
        must_succeed(&conn, &seed_price(TENANT)).await;
        must_succeed(&conn, &seed_run(RUN, TENANT)).await;
        // The two terminal states are only reachable through `committing`, and
        // the run must not keep its lock across that edge either.
        if matches!(state, "completed" | "completed_with_conflicts") {
            must_succeed(&conn, &advance(RUN, "committing")).await;
        }
        // D-267's state is reachable only from a pending approval, and walking to
        // it by its own real path is the point: a run parked in a state it could
        // not have reached is not a fixture this rule is ever asked about.
        if state == "rejected" {
            must_succeed(&conn, &advance(RUN, "awaiting_approval")).await;
        }
        if state != "validating" {
            must_succeed(&conn, &advance(RUN, state)).await;
        }
        must_be_rejected(
            &conn,
            &take_lock(RUN, TENANT, PRICE),
            "only on entry to committing",
        )
        .await;
    }
}

/// The lock's tenant is its run's tenant. The foreign key covers the operation
/// alone, so without this arm one tenant's run could mark another tenant's row —
/// and the concurrent-edit check, scoped to that second tenant, would refuse an
/// edit naming an operation the tenant cannot see.
#[tokio::test]
async fn a_run_may_not_lock_another_tenants_row() {
    let conn = migrated_db().await;
    committing_run(&conn).await;
    must_be_rejected(
        &conn,
        &take_lock(RUN, OTHER_TENANT, PRICE),
        "belongs to another tenant",
    )
    .await;
}

/// One lock per row: the key **is** the mutual exclusion, so a second run cannot
/// take a row the first is holding.
#[tokio::test]
async fn two_runs_cannot_hold_one_row() {
    let conn = migrated_db().await;
    committing_run(&conn).await;
    must_succeed(&conn, &take_lock(RUN, TENANT, PRICE)).await;

    must_succeed(&conn, &seed_run(OTHER_RUN, TENANT)).await;
    must_succeed(&conn, &advance(OTHER_RUN, "committing")).await;
    must_be_rejected(&conn, &take_lock(OTHER_RUN, TENANT, PRICE), "UNIQUE").await;
}

/// A lock is taken or released, never edited: an in-place hand-off would tell the
/// interactive editor that an operation which never took the row holds it, and
/// would leave the release path unable to find what it owns.
#[tokio::test]
async fn a_lock_is_never_edited() {
    let conn = migrated_db().await;
    committing_run(&conn).await;
    must_succeed(&conn, &take_lock(RUN, TENANT, PRICE)).await;
    must_succeed(&conn, &seed_run(OTHER_RUN, TENANT)).await;
    must_succeed(&conn, &advance(OTHER_RUN, "committing")).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_bulk_row_lock SET bulk_operation_id = '{OTHER_RUN}' \
             WHERE tenant_id = '{TENANT}' AND price_id = '{PRICE}'"
        ),
        "never edited",
    )
    .await;
}

/// **A lock is releasable whatever its run's state, and that is the rule rather
/// than a gap.**
///
/// Both sibling tables of this slice refuse `DELETE` outright and copying that
/// here would be precisely wrong: a crashed run is terminal *and* still holding
/// rows, which is exactly when release matters most. D-37 puts the release on
/// lease takeover and on the operator's abort, and a crashed import must never
/// freeze interactive authoring indefinitely.
#[tokio::test]
async fn a_lock_is_releasable_whatever_state_its_run_reached() {
    for state in ["committing", "completed", "completed_with_conflicts"] {
        let conn = migrated_db().await;
        committing_run(&conn).await;
        must_succeed(&conn, &take_lock(RUN, TENANT, PRICE)).await;
        if state != "committing" {
            must_succeed(&conn, &advance(RUN, state)).await;
        }
        must_succeed(
            &conn,
            &format!(
                "DELETE FROM pricing_bulk_row_lock \
                 WHERE tenant_id = '{TENANT}' AND price_id = '{PRICE}'"
            ),
        )
        .await;
        assert_eq!(
            scalar(
                &conn,
                &format!(
                    "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_bulk_row_lock \
                     WHERE tenant_id = '{TENANT}' AND price_id = '{PRICE}'"
                )
            )
            .await,
            "0",
            "the release must actually have taken effect, not merely not errored"
        );
    }
}

/// Both keys, each asserted by making **its own** referent absent — and this is
/// the shape rather than one statement missing both, because `SQLite` names
/// neither the constraint nor the table in a key violation (the message is the
/// bare `FOREIGN KEY constraint failed`), so a case that removed two referents at
/// once would pass on either key and prove neither.
///
/// **No pragma.** `sqlx` puts `foreign_keys = ON` in its default pragma set, so
/// every connection this suite opens already enforces them; `toolkit_db`'s own
/// parser handles `journal_mode` and `synchronous` only and never touches this
/// one. Where `sqlite_bundle_store` does set it the reason is the opposite of
/// this one's: it asserts the **absence** of a key with `must_succeed`, which
/// pragma-off would make vacuous.
#[tokio::test]
async fn both_keys_name_a_row_that_exists() {
    // No run. The lock's INSERT arms defer when the run is absent, so the key is
    // what answers rather than a trigger reporting a tenancy or state fault the
    // caller does not have.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_price(TENANT)).await;
    must_be_rejected(&conn, &take_lock(OTHER_RUN, TENANT, PRICE), "FOREIGN KEY").await;

    // No price row.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_run(RUN, TENANT)).await;
    must_succeed(&conn, &advance(RUN, "committing")).await;
    must_be_rejected(&conn, &take_lock(RUN, TENANT, PRICE), "FOREIGN KEY").await;

    // With both present the same statement lands, so each refusal above is its
    // own key rather than something else about the statement.
    let conn = migrated_db().await;
    committing_run(&conn).await;
    must_succeed(&conn, &take_lock(RUN, TENANT, PRICE)).await;
}

/// **An import takes a lock too, and this case exists to pin an absence.**
///
/// `pricing_repricing_journal` carries an arm admitting only `kind = repricing`,
/// and the symmetry is a trap: `inst-bk-lock` is the **import's** own rule — "rows
/// in an in-flight import are marked" — and `inst-bs-commit` puts *every* import
/// on the edge into `committing`. Without this case every fixture in this file
/// seeds a repricing run, so an arm copied here from the sibling would leave the
/// whole suite green while every bulk import failed to take its lock at commit.
#[tokio::test]
async fn an_import_takes_a_lock_like_any_other_run() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_price(TENANT)).await;
    must_succeed(&conn, &seed_run_of_kind(RUN, TENANT, "import")).await;
    must_succeed(&conn, &advance(RUN, "committing")).await;
    must_succeed(&conn, &take_lock(RUN, TENANT, PRICE)).await;
    assert_eq!(
        locks_held(&conn).await,
        "1",
        "an import holds its rows exactly as a repricing run does"
    );
}
