//! Create `bss.products_audit_log` — the append-only audit trail (P-D-193,
//! P-D-200). Every row the gear writes today is a door's act on a subject:
//! submission and every terminal unit transition, category and SKU acts.
//!
//! It also carries the **reserved platform-sealing seam** (P-D-200) and never
//! seals it here: `seal_state`, `chain_id`, `seq`, `prev_hash` and `row_hash`.
//! `seal_state` is written `unsealed` at INSERT, always, so the unproven era is
//! queryable rather than inferred from a deployment date. This gear computes no
//! hash and runs no verification job — that is the platform capability's job.
//!
//! # `audit_id` is a surrogate uuid primary key, not `(tenant_id, chain_id, seq)`
//!
//! The sealing seam's one-way `UPDATE` has to address a row that is **not yet
//! sealed**, and `seq` is null until it is (P-D-200). A key built from a column
//! that is null on every unsealed row cannot address that row, so the key is
//! independent of the chain's ordering altogether. Pricing's `pricing_audit`
//! keys the same way (pricing D-433).
//!
//! # `actor_ref` is `uuid`
//!
//! Pseudonymous by construction, never a display name or an email address,
//! typed `uuid` rather than `text` so that constraint is physical: a `text`
//! column retained for years would eventually be handed directly-identifying
//! operator PII.
//!
//! # `subject_id` and `subject_revision` are nullable
//!
//! `chk_products_audit_log_subject_ref` is the "every row is identifiable by
//! something" rule: a row carries a `subject_id`, an `attempted_key`, or a
//! `session_id`. The gear writes `subject_id` on every row; `error_code`,
//! `attempted_key`, `session_id` and `ceremony_ref` are carried in the DDL and
//! written `NULL` (P-D-200).
//!
//! # No vocabulary `CHECK` on `action` or `subject_kind` — an owed debt
//!
//! The two columns are free tokens written by the doors. A vocabulary `CHECK`
//! on each is owed once the domain names a closed roster for them; it arrives
//! as a new migration, since the chain is deployed.
//!
//! # `correlation_id` is `text`
//!
//! The column holds a request's correlation (P-D-200). Products writes `NULL`
//! on every row: this gear establishes no request correlation. Pricing's twin
//! column carries its edge id (pricing D-431).
//!
//! # The append-only trigger guard
//!
//! **DELETE is refused unconditionally on both engines.**
//!
//! **UPDATE admits exactly one transition**: `unsealed` to `sealed`, one-way,
//! supplying `chain_id`, `seq`, `prev_hash` and `row_hash` together in the same
//! statement, with every **record** column unchanged from `OLD`.
//!
//! `prev_hash` is one of the four the seal supplies, not one of the columns
//! held unchanged — it is the link to the previous row in the segment. It is
//! the only one of the four that may stay `NULL`, and a `NULL` one means this
//! row is the segment head. Requiring it unchanged would pin it at the `NULL`
//! every `unsealed` row carries, so every sealed row would be a segment head
//! and the chain could never link — which is the whole of what a hash chain
//! is. Nothing can rewrite it afterwards either: an already-`sealed` row
//! matches no admitted transition, since the arm requires `OLD.seal_state` to
//! be `unsealed`. Without this arm the platform's sealing capability, which
//! computes the seal asynchronously over rows already immutable by trigger,
//! would be refused by a whitelist that admitted no column at all — precisely
//! the migration the reserved seam exists to avoid. The sealer's identity is an
//! application and grant guarantee, not something the trigger reads: the
//! session variable that would carry it exists on Postgres and not on
//! `SQLite`, so neither trigger reads one.
//!
//! `REVOKE UPDATE, DELETE` is not issued (P-D-200): it names a deployment role
//! this migration does not own, and `SQLite` has no `GRANT`/`REVOKE`.
//!
//! # Backend differences
//!
//! Postgres raises through one `PL/pgSQL` function branching on `TG_OP`, with
//! one trigger firing `BEFORE DELETE OR UPDATE`; its `DOWN` drops the function
//! as well as the table. `SQLite` has no procedural language and
//! `RAISE(ABORT, ...)` takes a literal message, so the mirror is three
//! triggers with fixed messages and `WHEN` clauses carrying the predicates:
//! one refusing every DELETE, one refusing every UPDATE that is not the
//! admitted sealing transition, and one refusing a sealing UPDATE that also
//! changes a record column. Postgres compares `NEW` against `OLD` with `IS
//! DISTINCT FROM` so a `NULL`-to-`NULL` comparison behaves; `SQLite` uses `IS NOT`,
//! its own null-safe form. `uuid` becomes `text`, `bytea` becomes `blob`,
//! `timestamptz` becomes `text`, and the `bss.` qualification is dropped.
//! Every `CHECK` and index is preserved on both sides.
//!

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_audit_log (
            audit_id          uuid        NOT NULL,
            tenant_id         uuid        NOT NULL,
            actor_ref         uuid        NOT NULL,
            action            text        NOT NULL,
            subject_kind      text        NOT NULL,
            subject_id        uuid,
            subject_revision  bigint,
            error_code        text,
            attempted_key     text,
            reason            text,
            correlation_id    text,
            written_at        timestamptz NOT NULL,
            session_id        uuid,
            ceremony_ref      uuid,
            seal_state        text        NOT NULL,
            chain_id          uuid,
            seq               bigint,
            prev_hash         bytea,
            row_hash          bytea,
            CONSTRAINT products_audit_log_pkey PRIMARY KEY (audit_id),
            CONSTRAINT chk_products_audit_log_seal_state CHECK (seal_state IN ('unsealed', 'sealed')),
            CONSTRAINT chk_products_audit_log_seal_group CHECK (
                (seal_state = 'unsealed' AND chain_id IS NULL AND seq IS NULL AND prev_hash IS NULL AND row_hash IS NULL)
                OR
                (seal_state = 'sealed' AND chain_id IS NOT NULL AND seq IS NOT NULL AND row_hash IS NOT NULL)
            ),
            CONSTRAINT chk_products_audit_log_seq CHECK (seq IS NULL OR seq >= 0),
            CONSTRAINT chk_products_audit_log_subject_ref CHECK (subject_id IS NOT NULL OR attempted_key IS NOT NULL OR session_id IS NOT NULL)
        )",
    "CREATE INDEX IF NOT EXISTS idx_products_audit_log_tenant_time ON bss.products_audit_log USING btree (tenant_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_products_audit_log_subject ON bss.products_audit_log USING btree (tenant_id, subject_kind, subject_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_products_audit_log_actor ON bss.products_audit_log USING btree (tenant_id, actor_ref, written_at)",
    "CREATE OR REPLACE FUNCTION bss.products_audit_log_append_only() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION 'products_audit_log is append-only: DELETE is not permitted';
          END IF;

          IF OLD.seal_state = 'unsealed'
             AND NEW.seal_state = 'sealed'
             AND NEW.chain_id IS NOT NULL
             AND NEW.seq IS NOT NULL
             AND NEW.row_hash IS NOT NULL
             AND NEW.audit_id IS NOT DISTINCT FROM OLD.audit_id
             AND NEW.tenant_id IS NOT DISTINCT FROM OLD.tenant_id
             AND NEW.actor_ref IS NOT DISTINCT FROM OLD.actor_ref
             AND NEW.action IS NOT DISTINCT FROM OLD.action
             AND NEW.subject_kind IS NOT DISTINCT FROM OLD.subject_kind
             AND NEW.subject_id IS NOT DISTINCT FROM OLD.subject_id
             AND NEW.subject_revision IS NOT DISTINCT FROM OLD.subject_revision
             AND NEW.error_code IS NOT DISTINCT FROM OLD.error_code
             AND NEW.attempted_key IS NOT DISTINCT FROM OLD.attempted_key
             AND NEW.reason IS NOT DISTINCT FROM OLD.reason
             AND NEW.correlation_id IS NOT DISTINCT FROM OLD.correlation_id
             AND NEW.written_at IS NOT DISTINCT FROM OLD.written_at
             AND NEW.session_id IS NOT DISTINCT FROM OLD.session_id
             AND NEW.ceremony_ref IS NOT DISTINCT FROM OLD.ceremony_ref
          THEN
            RETURN NEW;
          END IF;

          RAISE EXCEPTION 'products_audit_log is append-only: % is not permitted', TG_OP;
        END;
     $$ LANGUAGE plpgsql",
    "DROP TRIGGER IF EXISTS trg_products_audit_log_append_only ON bss.products_audit_log",
    "CREATE TRIGGER trg_products_audit_log_append_only BEFORE DELETE OR UPDATE ON bss.products_audit_log FOR EACH ROW EXECUTE FUNCTION bss.products_audit_log_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.products_audit_log",
    "DROP FUNCTION IF EXISTS bss.products_audit_log_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_audit_log (
            audit_id          text   NOT NULL,
            tenant_id         text   NOT NULL,
            actor_ref         text   NOT NULL,
            action            text   NOT NULL,
            subject_kind      text   NOT NULL,
            subject_id        text,
            subject_revision  bigint,
            error_code        text,
            attempted_key     text,
            reason            text,
            correlation_id    text,
            written_at        text   NOT NULL,
            session_id        text,
            ceremony_ref      text,
            seal_state        text   NOT NULL,
            chain_id          text,
            seq               bigint,
            prev_hash         blob,
            row_hash          blob,
            PRIMARY KEY (audit_id),
            CONSTRAINT chk_products_audit_log_seal_state CHECK (seal_state IN ('unsealed', 'sealed')),
            CONSTRAINT chk_products_audit_log_seal_group CHECK (
                (seal_state = 'unsealed' AND chain_id IS NULL AND seq IS NULL AND prev_hash IS NULL AND row_hash IS NULL)
                OR
                (seal_state = 'sealed' AND chain_id IS NOT NULL AND seq IS NOT NULL AND row_hash IS NOT NULL)
            ),
            CONSTRAINT chk_products_audit_log_seq CHECK (seq IS NULL OR seq >= 0),
            CONSTRAINT chk_products_audit_log_subject_ref CHECK (subject_id IS NOT NULL OR attempted_key IS NOT NULL OR session_id IS NOT NULL)
        )",
    "CREATE INDEX IF NOT EXISTS idx_products_audit_log_tenant_time ON products_audit_log (tenant_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_products_audit_log_subject ON products_audit_log (tenant_id, subject_kind, subject_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_products_audit_log_actor ON products_audit_log (tenant_id, actor_ref, written_at)",
    "DROP TRIGGER IF EXISTS trg_products_audit_log_no_delete",
    "CREATE TRIGGER trg_products_audit_log_no_delete BEFORE DELETE ON products_audit_log FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'products_audit_log is append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS trg_products_audit_log_no_update",
    "CREATE TRIGGER trg_products_audit_log_no_update BEFORE UPDATE ON products_audit_log FOR EACH ROW WHEN NOT (
            OLD.seal_state IS 'unsealed'
            AND NEW.seal_state IS 'sealed'
            AND NEW.chain_id IS NOT NULL
            AND NEW.seq IS NOT NULL
            AND NEW.row_hash IS NOT NULL
        ) BEGIN SELECT RAISE(ABORT, 'products_audit_log is append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS trg_products_audit_log_seal_unchanged",
    "CREATE TRIGGER trg_products_audit_log_seal_unchanged BEFORE UPDATE ON products_audit_log FOR EACH ROW WHEN (
            OLD.seal_state IS 'unsealed'
            AND NEW.seal_state IS 'sealed'
            AND NEW.chain_id IS NOT NULL
            AND NEW.seq IS NOT NULL
            AND NEW.row_hash IS NOT NULL
        ) AND NOT (
            NEW.audit_id IS OLD.audit_id
            AND NEW.tenant_id IS OLD.tenant_id
            AND NEW.actor_ref IS OLD.actor_ref
            AND NEW.action IS OLD.action
            AND NEW.subject_kind IS OLD.subject_kind
            AND NEW.subject_id IS OLD.subject_id
            AND NEW.subject_revision IS OLD.subject_revision
            AND NEW.error_code IS OLD.error_code
            AND NEW.attempted_key IS OLD.attempted_key
            AND NEW.reason IS OLD.reason
            AND NEW.correlation_id IS OLD.correlation_id
            AND NEW.written_at IS OLD.written_at
            AND NEW.session_id IS OLD.session_id
            AND NEW.ceremony_ref IS OLD.ceremony_ref
        ) BEGIN SELECT RAISE(ABORT, 'products_audit_log is append-only: UPDATE is not permitted'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS products_audit_log"];

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
#[path = "m20260925_000004_create_products_audit_log_tests.rs"]
mod tests;
