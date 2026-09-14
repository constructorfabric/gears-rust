//! The seven standalone stores of `pricing_read_model`…`…000009`, proved by
//! **executing the statement each object must refuse**, on Postgres.
//!
//! `pricing_read_model`, `pricing_catalog_version_ref`, `pricing_pin_frontier`,
//! `pricing_policy_object`, `pricing_operator_flag`,
//! `pricing_idempotency_dedup` and `pricing_outbox`.
//!
//! # Why this suite exists
//!
//! None of these seven migrations had ever run on the backend they target: the
//! `SQLite` mirror was what every test reached, and a repository is what reached
//! it. A repository writes only legal values, so a test that goes through one
//! catches a constraint that got *narrower* — the writer starts failing — and
//! never one that stopped refusing. The Phase-2 review of `pricing_price` found
//! fourteen constraints that could each be replaced with `CHECK (1 = 1)` with the
//! whole crate green; nothing distinguishes these tables from that one except
//! that nobody had looked.
//!
//! `tests/postgres_migrations.rs` closed half the gap by pinning the CHECK and
//! partial-index rosters **by name**, so an object cannot vanish unnoticed. It
//! issues no DML, so it says the objects reached the server and nothing about
//! what any of them does. This suite is the other half: one executed refusal per
//! object, and the assertion names the object the refusal came from.
//!
//! # The three rules every test here follows
//!
//! **Execute the refusal.** A test that writes valid values is not evidence
//! about a guard.
//!
//! **Put the world in the state where the object under test is what answers.**
//! Four of these tables carry constraints that overlap on the same illegal
//! value, and each of them was hit while writing this file:
//!
//! * `chk_pricing_catalog_version_ref_version` is only reachable on a row that
//!   already satisfies `chk_pricing_catalog_version_ref_commit`, so the negative
//!   version has to arrive **with** a `committed_at`; a bare
//!   `catalog_version = -1` is refused by the co-nullability constraint and the
//!   test would be green while proving nothing.
//! * `chk_pricing_approval_threshold_absolute_non_negative` needs its own basis
//!   left as the only one set, or `chk_pricing_approval_threshold_basis` answers
//!   first. (It replaced a pair on `pricing_policy_object` with the same overlap;
//!   `pricing_approval_threshold` holds the threshold, and `pricing_policy_object` carries neither column — its own
//!   versioned, per-currency table.)
//! * `chk_pricing_idempotency_dedup_status` needs a `response_body`, or
//!   `chk_pricing_idempotency_dedup_answered` answers.
//! * `uq_pricing_outbox_sequence` and `uq_pricing_outbox_dedup_key` cover the
//!   same table and would each refuse a naive duplicate row; every collision
//!   below therefore moves exactly one of the two keys and leaves the other
//!   distinct.
//!
//! **Assert the object, never the table.** Every CHECK and index over these
//! tables carries the table name, as does the constraint name Postgres prints
//! for a unique violation. A test that accepted any error naming the table would
//! pass with the guard it means to prove switched off.
//!
//! # There are no triggers here, and that is a decision each migration argues
//!
//! Not one of these seven tables carries an append-only trigger, and the reasons
//! differ table by table rather than being an omission: `pricing_read_model` is
//! a **projection** and the projector rebuilds its rows on a degraded-publish
//! re-drive, with the frozen-content guarantee enforced upstream on the truth
//! tables; `pricing_pin_frontier` is a **watermark** and is meant to be updated,
//! its one forbidden direction — backwards — being the repository's guarded
//! `WHERE catalog_version < :to`, which a whitelist trigger could not express
//! better; `pricing_operator_flag` clears a flag by **deleting the row**, because
//! a `cleared` tombstone would make "is this subject divergent" a question about
//! the newest of several rows. So this suite has no trigger arm to execute, and
//! the absence is recorded here rather than read as a gap.
//!
//! # Positives are load-bearing
//!
//! Two of them carry more weight than the rest and are worth naming:
//!
//! * **Two refs of one tenant may resolve to one `catalog_version`.** The table
//!   used to carry `uq_pricing_catalog_version_ref_version`, and under D-47
//!   batching that index refused the normal case — the registry batches approved
//!   publishes, so a version legitimately bundles several. The index is gone and
//!   its replacement is non-unique; this test is what would redden if somebody
//!   restored the bijection.
//! * **Every optional column of `pricing_policy_object` may be absent.** Every
//!   default there is the fail-safe one and the nullability *is* the encoding: an
//!   absent rounding policy means every published row must carry its own, and an
//!   absent cap means the ratified launch value. A `NOT NULL` slipped onto any of
//!   them would change a fail-safe default into a required configuration. The
//!   threshold is no longer among them — an absent **entry** is now an absent row
//!   in `pricing_approval_threshold`, which is where `inst-mat-percurrency`'s
//!   per-currency fail-safe reads it.
//!
//! # Objects this suite deliberately does not test by refusal
//!
//! `idx_pricing_read_model_resolve`, `idx_pricing_catalog_version_ref_version`,
//! `idx_pricing_operator_flag_by_flag`, `idx_pricing_idempotency_dedup_created`
//! and `idx_pricing_outbox_undrained` are **non-unique** indexes, the last of
//! them partial. A non-unique index refuses nothing; its only observable effect
//! is on plan choice, which is not a correctness property and would make a
//! brittle test. Their presence is pinned by name in
//! `tests/postgres_migrations.rs`, and that is the whole of what can be said
//! about them here.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_stores -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const TENANT_B: &str = "11111111-1111-1111-1111-1111111111b0";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";
const PLAN: &str = "22222222-0000-0000-0000-00000000000a";
const PLAN_B: &str = "22222222-0000-0000-0000-00000000000b";
// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A fresh database carrying the applied chain, on the one shared server.
///
/// **One** container for the whole binary and a `CREATE DATABASE` per test; the
/// arrangement and the eleven false positives that motivated it are documented
/// in `tests/pg_support/mod.rs`.
///
/// The connection handed back is a **plain** one: every statement this suite
/// issues is raw SQL that deliberately reaches past every repository, because
/// the repository is exactly the layer that cannot see a guard stop refusing.
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
async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

/// Reject, **and by the named object**.
///
/// See the module doc: the fragment is the whole assertion, because every guard
/// over these tables names the table too.
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

// ---------------------------------------------------------------------------
// Row rendering
// ---------------------------------------------------------------------------

fn render(table: &str, base: &[(&str, String)], overrides: &[(&str, &str)]) -> String {
    let mut columns: Vec<(String, String)> = base
        .iter()
        .map(|(column, value)| ((*column).to_owned(), value.clone()))
        .collect();
    for (name, value) in overrides {
        match columns.iter_mut().find(|(column, _)| column == name) {
            Some(slot) => (*value).clone_into(&mut slot.1),
            None => columns.push(((*name).to_owned(), (*value).to_owned())),
        }
    }
    let names = columns
        .iter()
        .map(|(column, _)| column.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO bss.{table} ({names}) VALUES ({values})")
}

// ===========================================================================
// `pricing_read_model` — pricing_read_model
// ===========================================================================

/// A minimal **valid** unwarmed delta row for one plan subject at version 1.
fn read_model(overrides: &[(&str, &str)]) -> String {
    render(
        "pricing_read_model",
        &[
            ("tenant_id", format!("'{TENANT}'")),
            ("catalog_version", "1".to_owned()),
            ("subject_kind", "'plan'".to_owned()),
            ("subject_ref", format!("'{PLAN}'")),
            ("payload", "'{}'".to_owned()),
        ],
        overrides,
    )
}

/// The valid rows, first. Without this every refusal below would pass against a
/// table that refuses everything.
///
/// One per subject kind, because `chk_pricing_read_model_subject_kind` admits
/// four and a suite that only projected plans would leave three quarters of the
/// admitted set unexercised — and the `overlay_index` token in particular is
/// D-112/D-133's shard, which no code in this repository writes yet.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_projected_subject_kind_is_storable() {
    let conn = applied().await;
    for kind in [
        "'plan'",
        "'price_overlay'",
        "'overlay_index'",
        "'group_membership'",
    ] {
        must_succeed(
            &conn,
            &read_model(&[("subject_kind", kind), ("subject_ref", kind)]),
        )
        .await;
    }
    // Version zero is the first catalog version a tenant has, not a missing one.
    must_succeed(&conn, &read_model(&[("catalog_version", "0")])).await;
    // Both consistent readings of the per-row warm marker (D-86/D-91).
    must_succeed(
        &conn,
        &read_model(&[
            ("catalog_version", "2"),
            ("warm_completed", "true"),
            ("warm_completed_at", "'2026-08-03 09:00:00+00'"),
        ]),
    )
    .await;
    must_succeed(&conn, &read_model(&[("catalog_version", "3")])).await;
}

/// The four tokens `domain::read_model::SubjectKind::as_str` renders.
///
/// `pricing_catalog_version_ref` carries the same four deliberately — the ref
/// names the subject the projector will write, so two vocabularies would be two
/// answers to one question.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_projected_subject_kind_outside_the_four_is_refused() {
    let conn = applied().await;
    for kind in ["'bundle'", "'PLAN'", "'price'"] {
        must_be_rejected(
            &conn,
            &read_model(&[("subject_kind", kind)]),
            "chk_pricing_read_model_subject_kind",
        )
        .await;
    }
}

/// Catalog versions count up from zero; a negative one names no publish.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_catalog_version_is_refused_by_the_read_model() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &read_model(&[("catalog_version", "-1")]),
        "chk_pricing_read_model_catalog_version",
    )
    .await;
}

/// The completion marker and its timestamp are one fact, refused in **both**
/// directions.
///
/// A row that is warm-complete with no record of when it completed cannot be
/// audited; a row carrying a completion instant while the marker says otherwise
/// is a projection that resolution will skip forever with a timestamp claiming
/// it finished. One-sided constraints are how this class rots — a bare
/// `warm_completed = false OR warm_completed_at IS NOT NULL` would refuse the
/// first case and admit the second.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_warm_marker_that_disagrees_with_its_timestamp_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &read_model(&[("warm_completed", "true")]),
        "chk_pricing_read_model_warm_marker",
    )
    .await;
    must_be_rejected(
        &conn,
        &read_model(&[
            ("warm_completed", "false"),
            ("warm_completed_at", "'2026-08-03 09:00:00+00'"),
        ]),
        "chk_pricing_read_model_warm_marker",
    )
    .await;
}

/// One subject has one delta row per version, and the key says which.
///
/// The accepting cases are the point of the composite: the **same** subject at
/// another version is another row — that is the whole of "per-subject deltas",
/// D-86/D-91 — and two subjects of one version are two rows.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_subject_has_one_delta_row_per_catalog_version() {
    let conn = applied().await;
    must_succeed(&conn, &read_model(&[])).await;
    must_be_rejected(
        &conn,
        &read_model(&[("payload", "'{\"a\":1}'")]),
        "pricing_read_model_pkey",
    )
    .await;
    must_succeed(&conn, &read_model(&[("catalog_version", "2")])).await;
    must_succeed(
        &conn,
        &read_model(&[("subject_ref", &format!("'{PLAN_B}'"))]),
    )
    .await;
    must_succeed(
        &conn,
        &read_model(&[("tenant_id", &format!("'{TENANT_B}'"))]),
    )
    .await;
}

/// **A hole two migrations in this chain describe differently, pinned so that
/// closing it is a decision rather than an accident.**
///
/// Nothing on this table refuses an UPDATE or a DELETE — deliberately, per
/// `pricing_read_model`'s migration doc: the table is a projection and the projector
/// rebuilds a row on a degraded-publish re-drive. But `pricing_catalog_version_ref`'s migration doc
/// argues for `subject_revision` on the ground that a wrong value's damage "is
/// permanent: a delta row is **INSERT-only** on the seven-year truth horizon, in
/// a store whose entire contract is that a completed version never changes".
///
/// Both readings cannot be physically true at once, and the schema holds the
/// first: a **warm-completed** row of a completed version is freely mutable and
/// freely deletable, by anything holding a connection. This is reported as a
/// divergence rather than closed here — a trigger would forbid the re-drive the
/// projector is built around, and which reading is intended is not a schema
/// suite's call to make.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_warm_completed_delta_row_is_physically_mutable_and_deletable() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &read_model(&[
            ("warm_completed", "true"),
            ("warm_completed_at", "'2026-08-03 09:00:00+00'"),
        ]),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_read_model SET payload = '{{\"rewritten\": true}}' \
             WHERE tenant_id = '{TENANT}' AND catalog_version = 1"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_read_model \
             WHERE tenant_id = '{TENANT}' AND catalog_version = 1"
        ),
    )
    .await;
}

// ===========================================================================
// `pricing_catalog_version_ref` — pricing_catalog_version_ref
// ===========================================================================

/// A minimal **valid** pending ref: a handle with no version yet, which is the
/// state D-47 batching leaves every publish in until the registry answers.
fn version_ref(pending: &str, overrides: &[(&str, &str)]) -> String {
    render(
        "pricing_catalog_version_ref",
        &[
            ("tenant_id", format!("'{TENANT}'")),
            ("pending_ref", format!("'{pending}'")),
            ("subject_kind", "'plan'".to_owned()),
            ("subject_ref", format!("'{PLAN}'")),
        ],
        overrides,
    )
}

/// The pending state, the finalized state, and the revision the publish judged.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_ref_stores_pending_and_finalized() {
    let conn = applied().await;
    must_succeed(&conn, &version_ref("pending-1", &[])).await;
    must_succeed(
        &conn,
        &version_ref(
            "pending-2",
            &[
                ("catalog_version", "7"),
                ("committed_at", "'2026-08-03 09:00:00+00'"),
                ("commit_observed_at", "'2026-08-03 08:59:00+00'"),
                ("subject_revision", "0"),
                ("subject_lifecycle_state", "'published'"),
            ],
        ),
    )
    .await;
    // `retired` is the other state D-128 sanctions: retirement is a publish unit
    // of its own that re-projects the plan subject under its own version.
    must_succeed(
        &conn,
        &version_ref(
            "pending-3",
            &[
                ("catalog_version", "8"),
                ("committed_at", "'2026-08-03 09:00:00+00'"),
                ("subject_revision", "1"),
                ("subject_lifecycle_state", "'retired'"),
            ],
        ),
    )
    .await;
    // `commit_observed_at` is deliberately settable while the version is still
    // NULL — that is the state D-166 added it to describe, and no CHECK pairs it
    // with anything.
    must_succeed(
        &conn,
        &version_ref(
            "pending-4",
            &[("commit_observed_at", "'2026-08-03 08:59:00+00'")],
        ),
    )
    .await;
}

/// **The index that used to be here refused the normal case.**
///
/// `uq_pricing_catalog_version_ref_version` asserted a per-tenant bijection
/// between pending handles and committed versions. §4.2 step 5 and §3.6 say the
/// registry is the sole incrementer and **batches** approved publishes, so two
/// publishes of one tenant landing in one batch is exactly what D-47's model
/// exists to serve — and the unique index made the second finalize fail.
///
/// This test is what reddens if somebody restores it, and it is the reason the
/// replacement index is non-unique.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_publishes_of_one_tenant_may_batch_onto_one_catalog_version() {
    let conn = applied().await;
    for pending in ["pending-1", "pending-2"] {
        must_succeed(
            &conn,
            &version_ref(
                pending,
                &[
                    ("subject_ref", &format!("'{pending}'")),
                    ("catalog_version", "7"),
                    ("committed_at", "'2026-08-03 09:00:00+00'"),
                ],
            ),
        )
        .await;
    }
}

/// The same four tokens `pricing_read_model` carries, and for the same reason.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_ref_subject_kind_outside_the_four_is_refused() {
    let conn = applied().await;
    for kind in ["'bundle'", "'plan_revision'"] {
        must_be_rejected(
            &conn,
            &version_ref("pending-1", &[("subject_kind", kind)]),
            "chk_pricing_catalog_version_ref_subject_kind",
        )
        .await;
    }
}

/// The commit is atomic in the row: the version and its instant are set together
/// or not at all, refused in **both** directions.
///
/// This is what stops a half-finalize. A row claiming a version with no record
/// of when it was assigned cannot be audited, and a row carrying a commit
/// instant with no version is a finalize that lost its answer.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_half_finalized_ref_is_refused_in_both_directions() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &version_ref("pending-1", &[("catalog_version", "7")]),
        "chk_pricing_catalog_version_ref_commit",
    )
    .await;
    must_be_rejected(
        &conn,
        &version_ref("pending-1", &[("committed_at", "'2026-08-03 09:00:00+00'")]),
        "chk_pricing_catalog_version_ref_commit",
    )
    .await;
}

/// A committed version counts up from zero.
///
/// The row carries a `committed_at`, and it has to: without it the
/// co-nullability constraint above answers first and this test would be green
/// while saying nothing about the constraint it names.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_committed_version_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &version_ref(
            "pending-1",
            &[
                ("catalog_version", "-1"),
                ("committed_at", "'2026-08-03 09:00:00+00'"),
            ],
        ),
        "chk_pricing_catalog_version_ref_version",
    )
    .await;
    // Zero is a real version.
    must_succeed(
        &conn,
        &version_ref(
            "pending-1",
            &[
                ("catalog_version", "0"),
                ("committed_at", "'2026-08-03 09:00:00+00'"),
            ],
        ),
    )
    .await;
}

/// Revision numbers count up from zero (D-145), and the ref pins the one its
/// publish judged.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_subject_revision_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &version_ref("pending-1", &[("subject_revision", "-1")]),
        "chk_pricing_catalog_version_ref_subject_revision",
    )
    .await;
    must_succeed(
        &conn,
        &version_ref("pending-1", &[("subject_revision", "0")]),
    )
    .await;
}

/// **Two tokens, two different defects**, and the loop below runs over three
/// values because they are not one rule with one reason.
///
/// **`superseded`** is the reachable one and is what the constraint was written
/// for. Reading the pinned revision's state as it *now* stands lets `superseded`
/// into a delta — a value `plan_repo::load_current` could never return and which
/// D-128 does not contemplate for a projected subject, whose clause names
/// `published` **or** `retired`. A consumer coding D-90's sellability predicate
/// as "is published" then reads the version as unsellable, permanently, on an
/// INSERT-only row.
///
/// **`draft` is kept out for a second reason, and this doc used to imply it rode
/// along (D-314, 2026-08-15).** `superseded` is a state a *frozen* revision drifts
/// into after its pin; a draft revision row is still **mutable**, and the
/// projector reads a pinned revision's content live off the truth row up to
/// D-47's batching maximum after the commit — a licence that rests on the row and
/// its revision-scoped children being physically immutable. So a draft-pinned ref
/// would freeze un-judged, still-moving content into an INSERT-only delta. This
/// clause is also the second of D-314's two grounds for the window surface's 404
/// on a plan that has never published, and its fast-tier twin is
/// `sqlite_window_service.rs::a_window_needs_a_frozen_revision_and_the_store_says_so_too`:
/// widening it here would leave that case's *surface* assertion green.
/// `abandoned` is the terminal draft (D-145) and is out for the same reason.
///
/// NULL is admitted because only a revisioned subject kind has a state at all.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_subject_lifecycle_state_outside_the_two_d128_sanctions_is_refused() {
    let conn = applied().await;
    for state in ["'superseded'", "'draft'", "'abandoned'"] {
        must_be_rejected(
            &conn,
            &version_ref("pending-1", &[("subject_lifecycle_state", state)]),
            "chk_pricing_catalog_version_ref_subject_lifecycle",
        )
        .await;
    }
    // And the shape it admits: absent, for a subject kind that has no revision.
    must_succeed(&conn, &version_ref("pending-1", &[])).await;
}

/// A pending handle is one row per tenant **and subject** (D-234).
///
/// **This test's premise changed with `pricing_catalog_version_ref`, and it is the surface
/// that caught the change.** It read "one row per tenant" and proved it by
/// re-inserting one handle under a different `subject_ref`; that is now the
/// *admitted* case, because a publish unit records against one handle every
/// subject it projects — one on the plan plane, two on the overlay plane and
/// three when a revision moves the scope value (D-112, D-133).
///
/// Worth stating twice over, because both halves are load-bearing:
///
/// * the key still refuses a **duplicate subject** on one handle, which is the
///   whole of what `record_pending`'s contract wanted — a handle arriving twice
///   for one subject means two publish transactions believe they own one
///   registry assignment;
/// * the sibling row is not a second publish, it is the same act's other
///   subject, and admitting it is what the overlay publish pipeline needs.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_pending_ref_is_one_row_per_tenant_and_subject() {
    let conn = applied().await;
    must_succeed(&conn, &version_ref("pending-1", &[])).await;
    // The same handle's other subject: the sibling an overlay publish records.
    must_succeed(
        &conn,
        &version_ref(
            "pending-1",
            &[
                ("subject_kind", "'overlay_index'"),
                ("subject_ref", "'global/global'"),
            ],
        ),
    )
    .await;
    // A second subject of the same **kind** is a sibling too - two shards move
    // when a revision moves the scope value.
    must_succeed(
        &conn,
        &version_ref(
            "pending-1",
            &[
                ("subject_kind", "'overlay_index'"),
                ("subject_ref", "'region/eu-west'"),
            ],
        ),
    )
    .await;
    // But the same handle and the same subject, twice, is still refused.
    must_be_rejected(
        &conn,
        &version_ref("pending-1", &[]),
        "pricing_catalog_version_ref_pkey",
    )
    .await;
    // A different subject of the kind the first row used is likewise a sibling,
    // not a duplicate - the refusal above is about the pair, not the kind.
    must_succeed(
        &conn,
        &version_ref("pending-1", &[("subject_ref", &format!("'{PLAN_B}'"))]),
    )
    .await;
    // The same handle spelling under another tenant is another publish.
    must_succeed(
        &conn,
        &version_ref("pending-1", &[("tenant_id", &format!("'{TENANT_B}'"))]),
    )
    .await;
}

// ===========================================================================
// `pricing_pin_frontier` — pricing_pin_frontier
// ===========================================================================

fn frontier(tenant: &str, version: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_pin_frontier (tenant_id, catalog_version, advanced_at) \
         VALUES ('{tenant}', {version}, '2026-08-03 09:00:00+00')"
    )
}

/// The watermark counts up from zero.
///
/// A negative frontier is a watermark below every version there is, which makes
/// every pin ineligible and `pricing.readmodel.pin_eligibility_overdue` fire
/// against a store that is behaving.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_pin_frontier_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &frontier(TENANT, "-1"),
        "chk_pricing_pin_frontier_version",
    )
    .await;
    // Zero is where a tenant starts, not a missing frontier.
    must_succeed(&conn, &frontier(TENANT, "0")).await;
}

/// One frontier per tenant, and it is **meant** to move forward.
///
/// The forward move is the load-bearing half: there is deliberately no
/// append-only trigger on this table, so a suite that only proved the refusals
/// would be consistent with a watermark that can never advance — which is the
/// same store, permanently stuck, with every pin ineligible.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_tenant_has_exactly_one_frontier_row_and_it_advances() {
    let conn = applied().await;
    must_succeed(&conn, &frontier(TENANT, "3")).await;
    must_be_rejected(&conn, &frontier(TENANT, "4"), "pricing_pin_frontier_pkey").await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_pin_frontier SET catalog_version = 4 \
             WHERE tenant_id = '{TENANT}' AND catalog_version < 4"
        ),
    )
    .await;
    must_succeed(&conn, &frontier(TENANT_B, "0")).await;
}

/// **The one thing that must never happen to a watermark is not enforced here**,
/// and the migration says so in as many words.
///
/// A receding frontier lets one pin resolve two different contents over time,
/// which is the entire reason the frontier is materialized — and the schema takes
/// it without complaint. `chk_pricing_pin_frontier_version` is a **floor**, not a
/// direction: the forward-only rule lives in the repository's conditional
/// `WHERE catalog_version < :to`, which a whitelist trigger could not express any
/// better and which additionally reports a typed refusal so the ordering bug
/// behind it surfaces instead of being swallowed.
///
/// Pinned so that a reader who took the CHECK for the forward-only rule meets the
/// fact instead of the assumption. The **unguarded** statement is what shows the
/// table permits a recession; the **last** one is the repository's conditional
/// written out, and it moves nothing because the row stands above its target.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_frontier_physically_recedes_and_the_table_carries_no_guard_against_it() {
    let conn = applied().await;
    must_succeed(&conn, &frontier(TENANT, "9")).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_pin_frontier SET catalog_version = 1 \
             WHERE tenant_id = '{TENANT}'"
        ),
    )
    .await;
    // **A target the guard can actually be asked about.** A guarded
    // `SET catalog_version = 0 WHERE catalog_version < 0` costs the case its
    // subject: `chk_pricing_pin_frontier_version` floors the column at 0, so that
    // predicate matches nothing whatever the guard does, and the *floor* answers
    // in place of the monotonicity rule.
    //
    // The row stands at 1, so `advance`'s own predicate for a target of 3 -
    // "move only if the standing version is below the target" - is satisfiable in
    // principle and excludes this row on its merits. The repository call itself is
    // driven in `tests/sqlite_pin_frontier.rs`; what this suite owns is that the
    // **table** carries no constraint of its own against the move.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_pin_frontier SET catalog_version = 3 \
             WHERE tenant_id = '{TENANT}' AND catalog_version < 3"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_pin_frontier SET catalog_version = 2 \
             WHERE tenant_id = '{TENANT}' AND catalog_version < 2"
        ),
    )
    .await;

    let remaining = conn
        .query_one_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT catalog_version AS v FROM bss.pricing_pin_frontier \
                 WHERE tenant_id = '{TENANT}'"
            ),
        ))
        .await
        .expect("read the frontier")
        .expect("one row")
        .try_get::<i64>("", "v")
        .expect("read the version");
    assert_eq!(
        remaining, 3,
        "the guarded recession to 2 moved nothing - the row stands at 3, above the \
         target - while the unguarded statement above showed the table itself \
         carries no constraint against receding"
    );
}

// ===========================================================================
// `pricing_policy_object` — pricing_policy_object
// ===========================================================================

fn policy(tenant: &str, overrides: &[(&str, &str)]) -> String {
    render(
        "pricing_policy_object",
        &[
            ("tenant_id", format!("'{tenant}'")),
            ("updated_by", format!("'{ACTOR}'")),
        ],
        overrides,
    )
}

/// **Every optional column may be absent, and the absence is the fail-safe
/// default.**
///
/// `default_rounding_policy_ref` absent means
/// every published row must carry its own, not that a mode is picked quietly;
/// each of the four D-152 caps absent means the ratified launch value from the
/// deployment section. A `NOT NULL` slipped onto any of them would turn a
/// fail-safe default into a required configuration, and this row is what would
/// stop refusing to prove it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_policy_object_stores_with_every_optional_left_absent() {
    let conn = applied().await;
    must_succeed(&conn, &policy(TENANT, &[])).await;
    // And the fully configured one, so the constraints below are whitelists.
    must_succeed(
        &conn,
        &policy(
            TENANT_B,
            &[
                ("tax_display_policy_mode", "'warn'"),
                ("default_rounding_policy_ref", "'half_up/2'"),
                ("enforced_migration_notice_days", "90"),
                ("max_tier_bands_per_row", "100"),
                ("max_price_rows_per_plan", "500"),
                ("max_custom_interval_days", "366"),
                ("max_custom_interval_months", "24"),
                ("additional_required_descriptors", "'[\"costCentre\"]'"),
            ],
        ),
    )
    .await;
}

/// The two tokens C4's tax-display **policy** has.
///
/// D-240 retires `tax_display_mode`, and this case is about the switch that
/// survives it. The retired column is the one that had a dedicated refusal case,
/// while `tax_display_policy_mode` — the switch §6 actually declares — is pinned by
/// **name only** in the two migration-roster censuses. Retiring the case along with
/// the column would leave the surviving switch's token set unproven on either
/// engine.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_tax_display_policy_mode_outside_the_two_is_refused() {
    let conn = applied().await;
    for mode in ["'gross'", "'FAIL_CLOSED'", "'strict'"] {
        must_be_rejected(
            &conn,
            &policy(TENANT, &[("tax_display_policy_mode", mode)]),
            "chk_pricing_policy_object_tax_display_policy",
        )
        .await;
    }
}

/// D-240: the retired column is not a column any more.
///
/// Postgres drops the CHECK naming it along with it, so the refusal here is the
/// parser's rather than a constraint's — which is the point. A case asserting
/// only that the CHECK is gone would pass equally against a column left behind
/// unguarded.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_retired_tax_display_mode_is_no_longer_a_column() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &policy(TENANT, &[("tax_display_mode", "'tax_inclusive'")]),
        "tax_display_mode",
    )
    .await;
}

// `a_threshold_missing_half_of_itself_is_refused` and
// `a_negative_approval_threshold_is_refused` stood here and are **deleted, not
// moved or skipped.** Both executed a CHECK over `pricing_policy_object`'s
// `approval_threshold_minor` / `approval_threshold_currency` pair, and
// `pricing_approval_threshold` holds them instead, and neither column nor constraint is on this row: §6
// requires per-currency `{absolute_minor | percent}` entries and D-10 requires the
// policy to be versioned, neither of which a single column pair can carry.
//
// **Their successors are the two tests that prove the columns and their CHECKs are
// GONE**, which is the deleted pair's actual property:
// `tests/postgres_approval_threshold.rs`'s
// `the_policy_objects_old_threshold_pair_is_gone` and
// `tests/sqlite_approval_threshold.rs`'s
// `the_policy_objects_old_threshold_pair_is_gone_and_the_rebuild_kept_the_rest`, each
// of which asserts that a `SELECT` naming either column fails with an
// undefined-column error rather than answering NULL — the failure this shape exists
// to avoid being two places to read a threshold from, one of them unmaintained.
//
// This note used to name three *other* tests as the successors —
// `a_threshold_entry_with_neither_basis_is_refused`,
// `a_threshold_entry_with_both_bases_is_refused` and
// `a_negative_absolute_threshold_is_refused`. Those are worth having and are not
// successors to anything: they prove the **new** table's CHECKs, which no deleted
// test ever covered. A tombstone that names the wrong heir leaves the deleted
// property unaccounted for while reading as though it were covered. The new table's
// own guards are that suite's subject and are proved there.
//
// The deletion is written out because a deleted guard test is what a regression looks
// like when nobody says it was deliberate.

/// D-49's sixty-day floor, in the schema rather than only in Slice 11.
///
/// A floor that lives only in application code is one migration script away from
/// being bypassed, and what it protects is a customer's notice of an enforced
/// price migration.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_notice_period_below_the_sixty_day_floor_is_refused() {
    let conn = applied().await;
    for days in ["59", "0", "-1"] {
        must_be_rejected(
            &conn,
            &policy(TENANT, &[("enforced_migration_notice_days", days)]),
            "chk_pricing_policy_object_notice_floor",
        )
        .await;
    }
    // Exactly sixty is the floor and not one past it.
    must_succeed(
        &conn,
        &policy(TENANT, &[("enforced_migration_notice_days", "60")]),
    )
    .await;
}

/// A zero band cap makes every plan unpublishable, and a cap that rejects
/// everything looks exactly like a cap that is switched on.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_non_positive_tier_band_cap_is_refused() {
    let conn = applied().await;
    for cap in ["0", "-1"] {
        must_be_rejected(
            &conn,
            &policy(TENANT, &[("max_tier_bands_per_row", cap)]),
            "chk_pricing_policy_object_tier_band_cap",
        )
        .await;
    }
    must_succeed(&conn, &policy(TENANT, &[("max_tier_bands_per_row", "1")])).await;
}

/// The same rule one column over, and a separate constraint, so a separate test.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_non_positive_price_row_cap_is_refused() {
    let conn = applied().await;
    for cap in ["0", "-1"] {
        must_be_rejected(
            &conn,
            &policy(TENANT, &[("max_price_rows_per_plan", cap)]),
            "chk_pricing_policy_object_price_row_cap",
        )
        .await;
    }
    must_succeed(&conn, &policy(TENANT, &[("max_price_rows_per_plan", "1")])).await;
}

/// A zero interval cap makes every custom frequency unpublishable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_non_positive_custom_interval_days_cap_is_refused() {
    let conn = applied().await;
    for cap in ["0", "-1"] {
        must_be_rejected(
            &conn,
            &policy(TENANT, &[("max_custom_interval_days", cap)]),
            "chk_pricing_policy_object_interval_days_cap",
        )
        .await;
    }
    must_succeed(&conn, &policy(TENANT, &[("max_custom_interval_days", "1")])).await;
}

/// And the months cap, which is its own constraint and its own column.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_non_positive_custom_interval_months_cap_is_refused() {
    let conn = applied().await;
    for cap in ["0", "-1"] {
        must_be_rejected(
            &conn,
            &policy(TENANT, &[("max_custom_interval_months", cap)]),
            "chk_pricing_policy_object_interval_months_cap",
        )
        .await;
    }
    must_succeed(
        &conn,
        &policy(TENANT, &[("max_custom_interval_months", "1")]),
    )
    .await;
}

/// One tenant, one policy object.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_tenant_has_one_policy_object() {
    let conn = applied().await;
    must_succeed(&conn, &policy(TENANT, &[])).await;
    must_be_rejected(
        &conn,
        &policy(TENANT, &[("tax_display_policy_mode", "'warn'")]),
        "pricing_policy_object_pkey",
    )
    .await;
    must_succeed(&conn, &policy(TENANT_B, &[])).await;
}

// ===========================================================================
// `pricing_operator_flag` — pricing_operator_flag
// ===========================================================================

fn flag(tenant: &str, subject: &str, name: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_operator_flag (tenant_id, subject_ref, flag, set_by) \
         VALUES ('{tenant}', '{subject}', '{name}', '{ACTOR}')"
    )
}

/// The closed, slice-owned flag set.
///
/// A typo'd flag name would raise a divergence nothing ever clears: clearing is a
/// row DELETE keyed on the flag, so a name no clearing path knows is a permanent
/// operator-plane alarm.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_operator_flag_outside_the_four_is_refused() {
    let conn = applied().await;
    for name in ["tier_drift", "TIER_DIVERGENT", "price_divergent"] {
        must_be_rejected(
            &conn,
            &flag(TENANT, PLAN, name),
            "chk_pricing_operator_flag_name",
        )
        .await;
    }
}

/// One subject carries every flag at once, and each of them only once.
///
/// The accepting half is the point of putting `flag` in the key: a subject can be
/// tier-divergent and tax-divergent at the same time, and a key of
/// `(tenant_id, subject_ref)` alone would make the second raise overwrite or
/// refuse the first.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_subject_carries_every_flag_and_each_only_once() {
    let conn = applied().await;
    for name in [
        "tier_divergent",
        "grants_divergent",
        "tax_readiness_divergent",
        "meter_binding_divergent",
    ] {
        must_succeed(&conn, &flag(TENANT, PLAN, name)).await;
    }
    must_be_rejected(
        &conn,
        &flag(TENANT, PLAN, "tier_divergent"),
        "pricing_operator_flag_pkey",
    )
    .await;
    // Another subject, and another tenant, raise their own.
    must_succeed(&conn, &flag(TENANT, PLAN_B, "tier_divergent")).await;
    must_succeed(&conn, &flag(TENANT_B, PLAN, "tier_divergent")).await;
    // And clearing is a DELETE, which is what makes the flag a presence rather
    // than the newest of several rows.
    must_succeed(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_operator_flag \
             WHERE tenant_id = '{TENANT}' AND subject_ref = '{PLAN}' \
               AND flag = 'tier_divergent'"
        ),
    )
    .await;
    must_succeed(&conn, &flag(TENANT, PLAN, "tier_divergent")).await;
}

// ===========================================================================
// `pricing_idempotency_dedup` — pricing_idempotency_dedup
// ===========================================================================

fn dedup(overrides: &[(&str, &str)]) -> String {
    render(
        "pricing_idempotency_dedup",
        &[
            ("tenant_id", format!("'{TENANT}'")),
            ("operation", "'createPlan'".to_owned()),
            ("client_key", "'key-1'".to_owned()),
            ("request_hash", "'\\x00'".to_owned()),
        ],
        overrides,
    )
}

/// A response status outside the HTTP range is not a status.
///
/// The row carries a `response_body`, and it has to: the answered-pairing
/// constraint would otherwise refuse a status with no body and this test would
/// be green while saying nothing about the constraint it names.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_response_status_outside_the_http_range_is_refused() {
    let conn = applied().await;
    for status in ["99", "600", "0", "-1"] {
        must_be_rejected(
            &conn,
            &dedup(&[("response_status", status), ("response_body", "'{}'")]),
            "chk_pricing_idempotency_dedup_status",
        )
        .await;
    }
    // The bounds are inclusive on both ends.
    for status in ["100", "599"] {
        must_succeed(
            &conn,
            &dedup(&[
                ("client_key", &format!("'key-{status}'")),
                ("response_status", status),
                ("response_body", "'{}'"),
            ]),
        )
        .await;
    }
}

/// The stored answer is a pair, refused in **both** directions.
///
/// The columns are nullable because the at-most-once gate is the key insert
/// itself, so the row exists before the operation it guards has produced
/// anything — `NULL` is the honest reading of "claimed, not yet answered". What
/// this constraint forbids is the **half**-recorded answer: a status with no body
/// replays an empty response as though it were the original, and a body with no
/// status has no code to replay it under.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_half_recorded_idempotent_answer_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &dedup(&[("response_status", "200")]),
        "chk_pricing_idempotency_dedup_answered",
    )
    .await;
    must_be_rejected(
        &conn,
        &dedup(&[("response_body", "'{}'")]),
        "chk_pricing_idempotency_dedup_answered",
    )
    .await;
    // The claimed-not-yet-answered row, which is what the gate inserts first.
    must_succeed(&conn, &dedup(&[])).await;
    must_succeed(
        &conn,
        &dedup(&[
            ("client_key", "'key-2'"),
            ("response_status", "201"),
            ("response_body", "'{\"planId\":\"p\"}'"),
        ]),
    )
    .await;
}

/// **The at-most-once gate is the primary key**, not an optimization.
///
/// Two concurrent requests carrying one client key race to insert and exactly one
/// wins. A uniqueness rule enforced by a read-then-write in application code
/// would admit both under concurrency, which is the one situation idempotency
/// exists for — so this key is the mechanism and not a detail of it.
///
/// The accepting cases say what the key does **not** conflate: the same client
/// key under a different operation, and under a different tenant, are different
/// claims.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_at_most_once_gate_is_the_primary_key() {
    let conn = applied().await;
    must_succeed(&conn, &dedup(&[])).await;
    must_be_rejected(
        &conn,
        &dedup(&[("request_hash", "'\\x01'")]),
        "pricing_idempotency_dedup_pkey",
    )
    .await;
    must_succeed(&conn, &dedup(&[("operation", "'createPrice'")])).await;
    must_succeed(&conn, &dedup(&[("tenant_id", &format!("'{TENANT_B}'"))])).await;
}

// ===========================================================================
// `pricing_outbox` — pricing_outbox
// ===========================================================================

const OUTBOX_1: &str = "66666666-0000-0000-0000-000000000001";
const OUTBOX_2: &str = "66666666-0000-0000-0000-000000000002";
const CORRELATION: &str = "77777777-0000-0000-0000-000000000001";

/// Fourteen names, the same set as `domain::events::CatalogEvent`.
///
/// Thirteen until 2026-08-07. `PriceOverlayPublished` (D-248) joins here and in
/// `chk_pricing_outbox_event_name` (`pricing_outbox`) together — this roster and
/// that constraint are the pair that made the addition a migration rather than an
/// enum edit, and `every_frozen_event_name_is_enqueueable` below is what pins them
/// to each other by driving every name into the table.
const EVENT_NAMES: &[&str] = &[
    "PlanCreated",
    "PlanUpdated",
    "PlanPublished",
    "PlanRetired",
    "PlanMigrationScheduled",
    "PlanPublishDegraded",
    "BundleUpdated",
    "PriceCreated",
    "PriceUpdated",
    "PriceWindowScheduled",
    "PriceWindowActivated",
    "PriceWindowExpired",
    "PriceWindowCancelled",
    "PriceOverlayPublished",
];

fn outbox(id: &str, overrides: &[(&str, &str)]) -> String {
    render(
        "pricing_outbox",
        &[
            ("outbox_id", format!("'{id}'")),
            ("tenant_id", format!("'{TENANT}'")),
            ("aggregate_id", format!("'{PLAN}'")),
            ("event_name", "'PlanPublished'".to_owned()),
            ("seq", "0".to_owned()),
            ("payload", "'{}'".to_owned()),
            ("dedup_key", format!("'{id}'")),
            ("correlation_id", format!("'{CORRELATION}'")),
        ],
        overrides,
    )
}

/// The frozen event-name set, from the accepting side.
///
/// A name here is a contract a consumer is entitled to keep receiving forever, so
/// all fourteen are exercised: a suite that only enqueued `PlanPublished` would
/// leave twelve of the admitted set unproved and would stay green against a
/// constraint narrowed to one name.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_frozen_event_name_is_enqueueable() {
    let conn = applied().await;
    for (index, name) in EVENT_NAMES.iter().enumerate() {
        must_succeed(
            &conn,
            &outbox(
                &format!("66666666-0000-0000-0000-0000000000{index:02}"),
                &[
                    ("event_name", &format!("'{name}'")),
                    ("seq", &index.to_string()),
                    ("dedup_key", &format!("'{name}'")),
                ],
            ),
        )
        .await;
    }
    let mut driven: Vec<String> = EVENT_NAMES.iter().map(|name| (*name).to_owned()).collect();
    driven.sort();
    assert_eq!(
        admitted_event_names(&conn).await,
        driven,
        "the admitted set and the list this loop drives must be the same set, in both \
         directions: a name added to `chk_pricing_outbox_event_name` and not here is an \
         admitted event no case ever enqueues, and a name here and not in the CHECK would \
         have failed the loop above"
    );
}

/// The event names `chk_pricing_outbox_event_name` admits, **read off the
/// catalog**, sorted.
///
/// This assertion used to compare `EVENT_NAMES.len()` with the literal `14`,
/// which is a fact about the array and about nothing else: a fifteenth token
/// added to `chk_pricing_outbox_event_name` leaves both
/// the array and the literal untouched, so an admitted event name with no case
/// driving it into the table read as covered. This is the same list-versus-catalog
/// blindness `pg_support::frozen_columns` closes for columns.
///
/// The parse is deliberately crude: Postgres renders the constraint as
/// `CHECK ((event_name = ANY (ARRAY['PlanCreated'::text, …])))`, no event name
/// contains a quote, so every odd single-quoted run is one admitted name.
async fn admitted_event_names(conn: &DatabaseConnection) -> Vec<String> {
    let def: String = conn
        .query_one_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_get_constraintdef(oid) AS v FROM pg_constraint \
             WHERE conname = 'chk_pricing_outbox_event_name'"
                .to_owned(),
        ))
        .await
        .expect("query the constraint")
        .expect("chk_pricing_outbox_event_name is declared")
        .try_get("", "v")
        .expect("read the definition");
    let mut names: Vec<String> = def
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(
        !names.is_empty(),
        "the CHECK must still be a list of quoted names; got: {def}"
    );
    names.sort();
    names
}

/// A typo must fail at insert rather than become an event nobody is subscribed
/// to.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_event_name_outside_the_frozen_fourteen_is_refused() {
    let conn = applied().await;
    for name in ["'PlanDeleted'", "'planPublished'", "'PriceRetired'"] {
        must_be_rejected(
            &conn,
            &outbox(OUTBOX_1, &[("event_name", name)]),
            "chk_pricing_outbox_event_name",
        )
        .await;
    }
}

/// The per-aggregate sequence counts up from zero.
///
/// Named apart from `uq_pricing_outbox_sequence`, which is a different object
/// over the same column: this one is the floor, that one is the uniqueness.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_outbox_sequence_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &outbox(OUTBOX_1, &[("seq", "-1")]),
        "chk_pricing_outbox_sequence",
    )
    .await;
    must_succeed(&conn, &outbox(OUTBOX_1, &[("seq", "0")])).await;
}

/// Two rows at one seq would leave the relay free to pick either, so the
/// per-aggregate order is a **total** order.
///
/// The colliding row carries a distinct `dedup_key` and a distinct `outbox_id`,
/// or `uq_pricing_outbox_dedup_key` or the primary key would be what answered.
/// The accepting case is the other half of the claim: ordering is per
/// `(tenant_id, aggregate_id)` and **not global**, because a global sequence
/// would serialize every tenant's publishing behind one counter.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_events_of_one_aggregate_cannot_share_a_sequence_number() {
    let conn = applied().await;
    must_succeed(&conn, &outbox(OUTBOX_1, &[("seq", "5")])).await;
    must_be_rejected(
        &conn,
        &outbox(OUTBOX_2, &[("seq", "5")]),
        "uq_pricing_outbox_sequence",
    )
    .await;
    // The same seq under another aggregate, and under another tenant.
    must_succeed(
        &conn,
        &outbox(
            OUTBOX_2,
            &[("seq", "5"), ("aggregate_id", &format!("'{PLAN_B}'"))],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &outbox(
            "66666666-0000-0000-0000-000000000003",
            &[("seq", "5"), ("tenant_id", &format!("'{TENANT_B}'"))],
        ),
    )
    .await;
}

/// The dedup key dedups **at the writer**, rather than at every consumer.
///
/// The colliding row carries a distinct `(aggregate_id, seq)` and a distinct
/// `outbox_id`, so neither the sequence index nor the primary key can be what
/// answers.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_events_of_one_tenant_cannot_share_a_dedup_key() {
    let conn = applied().await;
    must_succeed(&conn, &outbox(OUTBOX_1, &[("dedup_key", "'d-1'")])).await;
    must_be_rejected(
        &conn,
        &outbox(
            OUTBOX_2,
            &[
                ("dedup_key", "'d-1'"),
                ("aggregate_id", &format!("'{PLAN_B}'")),
                ("seq", "9"),
            ],
        ),
        "uq_pricing_outbox_dedup_key",
    )
    .await;
    // Dedup is per tenant: another tenant's identical key is another event.
    must_succeed(
        &conn,
        &outbox(
            OUTBOX_2,
            &[
                ("dedup_key", "'d-1'"),
                ("tenant_id", &format!("'{TENANT_B}'")),
            ],
        ),
    )
    .await;
}

/// One outbox row, one id.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_outbox_id_is_one_row() {
    let conn = applied().await;
    must_succeed(&conn, &outbox(OUTBOX_1, &[])).await;
    must_be_rejected(
        &conn,
        &outbox(
            OUTBOX_1,
            &[
                ("tenant_id", &format!("'{TENANT_B}'")),
                ("aggregate_id", &format!("'{PLAN_B}'")),
                ("seq", "9"),
                ("dedup_key", "'d-9'"),
            ],
        ),
        "pricing_outbox_pkey",
    )
    .await;
}
