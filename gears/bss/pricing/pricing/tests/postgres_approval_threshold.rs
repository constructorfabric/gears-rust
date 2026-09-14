//! `pricing_approval_threshold` on the backend it targets — one executed refusal
//! per guard, and the proof that the pair it replaces is gone.
//!
//! `pricing_approval_threshold` is the migration `design/05-governance.md` §6's
//! *"per-currency `{absolute_minor | percent}` thresholds"* finally has a shape
//! for, and D-10's *"the diff applies only after an independent `FinanceReviewer`
//! approves"* finally has a place to put a proposal. Everything it declares is a
//! refusal, and a refusal nothing executes is a `CHECK (1 = 1)` nobody has looked
//! at — the Phase-2 review of `pricing_price` found fourteen of those with the
//! whole crate green.
//!
//! # Staging, because three of the five CHECKs overlap
//!
//! Every row this suite writes is **valid but for the one thing under test**,
//! because `chk_pricing_approval_threshold_basis` refuses a great many of the
//! statements its siblings refuse:
//!
//! * `..._absolute_non_negative` needs `percent_bp` left NULL, or the basis
//!   constraint answers a row carrying `-1` and a percentage together;
//! * `..._percent_positive` needs `absolute_minor` left NULL, for the mirror
//!   reason;
//! * `..._currency` needs exactly one basis set, or the row is refused before its
//!   currency is looked at at all.
//!
//! `..._currency` is exercised with a **two**-character code and deliberately not
//! with a four-character one: `varchar(3)` refuses the long one itself, with a
//! type error rather than a constraint violation, so a test written that way would
//! pass with the CHECK deleted.
//!
//! # The two triggers are two triggers, and that is what makes them provable
//!
//! Every other table in this chain binds one `_append_only` function to `BEFORE
//! UPDATE OR DELETE`. This one has `trg_pricing_approval_threshold_no_delete` and
//! `trg_pricing_approval_threshold_no_update` separately, so removing either lets
//! exactly its own statement through instead of leaving it refused by the other
//! arm under a different sentence — which would make the proof a proof about a
//! message.
//!
//! # The tombstone table is here too, because it is the same store's other half
//!
//! `pricing_approval_threshold_tombstone` adds `pricing_approval_threshold_tombstone` (D-185) — one row
//! per version that has **no** currencies, which is the shape the entry table's
//! `(tenant_id, version, currency)` key cannot express and therefore the only way back
//! to §6's *"unset ⇒ two-person rule always"*. Its guards are asserted beside its
//! sibling's rather than in a suite of their own, because the property that matters is
//! about the **pair**: one version sequence, two tables, and no schema constraint
//! spanning them. The last case in this file is what pins that last clause.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_approval_threshold -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";
const AT: &str = "'2099-01-01T00:00:00Z'";
/// The values the immutability cases try to move a key column to.
const OTHER_TENANT: &str = "22222222-2222-2222-2222-222222222222";
const OTHER_ACTOR: &str = "55555555-5555-5555-5555-555555555555";

/// A fresh database carrying the applied chain, on the one shared server.
///
/// The connection is a **plain** one, with no search path: every statement here is
/// raw SQL that reaches past every repository, because the repository is exactly
/// the layer that cannot see a guard stop refusing.
async fn applied() -> DatabaseConnection {
    Pg::applied().await.raw().await
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

/// Reject, **and for the stated reason**.
///
/// Per-suite rather than hoisted, for the reason `tests/pg_support/mod.rs` gives:
/// a shared helper is the weakest of the ones it replaces. The sharpening here is
/// that the message must name **this** table as well as the fragment under test —
/// every CHECK, trigger and key over it carries `pricing_approval_threshold` in
/// its name or its message, so a bare fragment match could be satisfied by a
/// neighbour on another table.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the guard `{because}` must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("pricing_approval_threshold"),
        "the rejection must name the table it came from, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// One entry row, valid unless an override makes it otherwise.
fn entry(overrides: &[(&str, &str)]) -> String {
    let mut columns: Vec<(&str, String)> = vec![
        ("tenant_id", format!("'{TENANT}'")),
        ("version", "0".to_owned()),
        ("currency", "'USD'".to_owned()),
        ("absolute_minor", "50000".to_owned()),
        ("percent_bp", "NULL".to_owned()),
        ("effective_from", AT.to_owned()),
        ("created_by", format!("'{ACTOR}'")),
    ];
    for (name, value) in overrides {
        match columns.iter_mut().find(|(column, _)| column == name) {
            Some(slot) => (*value).clone_into(&mut slot.1),
            None => columns.push((name, (*value).to_owned())),
        }
    }
    let names = columns
        .iter()
        .map(|(column, _)| *column)
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO bss.pricing_approval_threshold ({names}) VALUES ({values})")
}

/// The baseline: a well-formed entry lands, so every refusal below is a refusal of
/// the one thing it changed and not of the shape in general.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_well_formed_entry_lands_on_each_basis() {
    let conn = applied().await;
    must_succeed(&conn, &entry(&[])).await;
    must_succeed(
        &conn,
        &entry(&[
            ("currency", "'EUR'"),
            ("absolute_minor", "NULL"),
            ("percent_bp", "500"),
        ]),
    )
    .await;
}

/// §6's `{absolute_minor | percent}` is a **choice**, and neither is not one.
///
/// An entry with no basis at all still makes its currency "a currency with an
/// entry", so `inst-mat-percurrency`'s fail-safe stops firing for it while nothing
/// thresholds anything — the fail-safe switched off by an empty row.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_threshold_entry_with_neither_basis_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &entry(&[("absolute_minor", "NULL"), ("percent_bp", "NULL")]),
        "chk_pricing_approval_threshold_basis",
    )
    .await;
}

/// And both is not a choice either: the evaluator would have to pick one, and
/// nothing in the design set says which.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_threshold_entry_with_both_bases_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &entry(&[("absolute_minor", "50000"), ("percent_bp", "500")]),
        "chk_pricing_approval_threshold_basis",
    )
    .await;
}

/// §6's `absolute_minor ≥ 0`. A negative threshold is below every change there
/// is, which is a two-person rule switched off by arithmetic.
///
/// `percent_bp` stays NULL, and it has to: with both set the basis constraint
/// answers first and this test would prove nothing about the one it names.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_absolute_threshold_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &entry(&[("absolute_minor", "-1")]),
        "chk_pricing_approval_threshold_absolute_non_negative",
    )
    .await;
    // Zero is a real threshold: everything is material.
    must_succeed(&conn, &entry(&[("absolute_minor", "0")])).await;
}

/// §6's `percent > 0`, verbatim. Zero would auto-publish a change that moved by
/// nothing at all, which is the one comparison a percentage cannot make.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_non_positive_percent_threshold_is_refused() {
    let conn = applied().await;
    for bp in ["0", "-1"] {
        must_be_rejected(
            &conn,
            &entry(&[("absolute_minor", "NULL"), ("percent_bp", bp)]),
            "chk_pricing_approval_threshold_percent_positive",
        )
        .await;
    }
    must_succeed(
        &conn,
        &entry(&[("absolute_minor", "NULL"), ("percent_bp", "1")]),
    )
    .await;
}

/// The ISO 4217 **shape**. A two-character code is the statement only this CHECK
/// refuses — `varchar(3)` refuses a longer one itself, with a type error, so the
/// short side is where the constraint is observable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_currency_of_the_wrong_length_is_refused() {
    let conn = applied().await;
    for code in ["'US'", "''"] {
        must_be_rejected(
            &conn,
            &entry(&[("currency", code)]),
            "chk_pricing_approval_threshold_currency",
        )
        .await;
    }
}

/// Versions are append-only and monotone, and a negative one would sort under the
/// tenant's first proposal forever.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_version_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &entry(&[("version", "-1")]),
        "chk_pricing_approval_threshold_version",
    )
    .await;
    must_succeed(&conn, &entry(&[("version", "0")])).await;
}

/// One version holds **one** entry per currency, by the primary key.
///
/// Two entries for one currency in one version is a policy with two answers for
/// the same row, and whichever the evaluator read would depend on row order.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_version_holds_one_entry_per_currency() {
    let conn = applied().await;
    must_succeed(&conn, &entry(&[])).await;
    must_be_rejected(
        &conn,
        &entry(&[("absolute_minor", "1")]),
        "pricing_approval_threshold_pkey",
    )
    .await;
    // A different currency in the same version, and the same currency in a
    // different version, are both the normal case.
    must_succeed(&conn, &entry(&[("currency", "'EUR'")])).await;
    must_succeed(&conn, &entry(&[("version", "1")])).await;
}

/// `DELETE` is refused unconditionally: a superseded threshold is a fact an
/// auditor is entitled to see, and it is what an earlier approval's pin covers.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_deleted_threshold_version_is_refused() {
    let conn = applied().await;
    must_succeed(&conn, &entry(&[])).await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_approval_threshold WHERE tenant_id = '{TENANT}'"),
        "append-only history",
    )
    .await;
}

/// `UPDATE` is refused unconditionally, and every column is why.
///
/// There is no whitelist here as there is on `pricing_price_window`, because every
/// column of this row is content: the keys, both bases, the instant it takes
/// effect and the provenance of the proposal. A correction is a new version.
///
/// Driven column by column, because "unconditionally" is a claim about the set
/// and one statement is evidence about one member of it. Today's trigger takes
/// one path for every column — no `WHEN`, no column list, unlike
/// `pricing_price_window`'s whitelist — so the loop is a guard against this table
/// growing one, which is exactly the shape a later exemption would arrive in.
///
/// The roster is the **table's** columns and not `entry`'s arguments:
/// `created_at` defaults rather than being written, and the migration's own doc
/// counts it as content. The `BEFORE UPDATE` trigger fires ahead of the row's
/// CHECKs, so a move that would also break the basis constraint is still refused
/// by the guard under test and not by a neighbour.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_updated_threshold_version_is_refused() {
    let conn = applied().await;
    must_succeed(&conn, &entry(&[])).await;
    for (column, moved_to) in [
        ("tenant_id", format!("'{OTHER_TENANT}'")),
        ("version", "1".to_owned()),
        ("currency", "'EUR'".to_owned()),
        ("absolute_minor", "1".to_owned()),
        ("percent_bp", "1".to_owned()),
        ("effective_from", "'2099-06-01T00:00:00Z'".to_owned()),
        ("created_by", format!("'{OTHER_ACTOR}'")),
        ("created_at", "'2099-06-01T00:00:00Z'".to_owned()),
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_approval_threshold SET {column} = {moved_to} \
                 WHERE tenant_id = '{TENANT}'"
            ),
            "is immutable",
        )
        .await;
    }
}

/// **The same migration removed the pair this table replaces.**
///
/// Asserted rather than assumed, because leaving the old columns behind is the
/// failure this shape exists to avoid: two places to read a threshold from, one of
/// which no writer maintains. A `SELECT` naming either column must fail with an
/// undefined-column error rather than answering NULL.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_policy_objects_old_threshold_pair_is_gone() {
    let conn = applied().await;
    for column in ["approval_threshold_minor", "approval_threshold_currency"] {
        let err = exec(
            &conn,
            &format!("SELECT {column} FROM bss.pricing_policy_object"),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("pricing_policy_object must no longer carry {column}"));
        let message = err.to_string();
        assert!(
            message.contains(column),
            "the error must name the dropped column, got: {message}"
        );
    }
    // The table itself is untouched and still readable, so this is a column drop
    // and not a table that failed to migrate.
    must_succeed(&conn, "SELECT tenant_id FROM bss.pricing_policy_object").await;
}

// ---------------------------------------------------------------------------
// D-185's tombstone table, on the backend it targets.
// ---------------------------------------------------------------------------

/// One tombstone row, valid unless an override makes it otherwise.
fn tombstone(overrides: &[(&str, &str)]) -> String {
    let mut columns: Vec<(&str, String)> = vec![
        ("tenant_id", format!("'{TENANT}'")),
        ("version", "0".to_owned()),
        ("effective_from", AT.to_owned()),
        ("created_by", format!("'{ACTOR}'")),
    ];
    for (name, value) in overrides {
        match columns.iter_mut().find(|(column, _)| column == name) {
            Some(slot) => (*value).clone_into(&mut slot.1),
            None => columns.push((name, (*value).to_owned())),
        }
    }
    let names = columns
        .iter()
        .map(|(column, _)| *column)
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO bss.pricing_approval_threshold_tombstone ({names}) VALUES ({values})")
}

/// The baseline, and the thing the entry table cannot hold: **a version with no
/// currency at all**.
///
/// It is what makes §6's *"unset ⇒ two-person rule always"* a state a tenant can
/// return to. The entry table's key is `(tenant_id, version, currency)`, so a version
/// naming no currency has zero rows there — invisible to `latest_version`, and
/// indistinguishable from a version nobody proposed.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_well_formed_tombstone_lands_and_holds_one_row_per_version() {
    let conn = applied().await;
    must_succeed(&conn, &tombstone(&[])).await;
    // One tombstone per version, by the primary key: a second row would be a
    // retirement with two authored instants and no answer to which one an approver
    // signed.
    must_be_rejected(
        &conn,
        &tombstone(&[("effective_from", "'2099-06-01T00:00:00Z'")]),
        "pricing_approval_threshold_tombstone_pkey",
    )
    .await;
    must_succeed(&conn, &tombstone(&[("version", "1")])).await;
}

/// `version >= 0` here too, because the two threshold tables are one sequence and
/// therefore carry one rule.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_tombstone_version_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &tombstone(&[("version", "-1")]),
        "chk_pricing_approval_threshold_tombstone_version",
    )
    .await;
    must_succeed(&conn, &tombstone(&[("version", "0")])).await;
}

/// `DELETE` is refused: a retirement is what an approval's pin covers, and an
/// auditor asking when this tenant stopped having thresholds reads exactly this row.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_deleted_tombstone_is_refused() {
    let conn = applied().await;
    must_succeed(&conn, &tombstone(&[])).await;
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_approval_threshold_tombstone WHERE tenant_id = '{TENANT}'"
        ),
        "append-only history",
    )
    .await;
}

/// `UPDATE` is refused, and `effective_from` is why it matters most: it is **when the
/// two-person rule comes back**, and it is inside the digest the reviewer signed.
///
/// Every column all the same, for the entry table's reason: the sentence above
/// names one column's stakes, not one column's coverage. `created_at` included,
/// which is the table's column rather than `tombstone`'s argument.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_updated_tombstone_is_refused() {
    let conn = applied().await;
    must_succeed(&conn, &tombstone(&[])).await;
    for (column, moved_to) in [
        ("tenant_id", format!("'{OTHER_TENANT}'")),
        ("version", "1".to_owned()),
        ("effective_from", "'2099-06-01T00:00:00Z'".to_owned()),
        ("created_by", format!("'{OTHER_ACTOR}'")),
        ("created_at", "'2099-06-01T00:00:00Z'".to_owned()),
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_approval_threshold_tombstone SET {column} = {moved_to} \
                 WHERE tenant_id = '{TENANT}'"
            ),
            "is immutable",
        )
        .await;
    }
}

/// **Nothing in either schema refuses one version number in both tables**, and that
/// is asserted rather than assumed.
///
/// It is the premise `threshold_repo::read_version`'s `CorruptRow` arm rests on: the
/// two tables have two primary keys and neither sees the other, so the ambiguous state
/// is reachable and the reader is the only thing that can fail closed on it. A build
/// that quietly grew a cross-table trigger would make that arm dead code, and this is
/// the test that would say so.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_two_tables_do_not_refuse_each_others_version_numbers() {
    let conn = applied().await;
    must_succeed(&conn, &entry(&[])).await;
    must_succeed(&conn, &tombstone(&[])).await;
}
