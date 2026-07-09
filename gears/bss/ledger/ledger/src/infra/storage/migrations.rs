//! Migration set for the bss-ledger gear (schema `bss`). Greenfield
//! chain; migrations are added by subsequent tasks.

pub mod m20260619_000001_create_bss_schema;
pub mod m20260619_000002_create_journal_tables;
pub mod m20260619_000003_create_balance_caches;
pub mod m20260619_000004_create_idempotency_and_reference;
pub mod m20260619_000005_create_fiscal_calendar;
pub mod m20260622_000006_create_payment_tables;
pub mod m20260623_000007_create_precedence_policy;
pub mod m20260623_000008_create_pending_event_queue;
pub mod m20260623_000009_add_ar_status;
pub mod m20260623_000010_create_dispute;
pub mod m20260624_000011_create_chain_state;
pub mod m20260624_000011_create_recognition_tables;
pub mod m20260624_000012_create_dual_control_tables;
pub mod m20260624_000012_relax_journal_entry_trigger;
pub mod m20260624_000013_create_scope_freeze;
pub mod m20260624_000014_create_secured_audit;
pub mod m20260624_000015_create_entry_annotation;
pub mod m20260624_000016_create_payer_pii_map;
pub mod m20260624_000017_create_chain_checkpoint;
pub mod m20260624_000018_create_audit_pack_export;
pub mod m20260625_000013_dual_control_approving_state;
pub mod m20260626_000019_create_invoice_exposure;
pub mod m20260626_000020_create_credit_note;
pub mod m20260626_000021_create_debit_note;
pub mod m20260626_000022_create_refund;
pub mod m20260626_000023_refund_approval_kind;
pub mod m20260626_000024_manual_adjustment_approval_kind;
pub mod m20260626_000025_note_approval_kinds;
pub mod m20260627_000026_create_fx_rate_tables;
pub mod m20260627_000027_journal_line_rate_ref;
pub mod m20260627_000028_wide_cache_functional_cols;
pub mod m20260627_000029_dual_column_commit_check;
pub mod m20260627_000030_fiscal_calendar_functional_ccy;
pub mod m20260628_000031_cache_functional_consistency;
pub mod m20260628_000032_snapshot_identity_rate_micro;
pub mod m20260628_000033_create_period_close;
pub mod m20260628_000034_create_exception_queue;
pub mod m20260628_000035_create_reconciliation_run;
pub mod m20260628_000036_exception_queue_open_uniq;
pub mod m20260629_000037_create_posting_policy;
pub mod m20260629_000038_create_verified_balance;
pub mod m20260629_000039_create_fx_revaluation_run;
pub mod m20260630_000040_create_fx_revaluation_mode;
pub mod m20260706_000041_currency_scale_immutable;

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260619_000001_create_bss_schema::Migration),
            Box::new(m20260619_000002_create_journal_tables::Migration),
            Box::new(m20260619_000003_create_balance_caches::Migration),
            Box::new(m20260619_000004_create_idempotency_and_reference::Migration),
            Box::new(m20260619_000005_create_fiscal_calendar::Migration),
            Box::new(m20260622_000006_create_payment_tables::Migration),
            Box::new(m20260623_000007_create_precedence_policy::Migration),
            Box::new(m20260623_000008_create_pending_event_queue::Migration),
            Box::new(m20260623_000009_add_ar_status::Migration),
            Box::new(m20260623_000010_create_dispute::Migration),
            Box::new(m20260624_000011_create_recognition_tables::Migration),
            Box::new(m20260624_000012_create_dual_control_tables::Migration),
            Box::new(m20260625_000013_dual_control_approving_state::Migration),
            Box::new(m20260626_000019_create_invoice_exposure::Migration),
            Box::new(m20260626_000020_create_credit_note::Migration),
            Box::new(m20260626_000021_create_debit_note::Migration),
            Box::new(m20260626_000022_create_refund::Migration),
            Box::new(m20260626_000023_refund_approval_kind::Migration),
            Box::new(m20260626_000024_manual_adjustment_approval_kind::Migration),
            Box::new(m20260626_000025_note_approval_kinds::Migration),
            // Shared `coord_leases` table (the single-active recognition-run
            // lease). Owned by the `coord` crate. Qualified into `bss` (like the
            // gear's other domain DDL) so it lands in `bss` regardless of the
            // connection's `search_path` order. NOTE: the toolkit migration runner
            // applies migrations in NAME order, so this `m0001_…` name actually
            // sorts FIRST — before `…000001_create_bss_schema`; coord's `in_schema`
            // `up` therefore runs `CREATE SCHEMA IF NOT EXISTS bss` itself before
            // the `CREATE TABLE`, so the qualification is safe despite running
            // first (and idempotent with the schema migration that follows).
            Box::new(coord::migration::Migration::in_schema("bss")),
            Box::new(m20260624_000011_create_chain_state::Migration),
            Box::new(m20260624_000012_relax_journal_entry_trigger::Migration),
            Box::new(m20260624_000013_create_scope_freeze::Migration),
            Box::new(m20260624_000014_create_secured_audit::Migration),
            Box::new(m20260624_000015_create_entry_annotation::Migration),
            Box::new(m20260624_000016_create_payer_pii_map::Migration),
            Box::new(m20260624_000017_create_chain_checkpoint::Migration),
            Box::new(m20260624_000018_create_audit_pack_export::Migration),
            // --- Slice 5 (FX & multi-currency) substrate, appended at the Vec
            // end (positions 30–34) so the down-magic counts in
            // postgres_migration_idempotency.rs shift by the +5 only. ---
            Box::new(m20260627_000026_create_fx_rate_tables::Migration),
            Box::new(m20260627_000027_journal_line_rate_ref::Migration),
            Box::new(m20260627_000028_wide_cache_functional_cols::Migration),
            Box::new(m20260627_000029_dual_column_commit_check::Migration),
            // S5-F3: the legal-entity functional-currency source (rides the
            // existing per-LE fiscal-calendar row).
            Box::new(m20260627_000030_fiscal_calendar_functional_ccy::Migration),
            Box::new(m20260628_000031_cache_functional_consistency::Migration),
            Box::new(m20260628_000032_snapshot_identity_rate_micro::Migration),
            // --- Slice 7 (reconciliation & period-close) substrate, appended at the
            // Vec end (after the Slice-5 remediation migrations) so the down-magic
            // counts in postgres_migration_idempotency.rs shift by the +4 only. ---
            Box::new(m20260628_000033_create_period_close::Migration),
            Box::new(m20260628_000034_create_exception_queue::Migration),
            Box::new(m20260628_000035_create_reconciliation_run::Migration),
            // Remediation: enforce single-OPEN-row dedup at the DB level.
            Box::new(m20260628_000036_exception_queue_open_uniq::Migration),
            // VHP-1853: tenant-configurable invoice-posting policies (missing-mapping
            // mode + AR-aging buckets), appended at the Vec end so the down-magic
            // counts in postgres_migration_idempotency.rs shift by +1 only.
            Box::new(m20260629_000037_create_posting_policy::Migration),
            // VHP-1843: incremental tie-out baseline (ledger_verified_balance),
            // appended at the Vec end so the down-magic counts in
            // postgres_migration_idempotency.rs shift by +1 only.
            Box::new(m20260629_000038_create_verified_balance::Migration),
            // VHP-1859 review C3: Mode-B FX-revaluation completion marker
            // (ledger_fx_revaluation_run), appended at the Vec end so the
            // down-magic counts in postgres_migration_idempotency.rs shift by +1.
            Box::new(m20260629_000039_create_fx_revaluation_run::Migration),
            // VHP-1986 per-tenant FX revaluation-mode table, appended at the Vec
            // end so the down-magic counts in postgres_migration_idempotency.rs
            // shift by +1 (43 → 44 applied; before-payment / before-queue downs +1).
            Box::new(m20260630_000040_create_fx_revaluation_mode::Migration),
            // Defense-in-depth: DB trigger enforcing currency-scale immutability
            // once a posting exists (design §3.7). Appended at the Vec end so the
            // down-magic counts in postgres_migration_idempotency.rs shift by +1.
            Box::new(m20260706_000041_currency_scale_immutable::Migration),
        ]
    }
}
