//! `pricing_approval`'s constraints and triggers, proved by **executing the
//! statement each of them must refuse**, on Postgres.
//!
//! `tests/postgres_migrations.rs` says the objects reached the server; it issues
//! no DML and is therefore evidence of presence only. This suite is the other
//! half for one table: every CHECK and every branch of
//! `bss.pricing_approval_append_only()` answers a statement written to trip it,
//! and the assertion names the object the answer came from. A suite that wrote
//! only valid rows would catch a constraint that got *narrower* and never one
//! that stopped refusing — the defect class that let fourteen `pricing_price`
//! CHECKs each be replaceable with `CHECK (1 = 1)` with the whole crate green.
//!
//! Two rules every test here follows, both from `postgres_migrations.rs`'s
//! module doc:
//!
//! **Put the world in the state where the object under test is what answers.**
//! A refusal an earlier guard produces is not evidence about the guard the test
//! names — which is why the trigger cases seed a valid row first and the CHECK
//! cases vary exactly one column of an otherwise-valid row.
//!
//! **Every guard must be provable by removal.** Drop the constraint or the
//! trigger branch, watch *exactly one* test fail, restore. The mapping is
//! recorded in the group report.
//!
//! # Why the decided rows are reached by `UPDATE` and not by `INSERT`
//!
//! A `pricing_approval` row is **born `submitted`** — the trigger's `INSERT`
//! branch refuses every other state, because §4 names `submitted` as the
//! machine's initial state and because a row born `approved` would hand publish
//! its authorization with no decision ever having been made. That branch is what
//! `a_record_is_born_submitted_or_it_is_not_born` executes, and it is also why
//! every case below that needs a *decided* row seeds a pending one and flips it
//! through the sanctioned `UPDATE`: minting the decided row directly is now
//! refused by the trigger, and a CHECK test that tripped the trigger instead
//! would be evidence about the wrong object.
//!
//! # Both enumerations are driven off the domain's `ALL`, on purpose
//!
//! D-158 requires `pricing_approval` and `pricing_audit_log` to spell one
//! `subject_kind` vocabulary and to be **extended together**. A test naming the
//! two tokens as literals would go on passing the day a third is added to
//! `AuditSubjectKind` and not to `chk_pricing_approval_subject_kind` — which is
//! exactly the drift D-158 exists to prevent — so the storable cases range over
//! `AuditSubjectKind::ALL`, and the state cases over `ApprovalState::ALL` with
//! each row's decision columns taken from that state's own
//! `requires_approver` / `requires_reason`. The refused tokens stay literal, and the
//! one this file uses is **`overlay`** — a member of S5 §6's list that this gear
//! declares no writer for. It used to be `window`, and that sentence went stale in the
//! change that mounted the window surfaces: `window` is declared now, admitted by the
//! CHECK on both backends, and written by `infra::window` and by the unit a window
//! mutation opens. `a_subject_kind_with_no_writer_is_refused` carries a guard that
//! makes the next such move deliberate — it fails if its own literal ever becomes
//! declared, rather than quietly asserting a refusal the store has stopped performing.
//!
//! The `SQLite` mirror carries the same CHECKs by name and splits the one
//! PL/pgSQL function into five literal-message triggers, so
//! `tests/sqlite_approval_repo.rs` and `tests/sqlite_approval_append_only.rs`
//! reach most of this without Docker. What they cannot reach is the branch
//! structure of the PL/pgSQL body — a function may reference a column that does
//! not exist and still be created, failing only when it fires — and that is what
//! these tests execute.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use bss_pricing::domain::approval::ApprovalState;
use bss_pricing::domain::audit::AuditSubjectKind;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const SUBJECT: &str = "22222222-2222-2222-2222-222222222222/3";
const SUBMITTER: &str = "44444444-4444-4444-4444-444444444444";
const APPROVER: &str = "55555555-5555-5555-5555-555555555555";
const PENDING: &str = "66666666-6666-6666-6666-666666666666";

const DECIDED_AT: &str = "'2026-08-03 10:00:00+00'";

/// A fresh database carrying the applied chain, on the one shared server.
///
/// **One container per binary and a `CREATE DATABASE` per test**, not a
/// container per test; `tests/pg_support/mod.rs` states why, and the short
/// version is that a `PortNotExposed` flake is indistinguishable from the second
/// test a guard-by-removal was not supposed to redden.
///
/// The connection handed back is a **plain** one: the toolkit `Db` is the
/// runner's handle, and everything this suite executes is deliberately raw SQL
/// that reaches past every repository.
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

/// Run one statement that must land.
///
/// As load-bearing as the refusals: every guard here is a whitelist rather than
/// a blanket ban, and a test suite that only ever saw refusals would pass
/// against a table nothing can be written to at all.
async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

/// Reject, **and by the named object**.
///
/// The fragment is the whole assertion. This table carries six CHECK
/// constraints and one trigger, every one of whose names contains
/// `pricing_approval`; a test that accepted any error naming the table would
/// pass with the guard it means to prove switched off, refused instead by a
/// neighbour it never intended to trip.
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

/// Read one value back, from a query that aliases it `v`.
async fn scalar(conn: &DatabaseConnection, sql: &str) -> String {
    conn.query_one_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .expect("query")
    .expect("one row")
    .try_get::<String>("", "v")
    .expect("read value")
}

/// A distinct approval id per case, so one test's rows cannot answer for
/// another's.
fn id_at(n: u8) -> String {
    format!("00000000-0000-0000-0000-0000000000{n:02x}")
}

/// One `submitted` record under `id`, valid in every respect.
async fn seed_pending_as(conn: &DatabaseConnection, id: &str) {
    must_succeed(
        conn,
        &insert(id, "submitted", "NULL", "NULL", "NULL", "plan_revision"),
    )
    .await;
}

/// One `submitted` record, valid in every respect, for the trigger cases to move.
async fn seed_pending(conn: &DatabaseConnection) {
    seed_pending_as(conn, PENDING).await;
}

/// An `INSERT` of one record, with the six varying columns supplied.
fn insert(
    id: &str,
    state: &str,
    approver: &str,
    reason: &str,
    decided_at: &str,
    subject_kind: &str,
) -> String {
    format!(
        "INSERT INTO bss.pricing_approval (
            approval_id, tenant_id, subject_ref, subject_kind, content_hash,
            state, submitter_principal, approver_principal, reason, materiality,
            submitted_at, decided_at)
         VALUES ('{id}', '{TENANT}', '{SUBJECT}', '{subject_kind}', '\\xdeadbeef',
            '{state}', '{SUBMITTER}', {approver}, {reason}, '{{}}'::jsonb,
            '2026-08-03 09:00:00+00', {decided_at})"
    )
}

/// The four decision columns a row in `state` must carry to satisfy every CHECK
/// on this table, taken from the domain's own predicates rather than restated.
///
/// `requires_approver` mirrors `chk_pricing_approval_approver` and
/// `requires_reason` mirrors `chk_pricing_approval_reason`; a state added to the
/// machine without a matching CHECK arm shows up here as a row the store
/// refuses.
fn decision_columns(state: ApprovalState) -> (String, String, String) {
    let approver = if state.requires_approver() {
        format!("'{APPROVER}'")
    } else {
        "NULL".to_owned()
    };
    let reason = if state.requires_reason() {
        "'margin below floor'".to_owned()
    } else {
        "NULL".to_owned()
    };
    let decided_at = if state.is_pending() {
        "NULL".to_owned()
    } else {
        DECIDED_AT.to_owned()
    };
    (approver, reason, decided_at)
}

/// The sanctioned flip, spelled as the store sees it.
fn update_to(id: &str, state: ApprovalState) -> String {
    let (approver, reason, decided_at) = decision_columns(state);
    format!(
        "UPDATE bss.pricing_approval
            SET state = '{}', approver_principal = {approver}, reason = {reason},
                decided_at = {decided_at}
          WHERE approval_id = '{id}'",
        state.as_str()
    )
}

// ---------------------------------------------------------------------------
// The CHECK constraints
// ---------------------------------------------------------------------------

/// The valid rows, first — the world in which every refusal below is a fact
/// about the constraint it names.
///
/// One per state, because the four constraints that mention `state` disagree
/// about which columns each state permits, and a suite that only ever wrote a
/// `submitted` row would leave three quarters of that unexercised. Each decided
/// row is *reached* rather than minted: the record is born `submitted` and
/// flipped, which is the only route the store leaves open.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_state_the_machine_reaches_is_storable() {
    let conn = applied().await;

    for (n, state) in ApprovalState::ALL.iter().enumerate() {
        let id = id_at(0x10 + u8::try_from(n).expect("the machine has four states"));
        seed_pending_as(&conn, &id).await;
        if !state.is_pending() {
            must_succeed(&conn, &update_to(&id, *state)).await;
        }
    }

    // And every subject kind D-158 obliges this store to declare. Ranged over
    // `ALL` so that widening `AuditSubjectKind` alone fails here rather than
    // going unnoticed until the two stores disagree about one decision.
    for (n, kind) in AuditSubjectKind::ALL.iter().enumerate() {
        let id = id_at(0x20 + u8::try_from(n).expect("the enumeration is short"));
        must_succeed(
            &conn,
            &insert(&id, "submitted", "NULL", "NULL", "NULL", kind.as_str()),
        )
        .await;
    }
}

/// A record is born `submitted`, and every other birth is refused.
///
/// The hole this closes: the trigger once guarded `UPDATE` and `DELETE` only, so
/// a row could arrive `approved` — with a valid approver, a valid `decided_at`
/// and every CHECK satisfied — having bypassed the whole decision plane
/// *because there was no `UPDATE` on which to bypass it*. On the one table whose
/// purpose is to record that a second human agreed, that is the two-person rule
/// defeated by a single statement.
///
/// Each row below satisfies all six CHECKs, so the trigger is the only object
/// that can refuse it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_record_is_born_submitted_or_it_is_not_born() {
    let conn = applied().await;

    for (n, state) in ApprovalState::ALL
        .iter()
        .filter(|state| !state.is_pending())
        .enumerate()
    {
        let (approver, reason, decided_at) = decision_columns(*state);
        must_be_rejected(
            &conn,
            &insert(
                &id_at(0x30 + u8::try_from(n).expect("three decided states")),
                state.as_str(),
                &approver,
                &reason,
                &decided_at,
                "plan_revision",
            ),
            "a record is born submitted",
        )
        .await;
    }
}

/// `inst-tp-distinct` at the storage layer.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_row_whose_approver_equals_its_submitter_is_refused() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
                SET state = 'approved', approver_principal = '{SUBMITTER}',
                    decided_at = {DECIDED_AT}
              WHERE approval_id = '{PENDING}'"
        ),
        "chk_pricing_approval_distinct_principals",
    )
    .await;
}

/// The other half of the same constraint, and the divergence this migration
/// records: an **open** record has no approver and must still be storable.
///
/// S5 §6's literal note is a bare `approver_principal <> submitter_principal`.
/// The `IS NULL` arm is spelled out so that a later tightening to
/// `IS DISTINCT FROM` — the spelling that means what the sentence says — cannot
/// silently make every pending record unwritable. This test is what would catch
/// it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_open_record_with_no_approver_is_storable() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            &id_at(0x02),
            "submitted",
            "NULL",
            "NULL",
            "NULL",
            "plan_revision",
        ),
    )
    .await;
}

/// The token set — reached with the trigger **switched off**, which is the only
/// state of the world in which this CHECK is what answers.
///
/// Both triggers standing between a caller and an unknown token get there first:
/// on `INSERT` the born-submitted branch refuses anything but `submitted`, and
/// on `UPDATE` the flip whitelist refuses anything but the three decided states.
/// So the constraint is genuinely shadowed on every reachable statement — which
/// is defence in depth and not redundancy, since it is what still holds if a
/// trigger is ever dropped. Disabling the trigger for one statement is how that
/// claim is *executed* rather than asserted; the earlier form of this test
/// inserted `withdrawn` and was answered by `chk_pricing_approval_approver`,
/// evidence about the wrong object.
///
/// The trigger is put back and its return proved, so nothing after this line
/// runs against a table whose guard this test switched off.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_state_outside_the_machine_is_refused() {
    let conn = applied().await;
    must_succeed(
        &conn,
        "ALTER TABLE bss.pricing_approval DISABLE TRIGGER trg_pricing_approval_append_only",
    )
    .await;

    // Valid in every other respect: an approver, a reason and a `decided_at`,
    // so the five neighbouring CHECKs are all satisfied.
    must_be_rejected(
        &conn,
        &insert(
            &id_at(0x03),
            "withdrawn",
            &format!("'{APPROVER}'"),
            "'withdrawn by the submitter'",
            DECIDED_AT,
            "plan_revision",
        ),
        "chk_pricing_approval_state",
    )
    .await;

    must_succeed(
        &conn,
        "ALTER TABLE bss.pricing_approval ENABLE TRIGGER trg_pricing_approval_append_only",
    )
    .await;
    // Read out of the catalog rather than by executing a branch: any statement
    // that proved the trigger is back would tie this test to whichever branch
    // answered it, and a later removal proof would then see two tests fail for
    // one guard. `tgenabled` is `O` — fires on origin — when the trigger is live.
    assert_eq!(
        scalar(
            &conn,
            "SELECT tgenabled::text AS v FROM pg_trigger
              WHERE tgname = 'trg_pricing_approval_append_only'",
        )
        .await,
        "O",
        "the guard this test switched off must be back on"
    );
}

/// D-158: the two stores spell one enumeration, and this one carries exactly
/// what `AuditSubjectKind` carries.
///
/// `overlay` is one of the members S5 §6 lists and this gear does not declare,
/// under the same section's "a token with no writer is not declared" rule. The
/// store refusing it is what makes the declaration a declaration rather than a
/// comment. It stays a literal because it is a token the *document* names and
/// the code deliberately does not — the storable direction is what ranges over
/// `AuditSubjectKind::ALL`, in `every_state_the_machine_reaches_is_storable`.
///
/// **The token has now moved four times, and the guard below forced every move
/// to be deliberate: it fails if the literal this test uses ever becomes
/// declared, so the test can never quietly assert a refusal the store has
/// stopped performing.**
///
/// It was `window` until the change that mounted the three window surfaces —
/// `window` is now declared, admitted by this CHECK on both backends, and written by
/// `infra::window` and by the pending unit a window mutation opens. Then `overlay`,
/// until 2026-08-06: D-221 gave the overlay plane its audit writer, D-158 obliged
/// this mirror to admit what the audit vocabulary declares, and `chk_pricing_approval_subject_kind`
/// widened the CHECK. Then `membership`, until 2026-08-11: `group_membership_repo`
/// (Task 4 of the customer-group plane) gave it an *audit*-plane writer and
/// `AuditSubjectKind::Membership` a variant — this CHECK was **not** widened
/// alongside it, deliberately (no approval-plane writer exists for this kind; see
/// `group_membership_repo`'s module doc), but the guard below tests
/// `AuditSubjectKind::ALL` and not this CHECK, so it still caught the move. Then,
/// briefly, `historical_import` — picked, and rejected in the same review round,
/// before it could land in history at all: S5 §6 already promises the plane it
/// names a `BackdateGrant` with a mandatory reason and a full audit contract
/// (`05-governance.md`), and it is a declared authz resource in this crate today
/// (`src/authz.rs`'s label, roster and `ResourceType`) — a plane the design set
/// already commits to auditing, which made it exactly the kind of "next
/// undeclared member" this probe keeps discovering the hard way.
///
/// **`not_a_subject_kind` is the replacement, and it is synthetic rather than
/// borrowed from S5 §6 — that is the fix, not a stronger version of the same
/// pick.** Every prior probe was a real domain word and every one of them
/// eventually got declared or, for `historical_import`, was already promised to
/// be. No domain word is safe for this probe, because the test's whole claim is
/// that an *undeclared* token is refused, and every domain word here is a
/// candidate to become declared. A token with no referent in the design set or
/// the codebase cannot be declared out from under the test, because there is
/// nothing for anyone to declare — this one is spelled `not_a_subject_kind`
/// specifically so it reads as "this is not a kind" rather than as a plausible
/// next feature to build.
///
/// **This is the copy the fast tier could not reach.** Two siblings carry the same
/// premise, on two different triggers rather than one:
/// `approval_repo_tests::a_subject_kind_outside_d158s_enumeration_is_a_corrupt_row`
/// reddened the moment `AuditSubjectKind::Membership` was declared — it reads the
/// Rust enumeration directly, with no database in the loop — while
/// `sqlite_approval_repo::a_subject_kind_outside_the_enumeration_is_refused_by_the_mirror`
/// stayed green through that change (this CHECK still correctly refused
/// `membership` then) and reddened only once `chk_pricing_approval_subject_kind`
/// widened it to match the declaration, days later. This one is `#[ignore]`d behind Docker, so
/// it would have stayed green through both rounds and failed only on the next
/// Postgres run, had its own guard not caught the same fact ahead of a live run.
/// Three copies of one premise, on two tiers, tripped by two different events.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_subject_kind_with_no_writer_is_refused() {
    let conn = applied().await;
    assert!(
        !AuditSubjectKind::ALL
            .iter()
            .any(|kind| kind.as_str() == "not_a_subject_kind"),
        "this test is about a token the gear does not declare"
    );
    must_be_rejected(
        &conn,
        &insert(
            &id_at(0x05),
            "submitted",
            "NULL",
            "NULL",
            "NULL",
            "not_a_subject_kind",
        ),
        "chk_pricing_approval_subject_kind",
    )
    .await;
}

/// Pending and undecided are one fact, and the biconditional refuses both ways.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_submitted_row_carrying_a_decided_at_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            &id_at(0x06),
            "submitted",
            "NULL",
            "NULL",
            DECIDED_AT,
            "plan_revision",
        ),
        "chk_pricing_approval_decided_at",
    )
    .await;
}

/// The other direction, which a one-sided test would miss: a decided row with no
/// `decided_at` is just as impossible, and `state <> 'submitted' => decided_at
/// IS NOT NULL` is the half that a `CHECK (decided_at IS NULL OR state <>
/// 'submitted')` would have dropped.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_decided_row_carrying_no_decided_at_is_refused() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
                SET state = 'approved', approver_principal = '{APPROVER}'
              WHERE approval_id = '{PENDING}'"
        ),
        "chk_pricing_approval_decided_at",
    )
    .await;
}

/// `inst-as-reject`'s mandatory reason, at the storage layer.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_rejected_row_without_a_reason_is_refused() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
                SET state = 'rejected', approver_principal = '{APPROVER}',
                    decided_at = {DECIDED_AT}
              WHERE approval_id = '{PENDING}'"
        ),
        "chk_pricing_approval_reason",
    )
    .await;
}

/// A decision names who made it. The `voided` exemption is proved by the valid
/// row above, so this constraint is pinned from both sides.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_decided_row_naming_no_approver_is_refused() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
                SET state = 'rejected', reason = 'margin below floor',
                    decided_at = {DECIDED_AT}
              WHERE approval_id = '{PENDING}'"
        ),
        "chk_pricing_approval_approver",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The append-only trigger
// ---------------------------------------------------------------------------

/// The sanctioned flip, first. Without it the three refusals below would pass
/// against a trigger that refuses every UPDATE.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_pending_record_takes_its_one_decision() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
             SET state = 'approved', approver_principal = '{APPROVER}',
                 decided_at = '2026-08-03 10:00:00+00'
             WHERE approval_id = '{PENDING}'"
        ),
    )
    .await;
}

/// `inst-as-immutable`, executed.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_decided_row_cannot_be_updated() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
             SET state = 'approved', approver_principal = '{APPROVER}',
                 decided_at = '2026-08-03 10:00:00+00'
             WHERE approval_id = '{PENDING}'"
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval SET reason = 'on reflection'
             WHERE approval_id = '{PENDING}'"
        ),
        "a decided record is immutable",
    )
    .await;
}

/// The pin. `content_hash` **is** the TOCTOU guard, and a decision that could
/// re-pin it in place would launder the mutation the guard exists to catch.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_pending_records_pinned_columns_cannot_move() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval SET content_hash = '\\xcafebabe'
             WHERE approval_id = '{PENDING}'"
        ),
        "only the decision columns may move",
    )
    .await;
    // And the other seven, so the whitelist is a whitelist rather than one
    // column somebody remembered.
    for column in [
        "subject_ref = 'other/1'",
        "subject_kind = 'price_unit'",
        "submitter_principal = '77777777-7777-7777-7777-777777777777'",
        "materiality = '{\"a\":1}'::jsonb",
        "submitted_at = '2026-08-03 08:00:00+00'",
        "tenant_id = '88888888-8888-8888-8888-888888888888'",
        "approval_id = '99999999-9999-9999-9999-999999999999'",
    ] {
        must_be_rejected(
            &conn,
            &format!("UPDATE bss.pricing_approval SET {column} WHERE approval_id = '{PENDING}'"),
            "only the decision columns may move",
        )
        .await;
    }
}

/// The flip whitelist: `submitted -> submitted` is not a move the machine has.
///
/// The `CHECK` cannot catch this one — the resulting row is a perfectly valid
/// pending record — so the trigger is the only thing standing between a
/// re-submit and one approval unit being re-used in place.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_pending_record_cannot_stay_pending_through_an_update() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval SET state = 'submitted'
             WHERE approval_id = '{PENDING}'"
        ),
        "is not a sanctioned flip",
    )
    .await;
}

/// No `REVOKE` is issued anywhere in this chain, deliberately: it names a
/// deployment role the migration does not own and `SQLite` has no `GRANT` at
/// all. The trigger is the portable half, and it is the half that has to work.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_approval_row_cannot_be_deleted() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_approval WHERE approval_id = '{PENDING}'"),
        "is not permitted",
    )
    .await;

    // A decided one either: the DELETE branch runs before the immutability
    // branch, so a test on a pending row alone would leave the decided case
    // resting on the reading of a `RETURN` this suite never executed.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
             SET state = 'voided', decided_at = '2026-08-03 10:00:00+00'
             WHERE approval_id = '{PENDING}'"
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_approval WHERE approval_id = '{PENDING}'"),
        "is not permitted",
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_approval_key` — the register's PL/pgSQL branches, executed
// ---------------------------------------------------------------------------

/// One canonical scope key, as the register renders it.
const REGISTER_KEY: &str = "3f2a|EUR|eu|base|9c1|all_subscriptions|recurring|none";

/// Insert one register row holding `key` for `unit`, in `state`.
fn hold(unit: &str, key: &str, state: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_approval_key (approval_id, tenant_id, scope_key, state)
         VALUES ('{unit}', '{TENANT}', '{key}', '{state}')"
    )
}

/// **The ad-hoc `UPDATE` the migration's doc said was impossible, on the canonical
/// backend.**
///
/// The mirror's version is `sqlite_approval_append_only.rs`'s, and it is not a
/// substitute: this branch is a **subquery inside a PL/pgSQL trigger function**, and a
/// function may reference a table or column that does not exist and still be created —
/// failing only when it fires. So the clause that reads the parent's state is
/// executable-or-not on Postgres and nowhere else.
///
/// `UPDATE bss.pricing_approval_key SET state = 'voided' WHERE scope_key = '<key>'`
/// leaves the three pinned columns untouched and satisfies `OLD.state = 'submitted'`, so
/// the three arms that stood before this one all pass it. Before the direction whitelist
/// it landed, and `uq_pricing_approval_key_pending` then admitted a second holder — two
/// units over one key with the first still approvable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_ad_hoc_update_cannot_free_a_held_key() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_succeed(&conn, &hold(PENDING, REGISTER_KEY, "submitted")).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval_key SET state = 'voided'
              WHERE scope_key = '{REGISTER_KEY}'"
        ),
        "a register row follows its unit",
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            &format!(
                "SELECT state AS v FROM bss.pricing_approval_key
                  WHERE scope_key = '{REGISTER_KEY}'"
            )
        )
        .await,
        "submitted",
        "the key is still held"
    );
}

/// And the parent's own transition **does** move it, which is what keeps the guard
/// above from being a table nobody can write to.
///
/// The load-bearing half on this backend for a reason the mirror does not share: the
/// follow trigger is `AFTER UPDATE` on the parent and the child's guard reads the
/// parent's state in a subquery, so the two are only compatible if the AFTER trigger's
/// query sees the row the statement just wrote. That is Postgres semantics rather than
/// anything this chain controls, and it is asserted rather than assumed.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_parents_decision_carries_the_register_with_it() {
    let conn = applied().await;
    seed_pending(&conn).await;
    must_succeed(&conn, &hold(PENDING, REGISTER_KEY, "submitted")).await;

    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
                SET state = 'approved', approver_principal = '{APPROVER}',
                    decided_at = '2026-08-03 10:00:00+00'
              WHERE approval_id = '{PENDING}'"
        ),
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            &format!(
                "SELECT state AS v FROM bss.pricing_approval_key
                  WHERE scope_key = '{REGISTER_KEY}'"
            )
        )
        .await,
        "approved",
        "the register followed its unit out of `submitted`, so the key is free"
    );
}

/// A register row is born under a **pending** unit — the missing foreign key and the
/// parent-state check, in the branch that can only be executed here.
///
/// Both births are staged because they are two mistakes with one remedy: a unit that
/// does not exist, and one that exists and is decided. Either leaves a hold nothing can
/// ever release — `follow_state` fires only `AFTER UPDATE`, and the parent refuses every
/// UPDATE once decided.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_register_row_is_born_under_a_pending_unit() {
    let conn = applied().await;

    must_be_rejected(
        &conn,
        &hold(&id_at(0x51), REGISTER_KEY, "submitted"),
        "is not a pending unit",
    )
    .await;

    seed_pending(&conn).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval
                SET state = 'approved', approver_principal = '{APPROVER}',
                    decided_at = '2026-08-03 10:00:00+00'
              WHERE approval_id = '{PENDING}'"
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &hold(PENDING, REGISTER_KEY, "submitted"),
        "is not a pending unit",
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            "SELECT CAST(count(*) AS TEXT) AS v FROM bss.pricing_approval_key"
        )
        .await,
        "0",
        "no phantom hold landed"
    );
}

/// **`chk_pricing_approval_key_state` is shadowed on both verbs, and this is the
/// case that says which guard answers instead.**
///
/// The register's four-name vocabulary fails **open** — it refuses a value, so
/// dropping it breaks nothing and admits a state
/// `uq_pricing_approval_key_pending` does not recognise, which is a key held by a
/// row no decision path can ever free. But the `CHECK` cannot be reached by any
/// statement while the append-only function is live:
///
/// * at `INSERT`, the born-submitted arm is strictly narrower — every state but
///   `submitted` is refused before a `CHECK` is evaluated at all, and a bogus one
///   is just another of them;
/// * at `UPDATE`, the direction whitelist makes the destination the **parent's
///   current state**, and the parent's own `chk_pricing_approval_state` carries
///   the identical four names — so a bogus destination is unreachable unless the
///   parent already holds it, which that constraint forbids.
///
/// That is `a_state_outside_the_machine_is_refused`'s situation one table over,
/// and it takes the same remedy: the guard is switched off for one statement, so
/// the claim that the `CHECK` is defence in depth behind the trigger is
/// *executed* rather than asserted. Both shadows are measured first, because a
/// case that only disabled the trigger would not record which rule is actually
/// load-bearing today.
///
/// The trigger is put back and its return proved, so nothing after this line
/// runs against a register whose guard this test switched off.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_register_state_outside_the_four_is_refused() {
    let conn = applied().await;
    seed_pending(&conn).await;

    // The shadow at INSERT: the born-submitted arm, not the vocabulary.
    must_be_rejected(
        &conn,
        &hold(PENDING, REGISTER_KEY, "archived"),
        "is born submitted with its unit",
    )
    .await;

    // And the shadow at UPDATE: the direction whitelist, whose destination is a
    // state the parent's own CHECK bounds to the same four.
    must_succeed(&conn, &hold(PENDING, REGISTER_KEY, "submitted")).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_approval_key SET state = 'archived'
              WHERE scope_key = '{REGISTER_KEY}'"
        ),
        "a register row follows its unit",
    )
    .await;

    // With the function off, the row faces the vocabulary and nothing else.
    must_succeed(
        &conn,
        "ALTER TABLE bss.pricing_approval_key DISABLE TRIGGER trg_pricing_approval_key_append_only",
    )
    .await;
    must_be_rejected(
        &conn,
        &hold(&id_at(0x52), REGISTER_KEY, "archived"),
        "chk_pricing_approval_key_state",
    )
    .await;
    // The four that are declared do land, so the constraint is pinned from both
    // sides: a vocabulary narrowed to `submitted` alone would leave every
    // decided unit's register row unwritable.
    for (n, state) in ["submitted", "approved", "rejected", "voided"]
        .into_iter()
        .enumerate()
    {
        must_succeed(
            &conn,
            &hold(&id_at(0x52), &format!("{REGISTER_KEY}|{n}"), state),
        )
        .await;
    }

    must_succeed(
        &conn,
        "ALTER TABLE bss.pricing_approval_key ENABLE TRIGGER trg_pricing_approval_key_append_only",
    )
    .await;
    // Read out of the catalog rather than by executing a branch, for
    // `a_state_outside_the_machine_is_refused`'s reason: any statement proving
    // the trigger is back would tie this test to whichever arm answered it.
    assert_eq!(
        scalar(
            &conn,
            "SELECT tgenabled::text AS v FROM pg_trigger
              WHERE tgname = 'trg_pricing_approval_key_append_only'",
        )
        .await,
        "O",
        "the guard this test switched off must be back on"
    );
}
