//! Postgres-only end-to-end for the dormant retention seam (Slice 6 Phase 4
//! Group 4B): the `chain_checkpoint` writer + the §4.8/E-5 partition detach
//! gate. Boots a container, migrates, then asserts:
//!   (A) `CheckpointWriter::write_checkpoint` DERIVES the covered-entry count by
//!       walking the chain range — a caller no longer supplies it —
//!       and rejects a non-contiguous range;
//!   (B) `DetachGate::may_detach` enforces BOTH §4.8 halves: the
//!       period must be fully sealed AND every sealed entry must be covered by a
//!       `chain_checkpoint` range. A sealed-but-uncovered period is blocked
//!       (`uncovered_count`); a covered period passes; an unsealed entry blocks
//!       (`unsealed_count`).
//!
//! Test choice: rather than drive the heavy `InvoicePostService`, entries are
//! raw-INSERTed directly into `bss.ledger_journal_entry` with the DEFERRABLE
//! balanced-entry trigger disabled (`trg_journal_entry_balanced`, the same
//! trigger `postgres_cross_tenant.rs` disables). The seam is a pure read over
//! `row_hash` / `prev_hash` / `created_seq`, so a hand-seeded chain exercises it
//! exactly and keeps the test fast and deterministic.
//!
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic,
    clippy::too_many_lines
)]

use bss_ledger::infra::retention::{CheckpointWriter, DetachGate};
use bss_ledger::infra::storage::entity::chain_checkpoint;
use bss_ledger::infra::storage::migrations::Migrator;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Statement,
};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::{AccessScope, SecureEntityExt};
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn.query_one(pg(sql.to_owned())).await.unwrap();
    row.map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap())
}

/// Lowercase hex of a byte slice (for `decode(... ,'hex')` literals + Rust-side
/// hash values that must match the inserted bytea byte-for-byte).
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A distinct 32-byte hash filled with `fill`.
fn h32(fill: u8) -> Vec<u8> {
    vec![fill; 32]
}

/// Boot a container, migrate the whole chain, and return the raw connection +
/// a scoped `DBProvider`.
async fn setup(url: &str) -> (DatabaseConnection, DBProvider<DbError>) {
    let raw = Database::connect(url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);
    (raw, provider)
}

/// Raw-INSERT one `journal_entry` header into `(tenant, period)` with the given
/// `row_hash` / `prev_hash` (`bytea` SQL literals, or `NULL`). `created_seq` is
/// auto-assigned (BIGSERIAL), so entries are ordered by insertion. The
/// balanced-entry trigger is disabled around the line-less insert.
async fn insert_entry(
    raw: &DatabaseConnection,
    tenant: Uuid,
    period: &str,
    row_hash_sql: &str,
    prev_hash_sql: &str,
) {
    let entry_id = Uuid::now_v7();
    let actor = Uuid::now_v7();
    let correlation = Uuid::now_v7();
    raw.execute(pg(
        "ALTER TABLE bss.ledger_journal_entry DISABLE TRIGGER trg_journal_entry_balanced",
    ))
    .await
    .unwrap();
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_journal_entry \
           (entry_id, tenant_id, legal_entity_id, period_id, entry_currency, \
            source_doc_type, source_business_id, posted_at_utc, effective_at, \
            origin, posted_by_actor_id, correlation_id, rounding_evidence, \
            row_hash, prev_hash) \
         VALUES ('{entry_id}','{tenant}','{tenant}','{period}','USD', \
            'INVOICE_POST','INV-RET', now(), '2026-06-01', \
            'SYSTEM','{actor}','{correlation}', '{{}}'::jsonb, {row_hash_sql}, {prev_hash_sql})"
    )))
    .await
    .unwrap();
    raw.execute(pg(
        "ALTER TABLE bss.ledger_journal_entry ENABLE TRIGGER trg_journal_entry_balanced",
    ))
    .await
    .unwrap();
}

fn bytea(hash: &[u8]) -> String {
    format!("decode('{}','hex')", hex(hash))
}

/// (A) `write_checkpoint` DERIVES `covered_entry_count` by walking the chain
/// range, and rejects a non-contiguous range.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn checkpoint_derives_count_and_rejects_noncontiguous() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;

    let tenant = Uuid::now_v7();
    let period = "202606";
    let scope = AccessScope::for_tenant(tenant);
    let writer = CheckpointWriter::new(provider.clone());

    // A 3-link chain: h1 (genesis prev) ← h2 ← h3.
    let (h1, h2, h3) = (h32(0xa1), h32(0xb2), h32(0xc3));
    insert_entry(&raw, tenant, period, &bytea(&h1), "NULL").await;
    insert_entry(&raw, tenant, period, &bytea(&h2), &bytea(&h1)).await;
    insert_entry(&raw, tenant, period, &bytea(&h3), &bytea(&h2)).await;

    // Checkpoint the full range h1..h3 → derived count = 3 (caller passes NO count).
    let checkpoint_id = writer
        .write_checkpoint(&scope, tenant, h1.clone(), h3.clone())
        .await
        .expect("write_checkpoint over a contiguous range");

    let total = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.chain_checkpoint \
             WHERE checkpoint_id='{checkpoint_id}' AND tenant_id='{tenant}' \
               AND covered_entry_count=3 AND signature IS NULL"
        ),
    )
    .await;
    assert_eq!(
        total, 1,
        "covered_entry_count is DERIVED as 3, not caller-supplied"
    );

    // Read back through the secure ORM: range round-trips, signature unset.
    let conn = writer.db().conn().expect("conn");
    let row = chain_checkpoint::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(chain_checkpoint::Column::CheckpointId.eq(checkpoint_id)))
        .one(&conn)
        .await
        .expect("read chain_checkpoint")
        .expect("the checkpoint row is found under its tenant scope");
    assert_eq!(row.from_row_hash, h1, "from_row_hash round-trips");
    assert_eq!(row.to_row_hash, h3, "to_row_hash round-trips");
    assert_eq!(row.covered_entry_count, 3, "derived covered_entry_count");
    assert!(row.signature.is_none(), "an MVP checkpoint is unsigned");

    // A sub-range h2..h3 derives count = 2.
    let sub = writer
        .write_checkpoint(&scope, tenant, h2.clone(), h3.clone())
        .await
        .expect("sub-range checkpoint");
    let sub_ok = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.chain_checkpoint \
             WHERE checkpoint_id='{sub}' AND covered_entry_count=2"
        ),
    )
    .await;
    assert_eq!(sub_ok, 1, "sub-range derives count = 2");

    // A non-contiguous range (from = an unknown hash) is rejected.
    let unknown = h32(0xff);
    let err = writer
        .write_checkpoint(&scope, tenant, unknown, h3.clone())
        .await
        .expect_err("a non-contiguous range must be rejected");
    assert!(
        format!("{err:?}").contains("not contiguous"),
        "non-contiguous range error: {err:?}"
    );
}

/// (B) The detach gate enforces both §4.8 halves: sealed AND checkpoint-covered.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn detach_gate_requires_sealed_and_checkpoint_covered() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;

    let tenant = Uuid::now_v7();
    let period = "202606";
    let scope = AccessScope::for_tenant(tenant);
    let writer = CheckpointWriter::new(provider.clone());
    let gate = DetachGate::new(provider.clone());

    // Two sealed entries forming a chain in the period.
    let (h1, h2) = (h32(0x11), h32(0x22));
    insert_entry(&raw, tenant, period, &bytea(&h1), "NULL").await;
    insert_entry(&raw, tenant, period, &bytea(&h2), &bytea(&h1)).await;

    // Sealed but NOT yet checkpoint-covered → blocked, naming the uncovered count.
    let err = gate
        .may_detach(&scope, tenant, period)
        .await
        .expect_err("a sealed but un-anchored period must be blocked");
    assert_eq!(err.unsealed_count, 0, "all entries are sealed");
    assert_eq!(
        err.uncovered_count, 2,
        "neither entry is checkpoint-covered yet"
    );
    assert!(
        err.to_string().contains("PARTITION_DETACH_BLOCKED"),
        "the Display references the alarm token: {err}"
    );

    // Write a checkpoint covering h1..h2 → both halves satisfied → detach allowed.
    writer
        .write_checkpoint(&scope, tenant, h1.clone(), h2.clone())
        .await
        .expect("write covering checkpoint");
    gate.may_detach(&scope, tenant, period)
        .await
        .expect("a sealed + checkpoint-covered period may detach");

    // Add one unsealed entry → the sealed half fails first (unsealed_count = 1).
    insert_entry(&raw, tenant, period, "NULL", &bytea(&h2)).await;
    let err = gate
        .may_detach(&scope, tenant, period)
        .await
        .expect_err("an unsealed entry blocks detach");
    assert_eq!(err.unsealed_count, 1, "exactly one unsealed entry");
    assert_eq!(err.tenant_id, tenant, "the block names the tenant");
    assert_eq!(err.period_id, period, "the block names the period");

    // A foreign-tenant scope sees no rows, so its (empty) period trivially passes
    // (SQL-level BOLA — the rows are invisible to it).
    let foreign = AccessScope::for_tenant(Uuid::now_v7());
    gate.may_detach(&foreign, tenant, period)
        .await
        .expect("a foreign scope sees no rows");
}
