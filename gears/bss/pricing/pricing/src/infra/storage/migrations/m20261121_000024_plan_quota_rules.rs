//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-store:p1
//! D-546: a revision's quota rules and the set row that carries their digest (D-541).
//!
//! A forward migration that creates two tables and one index; it changes no existing table.
//!
//! - `pricing_plan_quota_set`: one row per revision that holds rules, keyed by the revision. It
//!   holds the set digest, computed once when the set is written (64 lowercase hex characters),
//!   the number of rules (1 to 200), the last writer and the time. A replace with no rules deletes
//!   the row, so a revision without rules has none and reads the empty set and the empty set's
//!   digest.
//! - `pricing_plan_quota_rule`: one row per rule, keyed `(revision_id, rule_id)`, with foreign
//!   keys to its revision and to its item and no cascade, so the delete of an item that a rule
//!   names fails in the database. It keeps the exact Products definition version (id, version text
//!   of 1 to 20 characters, 64-character digest), the dimension value, the binding key (1 to 64
//!   characters), the subject selector (`resource_owner`), the scope selection, the amount (1 to
//!   39 characters), the unit, the basis (`fixed` or `per_plan_unit`), the definition's evidence
//!   and the availability observation. JSON is text, as `pricing_acceptance.receipt_json` is. The
//!   index `pricing_plan_quota_rule_item` serves the item delete's check.
//!
//! No trigger guards the rules: the draft fence is the conditional revision write (D-543), as for
//! items. Both engines get the same tables, CHECKs, keys and index; `SQLite` stores uuid and
//! timestamptz as text.
//!
//! **A later `SQLite` rebuild of `pricing_plan_item` or `pricing_plan_revision`** (the shape of
//! `m20261003_000023`) must carry `pricing_plan_quota_rule` and `pricing_plan_quota_set`, their
//! children: a rebuild that drops the parent fails on their rows, or loses them.
//!
//! `down` refuses while a rule row exists, naming the table: reverting would lose the stored rules.
//! On an empty rule table it drops the rule table, then the set table; a second `down` finds
//! neither and changes nothing.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss.pricing_plan_quota_set (
  revision_id uuid PRIMARY KEY REFERENCES bss.pricing_plan_revision(id), tenant_id uuid NOT NULL,
  quota_rules_digest text NOT NULL, rule_count integer NOT NULL, updated_by uuid NOT NULL,
  updated_at timestamptz NOT NULL,
  CONSTRAINT chk_pricing_plan_quota_set_count CHECK (rule_count BETWEEN 1 AND 200),
  CONSTRAINT chk_pricing_plan_quota_set_digest CHECK (length(quota_rules_digest) = 64)
)",
    r"CREATE TABLE IF NOT EXISTS bss.pricing_plan_quota_rule (
  revision_id uuid NOT NULL REFERENCES bss.pricing_plan_revision(id), rule_id uuid NOT NULL,
  tenant_id uuid NOT NULL, item_id uuid NOT NULL REFERENCES bss.pricing_plan_item(id),
  definition_id uuid NOT NULL, definition_version text NOT NULL, definition_digest text NOT NULL,
  dimension_value text, logical_binding_key text NOT NULL, subject_selector text NOT NULL,
  scope_selection_json text NOT NULL, amount text NOT NULL, unit_ref text NOT NULL,
  basis text NOT NULL, evidence_json text NOT NULL, availability_json text NOT NULL,
  created_at timestamptz NOT NULL,
  PRIMARY KEY (revision_id, rule_id),
  CONSTRAINT chk_pricing_plan_quota_rule_basis CHECK (basis IN ('fixed','per_plan_unit')),
  CONSTRAINT chk_pricing_plan_quota_rule_subject CHECK (subject_selector = 'resource_owner'),
  CONSTRAINT chk_pricing_plan_quota_rule_key CHECK (length(logical_binding_key) BETWEEN 1 AND 64),
  CONSTRAINT chk_pricing_plan_quota_rule_digest CHECK (length(definition_digest) = 64),
  CONSTRAINT chk_pricing_plan_quota_rule_amount CHECK (length(amount) BETWEEN 1 AND 39),
  CONSTRAINT chk_pricing_plan_quota_rule_version CHECK (length(definition_version) BETWEEN 1 AND 20)
)",
    r"CREATE INDEX IF NOT EXISTS pricing_plan_quota_rule_item
  ON bss.pricing_plan_quota_rule (item_id)",
];

const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS pricing_plan_quota_set (
  revision_id text NOT NULL PRIMARY KEY REFERENCES pricing_plan_revision(id),
  tenant_id text NOT NULL,
  quota_rules_digest text NOT NULL, rule_count integer NOT NULL, updated_by text NOT NULL,
  updated_at text NOT NULL,
  CONSTRAINT chk_pricing_plan_quota_set_count CHECK (rule_count BETWEEN 1 AND 200),
  CONSTRAINT chk_pricing_plan_quota_set_digest CHECK (length(quota_rules_digest) = 64)
)",
    r"CREATE TABLE IF NOT EXISTS pricing_plan_quota_rule (
  revision_id text NOT NULL REFERENCES pricing_plan_revision(id), rule_id text NOT NULL,
  tenant_id text NOT NULL, item_id text NOT NULL REFERENCES pricing_plan_item(id),
  definition_id text NOT NULL, definition_version text NOT NULL, definition_digest text NOT NULL,
  dimension_value text, logical_binding_key text NOT NULL, subject_selector text NOT NULL,
  scope_selection_json text NOT NULL, amount text NOT NULL, unit_ref text NOT NULL,
  basis text NOT NULL, evidence_json text NOT NULL, availability_json text NOT NULL,
  created_at text NOT NULL,
  PRIMARY KEY (revision_id, rule_id),
  CONSTRAINT chk_pricing_plan_quota_rule_basis CHECK (basis IN ('fixed','per_plan_unit')),
  CONSTRAINT chk_pricing_plan_quota_rule_subject CHECK (subject_selector = 'resource_owner'),
  CONSTRAINT chk_pricing_plan_quota_rule_key CHECK (length(logical_binding_key) BETWEEN 1 AND 64),
  CONSTRAINT chk_pricing_plan_quota_rule_digest CHECK (length(definition_digest) = 64),
  CONSTRAINT chk_pricing_plan_quota_rule_amount CHECK (length(amount) BETWEEN 1 AND 39),
  CONSTRAINT chk_pricing_plan_quota_rule_version CHECK (length(definition_version) BETWEEN 1 AND 20)
)",
    r"CREATE INDEX IF NOT EXISTS pricing_plan_quota_rule_item
  ON pricing_plan_quota_rule (item_id)",
];

/// The rule table first: it is the set table's sibling, and both are children of the revision.
const PG_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_plan_quota_rule",
    "DROP TABLE IF EXISTS bss.pricing_plan_quota_set",
];
const SQLITE_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS pricing_plan_quota_rule",
    "DROP TABLE IF EXISTS pricing_plan_quota_set",
];

/// The table whose rows `down` refuses to lose.
const RULES: &str = "pricing_plan_quota_rule";

/// Refuses while the rule table exists and holds a row, naming it; a table that does not exist (a
/// second `down`) holds none.
async fn refuse_while_rules(migration: &str, manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let backend = manager.get_database_backend();
    let conn = manager.get_connection();
    let (exists, qualified) = match backend {
        DbBackend::Postgres => (
            format!("SELECT to_regclass('bss.{RULES}') IS NOT NULL AS present"),
            format!("bss.{RULES}"),
        ),
        DbBackend::Sqlite => (
            format!(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = \
                 '{RULES}') AS present"
            ),
            RULES.to_owned(),
        ),
        _ => {
            return Err(DbErr::Migration(format!(
                "{backend:?} is not a supported backend for bss-pricing"
            )));
        }
    };
    let present = conn
        .query_one_raw(Statement::from_string(backend, exists))
        .await?
        .map(|row| row.try_get::<bool>("", "present"))
        .transpose()?
        .unwrap_or(false);
    if !present {
        return Ok(());
    }
    let held = conn
        .query_one_raw(Statement::from_string(
            backend,
            format!("SELECT 1 AS held FROM {qualified} LIMIT 1"),
        ))
        .await?;
    if held.is_none() {
        return Ok(());
    }
    Err(DbErr::Migration(format!(
        "{migration}: down refused: {RULES} holds rows; reverting would lose the plans' stored \
         quota rules, so it runs only on an empty rule table"
    )))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }

    /// Refuses while a rule row exists; on an empty rule table drops the rule table, then the set
    /// table.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        refuse_while_rules(self.name(), manager).await?;
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}

#[cfg(test)]
#[path = "m20261121_000024_plan_quota_rules_tests.rs"]
mod tests;
