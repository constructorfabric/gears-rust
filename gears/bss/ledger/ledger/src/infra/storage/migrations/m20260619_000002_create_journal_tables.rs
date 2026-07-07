//! Create the append-only truth tables `journal_entry` and `journal_line`
//! in schema `bss`. Postgres carries the append-only REVOKE-equivalent
//! reject-mutation triggers and a DEFERRABLE balanced/single-payer/
//! single-currency constraint trigger; `SQLite` (non-production test
//! backend) omits all triggers and PL/pgSQL — those invariants are
//! re-asserted in application code in a later phase (P3). Every CHECK,
//! index, PK, and FK is preserved on both backends.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

pub(crate) const PG_UP_STATEMENTS: &[&str] = &[
    // --- journal_entry (append-only truth header) ---
    "CREATE TABLE bss.ledger_journal_entry (
        entry_id           uuid          NOT NULL,
        tenant_id          uuid          NOT NULL,
        legal_entity_id    uuid          NOT NULL,
        period_id          varchar(6)    NOT NULL,
        entry_currency     varchar(16)   NOT NULL,
        source_doc_type    text          NOT NULL,
        source_business_id varchar(256)  NOT NULL,
        reverses_entry_id  uuid,
        reverses_period_id varchar(6),
        posted_at_utc      timestamptz   NOT NULL DEFAULT now(),
        effective_at       date          NOT NULL,
        origin             text          NOT NULL CHECK (origin IN ('SYSTEM','USER')),
        posted_by_actor_id uuid          NOT NULL,
        correlation_id     uuid          NOT NULL,
        rounding_evidence  jsonb         NOT NULL DEFAULT '{}'::jsonb,
        created_seq        bigserial     NOT NULL,
        row_hash           bytea,
        prev_hash          bytea,
        PRIMARY KEY (tenant_id, period_id, entry_id),
        CONSTRAINT chk_journal_entry_reversal_co_null CHECK (
            (reverses_entry_id IS NULL) = (reverses_period_id IS NULL))
    )",
    "ALTER TABLE bss.ledger_journal_entry
        ADD CONSTRAINT chk_journal_entry_source_doc_type CHECK (source_doc_type IN (
            'INVOICE_POST','REVERSAL','MAPPING_CORRECTION','PAYMENT_SETTLE','PAYMENT_ALLOCATE',
            'CHARGEBACK','CREDIT_APPLY','SETTLEMENT_RETURN','MANUAL_ADJUSTMENT','SCHEDULE_BUILD',
            'RECOGNITION','CREDIT_NOTE','DEBIT_NOTE','REFUND','FX_REVALUATION','FX_REVAL_REVERSAL'))",
    "CREATE UNIQUE INDEX uq_journal_entry_reversal
        ON bss.ledger_journal_entry (tenant_id, reverses_period_id, reverses_entry_id)
        WHERE reverses_entry_id IS NOT NULL",
    "CREATE INDEX idx_journal_entry_source
        ON bss.ledger_journal_entry (tenant_id, source_doc_type, source_business_id)",
    "CREATE INDEX idx_journal_entry_created_seq
        ON bss.ledger_journal_entry (tenant_id, created_seq)",
    "CREATE INDEX idx_journal_entry_chain_scan
        ON bss.ledger_journal_entry (tenant_id, posted_at_utc, created_seq) WHERE row_hash IS NULL",
    // --- journal_line (append-only truth detail; sole source of truth) ---
    "CREATE TABLE bss.ledger_journal_line (
        line_id                uuid        NOT NULL,
        entry_id               uuid        NOT NULL,
        tenant_id              uuid        NOT NULL,
        period_id              varchar(6)  NOT NULL,
        payer_tenant_id        uuid        NOT NULL,
        seller_tenant_id       uuid,
        resource_tenant_id     uuid,
        account_id             uuid        NOT NULL,
        account_class          text        NOT NULL,
        gl_code                varchar(128),
        side                   text        NOT NULL CHECK (side IN ('DR','CR')),
        amount_minor           bigint      NOT NULL,
        currency               varchar(16) NOT NULL,
        currency_scale         smallint    NOT NULL,
        invoice_id             varchar(128),
        due_date               date,
        revenue_stream         text,
        mapping_status         text        NOT NULL CHECK (mapping_status IN ('RESOLVED','PENDING')),
        functional_amount_minor bigint,
        functional_currency    varchar(16),
        tax_jurisdiction       varchar(128),
        tax_filing_period      varchar(32),
        tax_rate_ref           varchar(128),
        legal_entity_id        uuid,
        invoice_item_ref       varchar(128),
        sku_or_plan_ref        varchar(128),
        price_id               varchar(128),
        pricing_snapshot_ref   varchar(128),
        po_allocation_group    varchar(128),
        credit_grant_event_type text,
        PRIMARY KEY (tenant_id, period_id, line_id),
        FOREIGN KEY (tenant_id, period_id, entry_id)
            REFERENCES bss.ledger_journal_entry (tenant_id, period_id, entry_id),
        CONSTRAINT chk_journal_line_account_class CHECK (account_class IN (
            'AR','CASH_CLEARING','UNALLOCATED','REUSABLE_CREDIT','CONTRACT_LIABILITY','REVENUE',
            'TAX_PAYABLE','SUSPENSE','DISPUTE_HOLD','REFUND_CLEARING','CONTRA_REVENUE','GOODWILL',
            'DISPUTE_LOSS_EXPENSE','PSP_FEE_EXPENSE','FX_GAIN_LOSS','FX_UNREALIZED')),
        CONSTRAINT chk_journal_line_amount CHECK (
            amount_minor > 0 OR (amount_minor = 0 AND functional_amount_minor IS NOT NULL)),
        CONSTRAINT chk_journal_line_tax_dims CHECK (
            account_class <> 'TAX_PAYABLE'
            OR (tax_jurisdiction IS NOT NULL AND tax_filing_period IS NOT NULL)),
        CONSTRAINT chk_journal_line_revenue_stream CHECK (
            account_class NOT IN ('REVENUE','CONTRACT_LIABILITY') OR revenue_stream IS NOT NULL),
        CONSTRAINT chk_journal_line_credit_grant CHECK (
            (account_class = 'REUSABLE_CREDIT') = (credit_grant_event_type IS NOT NULL))
    )",
    "CREATE INDEX idx_journal_line_account
        ON bss.ledger_journal_line (tenant_id, account_id, currency)",
    "CREATE INDEX idx_journal_line_ar
        ON bss.ledger_journal_line (tenant_id, payer_tenant_id, invoice_id)",
    "CREATE INDEX idx_journal_line_item
        ON bss.ledger_journal_line (tenant_id, invoice_id, invoice_item_ref)",
    "CREATE INDEX idx_journal_line_entry
        ON bss.ledger_journal_line (tenant_id, period_id, entry_id)",
    // --- append-only enforcement (Postgres-only; REVOKE-equivalent) ---
    "CREATE OR REPLACE FUNCTION bss.reject_mutation() RETURNS trigger AS $$
        BEGIN RAISE EXCEPTION 'append-only table: % not permitted', TG_OP; END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_journal_entry_append_only
        BEFORE UPDATE OR DELETE ON bss.ledger_journal_entry
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
    "CREATE TRIGGER trg_journal_line_append_only
        BEFORE UPDATE OR DELETE ON bss.ledger_journal_line
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
    // --- deferrable balanced / >=1-line / single-payer / single-currency trigger ---
    // A single non-grouped aggregate computes line_count / payer_count /
    // currency_mismatch so a ZERO-line entry is detected (a GROUP BY on the
    // entry would yield no row and silently pass). Distinct canonical RFC-9457
    // codes are raised so the P3 API layer can map each to its own status.
    "CREATE OR REPLACE FUNCTION bss.check_entry_balanced() RETURNS trigger AS $$
        DECLARE
          line_count        int;
          payer_count       int;
          currency_mismatch int;
          unbalanced        int;
        BEGIN
          SELECT count(*),
                 count(DISTINCT l.payer_tenant_id),
                 count(*) FILTER (WHERE l.currency <> NEW.entry_currency
                                  AND NOT (l.amount_minor = 0
                                           AND l.functional_amount_minor IS NOT NULL))
            INTO line_count, payer_count, currency_mismatch
            FROM bss.ledger_journal_line l
           WHERE (l.tenant_id, l.period_id, l.entry_id)
                 = (NEW.tenant_id, NEW.period_id, NEW.entry_id);

          IF line_count < 1 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_EMPTY entry=%', NEW.entry_id;
          END IF;
          IF payer_count > 1 THEN
            RAISE EXCEPTION 'MIXED_PAYER_TENANT entry=%', NEW.entry_id;
          END IF;
          IF currency_mismatch > 0 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_CURRENCY_MISMATCH entry=%', NEW.entry_id;
          END IF;

          -- zero-sum per (currency, currency_scale), exact (zero tolerance)
          SELECT count(*) INTO unbalanced FROM (
            SELECT 1
            FROM bss.ledger_journal_line l
            WHERE (l.tenant_id, l.period_id, l.entry_id)
                  = (NEW.tenant_id, NEW.period_id, NEW.entry_id)
            GROUP BY l.currency, l.currency_scale
            HAVING sum(CASE WHEN l.side = 'DR' THEN l.amount_minor
                            ELSE -l.amount_minor END) <> 0
          ) u;
          IF unbalanced > 0 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_UNBALANCED entry=%', NEW.entry_id;
          END IF;

          RETURN NULL;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE CONSTRAINT TRIGGER trg_journal_entry_balanced
        AFTER INSERT ON bss.ledger_journal_entry
        DEFERRABLE INITIALLY DEFERRED
        FOR EACH ROW EXECUTE FUNCTION bss.check_entry_balanced()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_journal_line",
    "DROP TABLE IF EXISTS bss.ledger_journal_entry",
    "DROP FUNCTION IF EXISTS bss.check_entry_balanced()",
    "DROP FUNCTION IF EXISTS bss.reject_mutation()",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------
//
// Systematic transforms from the Postgres variant:
// * schema prefix `bss.` dropped (single namespace);
// * `jsonb` → `text`, JSON default `'{}'::jsonb` → `'{}'`;
// * `timestamptz NOT NULL DEFAULT now()` → `text NOT NULL DEFAULT (CURRENT_TIMESTAMP)`;
// * `bigserial` → `integer` (SQLite ROWID alias auto-increments);
// * `bytea` → `blob`;
// * append-only + balance triggers and the PL/pgSQL functions are
//   DROPPED — those invariants are re-asserted in application code (P3).
// Every CHECK, index, PK, and FK is preserved.

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_journal_entry (
        entry_id           text          NOT NULL,
        tenant_id          text          NOT NULL,
        legal_entity_id    text          NOT NULL,
        period_id          varchar(6)    NOT NULL,
        entry_currency     varchar(16)   NOT NULL,
        source_doc_type    text          NOT NULL,
        source_business_id varchar(256)  NOT NULL,
        reverses_entry_id  text,
        reverses_period_id varchar(6),
        posted_at_utc      text          NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        effective_at       date          NOT NULL,
        origin             text          NOT NULL CHECK (origin IN ('SYSTEM','USER')),
        posted_by_actor_id text          NOT NULL,
        correlation_id     text          NOT NULL,
        rounding_evidence  text          NOT NULL DEFAULT '{}',
        created_seq        integer       NOT NULL DEFAULT 0,
        row_hash           blob,
        prev_hash          blob,
        PRIMARY KEY (tenant_id, period_id, entry_id),
        CONSTRAINT chk_journal_entry_source_doc_type CHECK (source_doc_type IN (
            'INVOICE_POST','REVERSAL','MAPPING_CORRECTION','PAYMENT_SETTLE','PAYMENT_ALLOCATE',
            'CHARGEBACK','CREDIT_APPLY','SETTLEMENT_RETURN','MANUAL_ADJUSTMENT','SCHEDULE_BUILD',
            'RECOGNITION','CREDIT_NOTE','DEBIT_NOTE','REFUND','FX_REVALUATION','FX_REVAL_REVERSAL')),
        CONSTRAINT chk_journal_entry_reversal_co_null CHECK (
            (reverses_entry_id IS NULL) = (reverses_period_id IS NULL))
    )",
    "CREATE UNIQUE INDEX uq_journal_entry_reversal
        ON ledger_journal_entry (tenant_id, reverses_period_id, reverses_entry_id)
        WHERE reverses_entry_id IS NOT NULL",
    "CREATE INDEX idx_journal_entry_source
        ON ledger_journal_entry (tenant_id, source_doc_type, source_business_id)",
    "CREATE INDEX idx_journal_entry_created_seq
        ON ledger_journal_entry (tenant_id, created_seq)",
    "CREATE INDEX idx_journal_entry_chain_scan
        ON ledger_journal_entry (tenant_id, posted_at_utc, created_seq) WHERE row_hash IS NULL",
    "CREATE TABLE ledger_journal_line (
        line_id                text        NOT NULL,
        entry_id               text        NOT NULL,
        tenant_id              text        NOT NULL,
        period_id              varchar(6)  NOT NULL,
        payer_tenant_id        text        NOT NULL,
        seller_tenant_id       text,
        resource_tenant_id     text,
        account_id             text        NOT NULL,
        account_class          text        NOT NULL,
        gl_code                varchar(128),
        side                   text        NOT NULL CHECK (side IN ('DR','CR')),
        amount_minor           bigint      NOT NULL,
        currency               varchar(16) NOT NULL,
        currency_scale         smallint    NOT NULL,
        invoice_id             varchar(128),
        due_date               date,
        revenue_stream         text,
        mapping_status         text        NOT NULL CHECK (mapping_status IN ('RESOLVED','PENDING')),
        functional_amount_minor bigint,
        functional_currency    varchar(16),
        tax_jurisdiction       varchar(128),
        tax_filing_period      varchar(32),
        tax_rate_ref           varchar(128),
        legal_entity_id        text,
        invoice_item_ref       varchar(128),
        sku_or_plan_ref        varchar(128),
        price_id               varchar(128),
        pricing_snapshot_ref   varchar(128),
        po_allocation_group    varchar(128),
        credit_grant_event_type text,
        PRIMARY KEY (tenant_id, period_id, line_id),
        FOREIGN KEY (tenant_id, period_id, entry_id)
            REFERENCES ledger_journal_entry (tenant_id, period_id, entry_id),
        CONSTRAINT chk_journal_line_account_class CHECK (account_class IN (
            'AR','CASH_CLEARING','UNALLOCATED','REUSABLE_CREDIT','CONTRACT_LIABILITY','REVENUE',
            'TAX_PAYABLE','SUSPENSE','DISPUTE_HOLD','REFUND_CLEARING','CONTRA_REVENUE','GOODWILL',
            'DISPUTE_LOSS_EXPENSE','PSP_FEE_EXPENSE','FX_GAIN_LOSS','FX_UNREALIZED')),
        CONSTRAINT chk_journal_line_amount CHECK (
            amount_minor > 0 OR (amount_minor = 0 AND functional_amount_minor IS NOT NULL)),
        CONSTRAINT chk_journal_line_tax_dims CHECK (
            account_class <> 'TAX_PAYABLE'
            OR (tax_jurisdiction IS NOT NULL AND tax_filing_period IS NOT NULL)),
        CONSTRAINT chk_journal_line_revenue_stream CHECK (
            account_class NOT IN ('REVENUE','CONTRACT_LIABILITY') OR revenue_stream IS NOT NULL),
        CONSTRAINT chk_journal_line_credit_grant CHECK (
            (account_class = 'REUSABLE_CREDIT') = (credit_grant_event_type IS NOT NULL))
    )",
    "CREATE INDEX idx_journal_line_account
        ON ledger_journal_line (tenant_id, account_id, currency)",
    "CREATE INDEX idx_journal_line_ar
        ON ledger_journal_line (tenant_id, payer_tenant_id, invoice_id)",
    "CREATE INDEX idx_journal_line_item
        ON ledger_journal_line (tenant_id, invoice_id, invoice_item_ref)",
    "CREATE INDEX idx_journal_line_entry
        ON ledger_journal_line (tenant_id, period_id, entry_id)",
    // SQLite has no `bigserial`: emulate the monotonic, positive
    // `created_seq` sequence from the row's implicit `rowid` so reads
    // round-trip a DB-generated value (Postgres uses `bigserial`).
    "CREATE TRIGGER trg_journal_entry_created_seq
        AFTER INSERT ON ledger_journal_entry
        FOR EACH ROW WHEN NEW.created_seq = 0
        BEGIN
          UPDATE ledger_journal_entry SET created_seq = NEW.rowid
          WHERE rowid = NEW.rowid;
        END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_journal_line",
    "DROP TABLE IF EXISTS ledger_journal_entry",
];

#[cfg(test)]
#[path = "m20260619_000002_create_journal_tables_tests.rs"]
mod check_drift_tests;

// ---------------------------------------------------------------------------
// Migration dispatch.
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_UP_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP_STATEMENTS,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
