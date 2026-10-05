//! Compare actual pre/post rebuild DDL, including unnamed CHECKs and auto-index UNIQUEs.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_pricing::module::BssPricingGear;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::run_migrations_for_testing;

// Ignore identifier quoting and formatting, but retain literal contents and every old clause.
fn clauses(sql: &str) -> Vec<String> {
    let mut quoted = false;
    let compact: String = sql
        .chars()
        .filter(|c| {
            if *c == '\'' {
                quoted = !quoted;
            }
            quoted || (!c.is_whitespace() && *c != '"')
        })
        .collect();
    let body = &compact[compact.find('(').unwrap() + 1..compact.rfind(')').unwrap()];
    let mut depth = 0;
    let mut quoted = false;
    let mut start = 0;
    let mut result = Vec::new();
    for (i, c) in body.char_indices() {
        if c == '\'' {
            quoted = !quoted;
        }
        if !quoted {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    result.push(body[start..i].to_owned());
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    result.push(body[start..].to_owned());
    result.sort();
    result
}
async fn ddl(raw: &sea_orm::DatabaseConnection, table: &str) -> Vec<String> {
    let row = raw
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?",
            [table.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    clauses(&row.try_get::<String>("", "sql").unwrap())
}
#[tokio::test]
async fn review_migration_18_preserves_every_original_clause() {
    let dsn = crate::plan_support::entry_support::TestDsn::new("pricing-ddl-review-");
    let db = crate::seam_parity_support::open(&dsn).await;
    let prior = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| {
            let name = m.name();
            !(name.contains("000018")
                || name.contains("000019")
                || name.contains("000020")
                || name.contains("000021")
                || name.contains("000022")
                || name.contains("000023"))
        })
        .collect();
    run_migrations_for_testing(&db.db(), prior).await.unwrap();
    let raw = Database::connect(&dsn).await.unwrap();
    let tables = [
        "pricing_price_book_entry",
        "pricing_price",
        "pricing_plan_item",
    ];
    let mut before = Vec::new();
    for table in tables {
        before.push(ddl(&raw, table).await);
    }
    let migration = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| m.name().contains("000018"))
        .collect();
    run_migrations_for_testing(&db.db(), migration)
        .await
        .unwrap();
    let additions = clauses(
        "CREATE TABLE added (usage_policy_id text, usage_policy_version integer, usage_policy_digest text, CONSTRAINT pricing_entry_policy_complete CHECK ((usage_policy_id IS NULL AND usage_policy_version IS NULL AND usage_policy_digest IS NULL) OR (usage_policy_id IS NOT NULL AND usage_policy_version IS NOT NULL AND usage_policy_digest IS NOT NULL AND charge_kind = 'usage')), FOREIGN KEY (tenant_id, usage_policy_id, usage_policy_version, usage_policy_digest) REFERENCES pricing_usage_rating_policy (tenant_id, policy_id, version, digest))",
    );
    for (table, old) in tables.into_iter().zip(before) {
        let mut after = ddl(&raw, table).await;
        if table == "pricing_price_book_entry" {
            for addition in &additions {
                let index = after
                    .iter()
                    .position(|clause| clause == addition)
                    .expect("declared new policy clause exists");
                after.remove(index);
            }
        }
        assert_eq!(
            after, old,
            "migration 18 must preserve every original clause of {table}"
        );
    }
}
