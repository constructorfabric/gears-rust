//! `pricing_group_membership`'s non-overlap invariant (D-09), proved by
//! **executing the statement the schema layer must refuse**, on Postgres.
//!
//! # What this suite is for
//!
//! `tests/postgres_migrations.rs` pins this table's two CHECKs and its primary
//! key by name; it issues no DML, so it says the objects reached the server and
//! nothing about what any of them refuses. This is the other half, for the one
//! object that census cannot see at all:
//! `excl_pricing_group_membership_no_overlap` is a `contype = 'x'` exclusion
//! constraint, not a `CHECK`, so `postgres_migrations.rs`'s `CHECKS_SQL` does not
//! and should not name it — this suite is its only proof of presence and its
//! only proof of refusal both.
//!
//! # The case D-09 is actually about
//!
//! `design/09-price-overlays.md` §3 `inst-cg-resolve` states the rule as
//! *"membership intervals are non-overlapping per payer **across all groups** at
//! any instant"* — not per `(payer, group)`. The exclusion constraint's equality
//! list is `(tenant_id, payer_tenant_id)`, deliberately omitting `group_value`
//! (see `pricing_group_membership`'s migration doc), so the sharpest test this suite can
//! run is two *different* groups colliding for one payer
//! (`two_different_groups_may_not_overlap_for_one_payer`) — a same-group overlap
//! is the degenerate case of the same rule and is proved alongside it rather
//! than instead of it.
//!
//! # Every statement here is raw SQL, past every repository
//!
//! No `membership_repo` exists yet (this migration is the storage layer only —
//! see its module doc's closing section), so there is no other layer to write
//! through; the repository-level second check the design set calls for is owed,
//! not built here.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_group_membership -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
/// A second tenant, for the scoping check: an exclusion constraint's equality
/// list is `(tenant_id, payer_tenant_id)`, and a test that never varied the
/// tenant could not tell that column from a no-op.
const OTHER_TENANT: &str = "99999999-9999-9999-9999-999999999999";
const PAYER: &str = "22222222-2222-2222-2222-222222222222";
/// A second payer, for the same reason on the other equality column.
const OTHER_PAYER: &str = "33333333-3333-3333-3333-333333333333";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";

const GROUP_TRIAL: &str = "trial";
const GROUP_VIP: &str = "vip";

const M1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const M2: &str = "aaaaaaaa-0000-0000-0000-000000000002";
const M3: &str = "aaaaaaaa-0000-0000-0000-000000000003";
const M4: &str = "aaaaaaaa-0000-0000-0000-000000000004";

const CREATED_AT: &str = "'2026-08-11 09:00:00+00'";

async fn applied() -> (Pg, DatabaseConnection) {
    let pg = Pg::applied().await;
    let conn = pg.raw().await;
    (pg, conn)
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    conn.execute_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .map(|_| ())
}

async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

/// Reject, **and by the named object** — `excl_pricing_group_membership_no_overlap`
/// throughout this suite, so a refusal from an unrelated guard cannot pass silently.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, by: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the guard `{by}` must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains(by),
        "the rejection must be the one under test (`{by}`), got: {message}"
    );
}

/// An INSERT for one membership row. `effective_to` is passed as a literal SQL
/// fragment (`'...'` or `NULL`) so callers can express the open-ended case.
#[allow(clippy::too_many_arguments)]
fn insert(
    id: &str,
    tenant: &str,
    payer: &str,
    group_value: &str,
    effective_from: &str,
    effective_to: &str,
) -> String {
    format!(
        "INSERT INTO bss.pricing_group_membership (
             membership_id, tenant_id, payer_tenant_id, group_value,
             effective_from, effective_to, created_by, created_at_utc)
         VALUES ('{id}', '{tenant}', '{payer}', '{group_value}',
             '{effective_from}', {effective_to}, '{ACTOR}', {CREATED_AT})"
    )
}

const CONSTRAINT: &str = "excl_pricing_group_membership_no_overlap";

/// D-09's own case: two **different** groups colliding for one payer.
///
/// This is the refusal the migration's whole non-`group_value`-scoped equality
/// list exists for. A constraint scoped `(tenant_id, payer_tenant_id,
/// group_value)` would admit this insert — the false negative the module doc
/// calls out by name — so this is the sharpest thing this suite can prove.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_different_groups_may_not_overlap_for_one_payer() {
    let (_pg, conn) = applied().await;
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
    )
    .await;

    // `vip` from 2026-03-01, squarely inside the `trial` interval above, and in
    // a *different* group entirely.
    must_be_rejected(
        &conn,
        &insert(
            M2,
            TENANT,
            PAYER,
            GROUP_VIP,
            "2026-03-01 00:00:00+00",
            "'2026-09-01 00:00:00+00'",
        ),
        CONSTRAINT,
    )
    .await;
}

/// **The `CHECK` no statement on either engine executes**, with both of its
/// refusing shapes and its boundary.
///
/// Its neighbour `chk_pricing_group_membership_group_value_present` is executed --
/// `postgres_migrations` and `sqlite_migrations` each fire the whole stripped-
/// whitespace set at it with a padded control. This one appears in those two files
/// only in their name rosters, so until now nothing anywhere submitted a row it
/// refuses and it could have been dropped, or narrowed to a tautology, with every
/// gate green.
///
/// `effective_to > effective_from`, so equality is refused as well as inversion --
/// an instantaneous membership is a row the resolver would read as covering
/// nothing while occupying the payer's timeline.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_interval_that_does_not_move_forward_is_refused_by_its_own_check() {
    const CHECK: &str = "chk_pricing_group_membership_interval";

    let (_pg, conn) = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-06-01 00:00:00+00",
            "'2026-01-01 00:00:00+00'",
        ),
        CHECK,
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-06-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
        CHECK,
    )
    .await;
    // And the open-ended row the `IS NULL` disjunct admits, which is what stops
    // this pair being satisfied by a constraint that refused every interval.
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-06-01 00:00:00+00",
            "NULL",
        ),
    )
    .await;
}

/// The narrower, degenerate case: two intervals in the **same** group. Proved
/// alongside the cross-group case rather than instead of it, per the module
/// doc's `MEMBERSHIP_OVERLAP` note.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_intervals_in_the_same_group_may_not_overlap_either() {
    let (_pg, conn) = applied().await;
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &insert(
            M2,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-03-01 00:00:00+00",
            "'2026-09-01 00:00:00+00'",
        ),
        CONSTRAINT,
    )
    .await;
}

/// Scheduled sequential future-dated memberships are legal (2026-07-28 review
/// fix, confirmed 2026-07-31, `inst-cg-resolve`): a rule that refused these
/// would be wrong, not merely stricter than necessary.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_sequential_future_dated_memberships_are_accepted() {
    let (_pg, conn) = applied().await;
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2099-01-01 00:00:00+00",
            "'2099-06-01 00:00:00+00'",
        ),
    )
    .await;
    // Starts well after the first ends — no adjacency, no overlap, both in the
    // future relative to `now()` at test time.
    must_succeed(
        &conn,
        &insert(
            M2,
            TENANT,
            PAYER,
            GROUP_VIP,
            "2099-08-01 00:00:00+00",
            "'2099-12-01 00:00:00+00'",
        ),
    )
    .await;
}

/// Boundary: an interval starting **exactly** where another ends is legal,
/// because the interval is half-open `[from, to)`. Proved by executing the
/// insert, not assumed from the DDL.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_interval_starting_where_another_ends_is_not_an_overlap() {
    let (_pg, conn) = applied().await;
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
    )
    .await;
    // Starts at exactly the first row's `effective_to`. `[)` reads this as
    // adjacency, not collision.
    must_succeed(
        &conn,
        &insert(
            M2,
            TENANT,
            PAYER,
            GROUP_VIP,
            "2026-06-01 00:00:00+00",
            "'2026-09-01 00:00:00+00'",
        ),
    )
    .await;
}

/// An **open-ended** membership (`effective_to = NULL`) still collides with a
/// later interval that starts inside it — the open end must read as unbounded,
/// not as "no constraint applies".
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_open_ended_membership_still_collides_with_a_later_interval() {
    let (_pg, conn) = applied().await;
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "NULL",
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            M2,
            TENANT,
            PAYER,
            GROUP_VIP,
            "2026-03-01 00:00:00+00",
            "'2026-09-01 00:00:00+00'",
        ),
        CONSTRAINT,
    )
    .await;
}

/// The equality list is `(tenant_id, payer_tenant_id)`, both ways — a
/// colliding interval for a **different payer**, or a **different tenant**
/// holding the same payer id, is unconstrained. Without this the suite could
/// not tell a real scoping rule from a constraint that happened to be looser
/// than intended in one direction.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_scope_is_per_tenant_and_per_payer_not_narrower_than_either() {
    let (_pg, conn) = applied().await;
    must_succeed(
        &conn,
        &insert(
            M1,
            TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
    )
    .await;

    // Same tenant, a different payer, the identical interval and group: no
    // collision, because the payer differs.
    must_succeed(
        &conn,
        &insert(
            M3,
            TENANT,
            OTHER_PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
    )
    .await;

    // A different tenant, the same payer id and the identical interval: no
    // collision either, because the tenant differs.
    must_succeed(
        &conn,
        &insert(
            M4,
            OTHER_TENANT,
            PAYER,
            GROUP_TRIAL,
            "2026-01-01 00:00:00+00",
            "'2026-06-01 00:00:00+00'",
        ),
    )
    .await;
}
