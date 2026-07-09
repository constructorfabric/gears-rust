//! Anti-drift: the `event_type` literal set is maintained independently in the
//! [`AuditEventType::ALL`] catalogue and in this migration's raw-SQL `CHECK`
//! constraint. A variant added to one side but not the other would let the DB
//! reject a "valid" enum token or accept an unlisted one. This test pins the
//! two sets equal by parsing the literals straight out of the migration SQL.

#![allow(clippy::panic, clippy::expect_used)]

use crate::infra::audit::event_type::AuditEventType;

use super::PG_UP_STATEMENTS;

/// Extract the single-quoted literals from the first `event_type IN ( … )`
/// clause across the Postgres up-statements (the `CHECK` constraint).
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
fn event_type_enum_matches_migration_check() {
    assert_eq!(
        check_literals("event_type"),
        sorted(AuditEventType::ALL),
        "AuditEventType::ALL drifted from chk_secured_audit_event_type"
    );
}
