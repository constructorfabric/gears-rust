//! Create `bss.products_idempotency` — the at-most-once gate for every
//! mutating flow that carries an `Idempotency-Key`, keyed `(tenant_id,
//! endpoint, client_key)` (P-D-193, P-D-198).
//!
//! # The claim `INSERT` is the gate, not a lookup — this migration lays down
//! only the storage half
//!
//! `cpt-cf-bss-products-dod-idempotency-store` and P-D-198 make the claim
//! `INSERT` itself the concurrency gate: it joins the guarded mutation's own
//! transaction, so a rollback frees the key with no separate release step.
//! This migration lays down exactly the table shape that claim, the takeover's
//! compare-and-swap and the `CHECK` need.
//!
//! # No `in_flight_until` column — its absence is a decision, not an
//! oversight
//!
//! An in-flight deadline would exist only for a claim committed in its own
//! transaction ahead of the mutation it guards. The claim `INSERT` sits inside
//! the mutation's transaction instead (P-D-198): an unanswered claim cannot
//! outlive its transaction, because a rollback frees the key automatically.
//! There is no state left for a separate deadline to describe.
//!
//! # `state`, the response pair, and the one `CHECK` that ties them
//!
//! `state` is `claimed` or `answered`, and `chk_products_idempotency_response_group`
//! ties it to the response columns: `claimed` implies both response columns
//! `NULL`, `answered` implies both `NOT NULL`. There is no third shape.
//!
//! **An answer is stored only when its transaction commits** (P-D-198): the
//! answer write joins the mutation's transaction and rolls back with it, and
//! the claim shares that transaction too, so a refusal that rolls back leaves
//! no row behind and the key is free for an immediate retry. A refusal whose
//! transaction commits (the 400 `UNIT_STALE` after a refresh) is stored and
//! replays.
//!
//! The replay is self-contained (P-D-198): `response_status` and
//! `response_body` are what a replay reproduces, not a reference to some other
//! row the answer might point at.
//!
//! # `expires_at` is both the retention deadline and the claim stamp
//!
//! `expires_at` is stamped at the claim `INSERT` from the configured retention
//! and is the retention window of the key. It is also the operand of the
//! expired-key takeover's compare-and-swap (P-D-198): nothing holds an expired
//! row between one transaction's conflict check and its takeover `UPDATE`, so
//! two duplicates on one expired key can both clear the check and both read
//! the same expired row. The takeover `UPDATE` therefore carries
//! `WHERE expires_at = <the value the reader saw>`; exactly one matches, the
//! other finds nothing left to update, and the loser is refused
//! `IDEMPOTENCY_KEY_IN_FLIGHT` having executed nothing.
//!
//! Retention (at least 24 hours, P-D-198) is configuration, not a constraint
//! this table can express. Expiry is decided at claim time, so correctness
//! never waits on a sweep.
//!
//! # `endpoint` is the concrete resource path, not the route template
//!
//! Keying on the route template would let two submits of different SKUs under
//! one client key share the whole key and an identical empty-body hash, so the
//! second would replay the first's answer without running. `endpoint`
//! therefore holds the concrete path a wire caller resolved, never the
//! template it matched (P-D-198).
//!
//! # No append-only trigger — an expiring operational store
//!
//! Unlike `products_audit_log`, this table carries **no append-only guard**.
//! It is expiring operational state, not a record: a row is claimed, answered,
//! taken over past its expiry, or eventually swept away, none of which an
//! append-only posture would admit. A later reader must not "restore" a guard
//! here by analogy with the audit log.
//!
//! # Backend differences
//!
//! `uuid` becomes `text` for `tenant_id`; `bytea` becomes `blob` for
//! `payload_hash`; `jsonb` becomes `text` for `response_body`; `timestamptz`
//! becomes `text` for `expires_at`; and the `bss.` qualification is dropped.
//! Both `CHECK`s and the primary key are preserved on both sides.
//!
//! # `entity_ref`
//!
//! Nullable and outside both `CHECK`s. It is carried from the backup chain's
//! DDL (P-D-193) and always `NULL`: no door stamps it (P-D-198).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_idempotency (
            tenant_id       uuid        NOT NULL,
            endpoint        text        NOT NULL,
            client_key      text        NOT NULL,
            state           text        NOT NULL,
            payload_hash    bytea       NOT NULL,
            response_status integer,
            response_body   jsonb,
            expires_at      timestamptz NOT NULL,
            entity_ref      uuid,
            CONSTRAINT products_idempotency_pkey PRIMARY KEY (tenant_id, endpoint, client_key),
            CONSTRAINT chk_products_idempotency_state CHECK (state IN ('claimed', 'answered')),
            CONSTRAINT chk_products_idempotency_response_group CHECK (
                (state = 'claimed' AND response_status IS NULL AND response_body IS NULL)
                OR
                (state = 'answered' AND response_status IS NOT NULL AND response_body IS NOT NULL)
            )
        )",
    "CREATE INDEX IF NOT EXISTS idx_products_idempotency_expires ON bss.products_idempotency USING btree (tenant_id, expires_at)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.products_idempotency"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_idempotency (
            tenant_id       text    NOT NULL,
            endpoint        text    NOT NULL,
            client_key      text    NOT NULL,
            state           text    NOT NULL,
            payload_hash    blob    NOT NULL,
            response_status integer,
            response_body   text,
            expires_at      text    NOT NULL,
            entity_ref      text,
            PRIMARY KEY (tenant_id, endpoint, client_key),
            CONSTRAINT chk_products_idempotency_state CHECK (state IN ('claimed', 'answered')),
            CONSTRAINT chk_products_idempotency_response_group CHECK (
                (state = 'claimed' AND response_status IS NULL AND response_body IS NULL)
                OR
                (state = 'answered' AND response_status IS NOT NULL AND response_body IS NOT NULL)
            )
        )",
    "CREATE INDEX IF NOT EXISTS idx_products_idempotency_expires ON products_idempotency (tenant_id, expires_at)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS products_idempotency"];

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

#[cfg(test)]
#[path = "m20260925_000005_create_products_idempotency_tests.rs"]
mod tests;
