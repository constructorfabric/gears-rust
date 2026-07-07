//! Anti-drift: the `account_class` / `source_doc_type` literal sets are
//! maintained independently in the SDK enums (`AccountClass::ALL` /
//! `SourceDocType::ALL`) and in this migration's raw-SQL `CHECK` constraints.
//! A variant added to one side but not the other would let the DB reject a
//! "valid" enum value or accept an unlisted one. These tests pin the two
//! sets equal by parsing the literals straight out of the migration SQL.

#![allow(clippy::panic, clippy::expect_used)]

use bss_ledger_sdk::{AccountClass, SourceDocType};

use super::PG_UP_STATEMENTS;

/// Extract the single-quoted literals from the first `<col> IN ( … )` clause
/// across the Postgres up-statements (the `CHECK` constraint for `col`).
fn check_literals(col: &str) -> Vec<String> {
    let key = format!("{col} IN (");
    let stmt = PG_UP_STATEMENTS
        .iter()
        .find(|s| s.contains(&key))
        .unwrap_or_else(|| panic!("no PG up-statement defines a CHECK on `{col}`"));
    let start = stmt.find(&key).expect("key present") + key.len();
    let rest = &stmt[start..];
    let end = rest.find(')').expect("CHECK IN list must be closed");
    let mut literals: Vec<String> = rest[..end]
        .split(',')
        .map(|t| t.trim().trim_matches('\'').to_owned())
        .collect();
    literals.sort();
    literals
}

fn sorted(all: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = all.iter().map(|s| (*s).to_owned()).collect();
    v.sort();
    v
}

#[test]
fn account_class_enum_matches_migration_check() {
    assert_eq!(
        check_literals("account_class"),
        sorted(AccountClass::ALL),
        "AccountClass::ALL drifted from chk_journal_line_account_class"
    );
}

#[test]
fn source_doc_type_enum_matches_migration_check() {
    assert_eq!(
        check_literals("source_doc_type"),
        sorted(SourceDocType::ALL),
        "SourceDocType::ALL drifted from chk_journal_entry_source_doc_type"
    );
}
