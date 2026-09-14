//! Create `bss.pricing_idempotency_dedup` — the at-most-once gate and the
//! replay-response source, keyed `(tenant_id, operation, client_key)`
//! (`design/01-foundation.md` §3.7).
//!
//! The row holds a hash of the request payload alongside the stored response,
//! and that pairing is the whole mechanism: a replay whose hash **matches**
//! returns the stored response; a replay whose hash **differs** is rejected with
//! `IDEMPOTENCY_PAYLOAD_MISMATCH` and is never replayed and never re-executed —
//! the two requests disagree about what they are, so neither answer would be
//! right. The idempotency check runs **before** the `ETag` check.
//!
//! The physical guard is the composite PK. It is the at-most-once gate itself,
//! not an optimization: two concurrent requests carrying the same client key
//! race to insert, and exactly one wins. A uniqueness rule enforced only by a
//! read-then-write in application code would admit both under concurrency,
//! which is the one situation idempotency exists for.
//!
//! `request_hash` is a `bytea` digest rather than the payload: the gate needs
//! to know whether two requests are the same, not what they said, and retaining
//! request bodies here would duplicate the audit trail in a table with no
//! retention story of its own.
//!
//! **The response columns are nullable, and that is the mechanism.** The
//! at-most-once gate is the `PRIMARY KEY` insert itself, so the row has to exist
//! *before* the operation it guards has produced anything to store. Seeding a
//! fabricated status in the meantime would put a value in a column that means
//! "this is what the caller was told" while nobody had been told anything — and
//! a row leaking past its transaction would then replay a fiction. `NULL` is the
//! honest reading of "claimed, not yet answered", and the pairing `CHECK` keeps
//! the two columns from drifting into a half-recorded answer that no reader
//! could interpret.
//!
//! **Backend differences.** `bytea` becomes `blob` and `jsonb` becomes `text` on
//! `SQLite`; nothing behavioural changes. Both `CHECK`s are written in the
//! subset both backends agree on — `IS NULL` yields a comparable value on each,
//! so the pairing constraint needs no dialect-specific spelling.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_idempotency_dedup (
            tenant_id       uuid        NOT NULL,
            operation       text        NOT NULL,
            client_key      text        NOT NULL,
            request_hash    bytea       NOT NULL,
            response_body   jsonb,
            response_status integer,
            created_at_utc  timestamptz NOT NULL DEFAULT now(),
            CONSTRAINT chk_pricing_idempotency_dedup_answered CHECK ((response_status IS NULL) = (response_body IS NULL)),
            CONSTRAINT chk_pricing_idempotency_dedup_status CHECK (response_status IS NULL OR response_status BETWEEN 100 AND 599),
            CONSTRAINT pricing_idempotency_dedup_pkey PRIMARY KEY (tenant_id, operation, client_key)
        )",
    "CREATE INDEX idx_pricing_idempotency_dedup_created ON bss.pricing_idempotency_dedup USING btree (tenant_id, created_at_utc)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_idempotency_dedup"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_idempotency_dedup (
            tenant_id       text    NOT NULL,
            operation       text    NOT NULL,
            client_key      text    NOT NULL,
            request_hash    blob    NOT NULL,
            response_body   text,
            response_status integer,
            created_at_utc  text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            PRIMARY KEY (tenant_id, operation, client_key),
            CONSTRAINT chk_pricing_idempotency_dedup_answered CHECK ((response_status IS NULL) = (response_body IS NULL)),
            CONSTRAINT chk_pricing_idempotency_dedup_status CHECK (response_status IS NULL OR response_status BETWEEN 100 AND 599)
        )",
    "CREATE INDEX idx_pricing_idempotency_dedup_created ON pricing_idempotency_dedup (tenant_id, created_at_utc)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_idempotency_dedup"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP_STATEMENTS, SQLITE_UP_STATEMENTS).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            PG_DOWN_STATEMENTS,
            SQLITE_DOWN_STATEMENTS,
        )
        .await
    }
}
