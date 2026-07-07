//! Create the six derived balance-cache tables in schema `bss`. These are
//! materialized projections of the journal truth; each carries a grain PK,
//! a `version`/`last_entry_seq` for optimistic concurrency, and a
//! conditional no-negative CHECK where the account class forbids negatives.
//! `SQLite` mirrors the same shape with the systematic transforms.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_account_balance (
        tenant_id      uuid        NOT NULL,
        account_id     uuid        NOT NULL,
        currency       varchar(16) NOT NULL,
        account_class  text        NOT NULL,
        normal_side    text        NOT NULL CHECK (normal_side IN ('DR','CR')),
        balance_minor  bigint      NOT NULL DEFAULT 0,
        last_entry_seq bigint,
        version        bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, account_id, currency),
        CONSTRAINT chk_account_balance_no_negative CHECK (
            account_class NOT IN
                ('AR','CASH_CLEARING','UNALLOCATED','CONTRACT_LIABILITY','DISPUTE_HOLD','REFUND_CLEARING')
            OR balance_minor >= 0)
    )",
    "CREATE TABLE bss.ledger_ar_invoice_balance (
        tenant_id          uuid        NOT NULL,
        payer_tenant_id    uuid        NOT NULL,
        account_id         uuid        NOT NULL,
        invoice_id         varchar(128) NOT NULL,
        currency           varchar(16) NOT NULL,
        balance_minor      bigint      NOT NULL DEFAULT 0,
        original_posted_at timestamptz,
        due_date           date,
        last_entry_seq     bigint,
        version            bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, account_id, invoice_id),
        CONSTRAINT chk_ar_invoice_balance_no_negative CHECK (balance_minor >= 0)
    )",
    "CREATE TABLE bss.ledger_ar_payer_balance (
        tenant_id       uuid        NOT NULL,
        payer_tenant_id uuid        NOT NULL,
        account_id      uuid        NOT NULL,
        currency        varchar(16) NOT NULL,
        balance_minor   bigint      NOT NULL DEFAULT 0,
        last_entry_seq  bigint,
        version         bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, account_id, currency),
        CONSTRAINT chk_ar_payer_balance_no_negative CHECK (balance_minor >= 0)
    )",
    "CREATE TABLE bss.ledger_tax_subbalance (
        tenant_id         uuid         NOT NULL,
        account_id        uuid         NOT NULL,
        tax_jurisdiction  varchar(128) NOT NULL,
        tax_filing_period varchar(32)  NOT NULL,
        balance_minor     bigint       NOT NULL DEFAULT 0,
        last_entry_seq    bigint,
        version           bigint       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, account_id, tax_jurisdiction, tax_filing_period)
    )",
    "CREATE TABLE bss.ledger_unallocated_balance (
        tenant_id               uuid        NOT NULL,
        payer_tenant_id         uuid        NOT NULL,
        account_id              uuid        NOT NULL,
        currency                varchar(16) NOT NULL,
        balance_minor           bigint      NOT NULL DEFAULT 0,
        functional_balance_minor bigint,
        functional_currency     varchar(16),
        last_entry_seq          bigint,
        version                 bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, currency),
        CONSTRAINT chk_unallocated_balance_no_negative CHECK (balance_minor >= 0)
    )",
    "CREATE TABLE bss.ledger_reusable_credit_subbalance (
        tenant_id               uuid        NOT NULL,
        payer_tenant_id         uuid        NOT NULL,
        account_id              uuid        NOT NULL,
        currency                varchar(16) NOT NULL,
        credit_grant_event_type text        NOT NULL,
        first_granted_at        timestamptz,
        balance_minor           bigint      NOT NULL DEFAULT 0,
        functional_balance_minor bigint,
        functional_currency     varchar(16),
        last_entry_seq          bigint,
        version                 bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, currency, credit_grant_event_type),
        CONSTRAINT chk_reusable_credit_subbalance_no_negative CHECK (balance_minor >= 0)
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_reusable_credit_subbalance",
    "DROP TABLE IF EXISTS bss.ledger_unallocated_balance",
    "DROP TABLE IF EXISTS bss.ledger_tax_subbalance",
    "DROP TABLE IF EXISTS bss.ledger_ar_payer_balance",
    "DROP TABLE IF EXISTS bss.ledger_ar_invoice_balance",
    "DROP TABLE IF EXISTS bss.ledger_account_balance",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PKs preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_account_balance (
        tenant_id      text        NOT NULL,
        account_id     text        NOT NULL,
        currency       varchar(16) NOT NULL,
        account_class  text        NOT NULL,
        normal_side    text        NOT NULL CHECK (normal_side IN ('DR','CR')),
        balance_minor  bigint      NOT NULL DEFAULT 0,
        last_entry_seq bigint,
        version        bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, account_id, currency),
        CONSTRAINT chk_account_balance_no_negative CHECK (
            account_class NOT IN
                ('AR','CASH_CLEARING','UNALLOCATED','CONTRACT_LIABILITY','DISPUTE_HOLD','REFUND_CLEARING')
            OR balance_minor >= 0)
    )",
    "CREATE TABLE ledger_ar_invoice_balance (
        tenant_id          text        NOT NULL,
        payer_tenant_id    text        NOT NULL,
        account_id         text        NOT NULL,
        invoice_id         varchar(128) NOT NULL,
        currency           varchar(16) NOT NULL,
        balance_minor      bigint      NOT NULL DEFAULT 0,
        original_posted_at text,
        due_date           date,
        last_entry_seq     bigint,
        version            bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, account_id, invoice_id),
        CONSTRAINT chk_ar_invoice_balance_no_negative CHECK (balance_minor >= 0)
    )",
    "CREATE TABLE ledger_ar_payer_balance (
        tenant_id       text        NOT NULL,
        payer_tenant_id text        NOT NULL,
        account_id      text        NOT NULL,
        currency        varchar(16) NOT NULL,
        balance_minor   bigint      NOT NULL DEFAULT 0,
        last_entry_seq  bigint,
        version         bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, account_id, currency),
        CONSTRAINT chk_ar_payer_balance_no_negative CHECK (balance_minor >= 0)
    )",
    "CREATE TABLE ledger_tax_subbalance (
        tenant_id         text         NOT NULL,
        account_id        text         NOT NULL,
        tax_jurisdiction  varchar(128) NOT NULL,
        tax_filing_period varchar(32)  NOT NULL,
        balance_minor     bigint       NOT NULL DEFAULT 0,
        last_entry_seq    bigint,
        version           bigint       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, account_id, tax_jurisdiction, tax_filing_period)
    )",
    "CREATE TABLE ledger_unallocated_balance (
        tenant_id               text        NOT NULL,
        payer_tenant_id         text        NOT NULL,
        account_id              text        NOT NULL,
        currency                varchar(16) NOT NULL,
        balance_minor           bigint      NOT NULL DEFAULT 0,
        functional_balance_minor bigint,
        functional_currency     varchar(16),
        last_entry_seq          bigint,
        version                 bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, currency),
        CONSTRAINT chk_unallocated_balance_no_negative CHECK (balance_minor >= 0)
    )",
    "CREATE TABLE ledger_reusable_credit_subbalance (
        tenant_id               text        NOT NULL,
        payer_tenant_id         text        NOT NULL,
        account_id              text        NOT NULL,
        currency                varchar(16) NOT NULL,
        credit_grant_event_type text        NOT NULL,
        first_granted_at        text,
        balance_minor           bigint      NOT NULL DEFAULT 0,
        functional_balance_minor bigint,
        functional_currency     varchar(16),
        last_entry_seq          bigint,
        version                 bigint      NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payer_tenant_id, currency, credit_grant_event_type),
        CONSTRAINT chk_reusable_credit_subbalance_no_negative CHECK (balance_minor >= 0)
    )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_reusable_credit_subbalance",
    "DROP TABLE IF EXISTS ledger_unallocated_balance",
    "DROP TABLE IF EXISTS ledger_tax_subbalance",
    "DROP TABLE IF EXISTS ledger_ar_payer_balance",
    "DROP TABLE IF EXISTS ledger_ar_invoice_balance",
    "DROP TABLE IF EXISTS ledger_account_balance",
];

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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use bss_ledger_sdk::AccountClass;

    use super::PG_UP_STATEMENTS;

    /// Parse the single-quoted literals from the `account_class NOT IN ( … )`
    /// clause of the `account_balance` no-negative CHECK.
    fn guarded_check_literals() -> Vec<String> {
        let key = "account_class NOT IN";
        let stmt = PG_UP_STATEMENTS
            .iter()
            .find(|s| s.contains(key))
            .expect("account_balance up-statement defines the no-negative CHECK");
        let after = &stmt[stmt.find(key).expect("key present") + key.len()..];
        let open = after.find('(').expect("NOT IN list opens");
        let close = after.find(')').expect("NOT IN list closes");
        let mut v: Vec<String> = after[open + 1..close]
            .split(',')
            .map(|t| t.trim().trim_matches('\'').to_owned())
            .collect();
        v.sort();
        v
    }

    /// Anti-drift: the no-negative CHECK's guarded-class list must equal the
    /// single source of truth `AccountClass::GUARDED` (the bug this pins:
    /// the tie-out backstop once duplicated this set and inverted it).
    #[test]
    fn no_negative_check_matches_guarded_set() {
        let mut expected: Vec<String> = AccountClass::GUARDED
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect();
        expected.sort();
        assert_eq!(
            guarded_check_literals(),
            expected,
            "chk_account_balance_no_negative drifted from AccountClass::GUARDED"
        );
    }
}
