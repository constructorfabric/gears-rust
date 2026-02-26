// Test file for DE0702: Statement::from_string detection
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unused_variables)]

use sea_orm::{ConnectionTrait, DbBackend, Statement};

/// Raw SQL via `Statement::from_string()` — should trigger DE0702.
async fn bad_statement_from_string() {
    // Should trigger DE0702 - raw SQL
    let _stmt = Statement::from_string(DbBackend::Postgres, "SELECT * FROM users");
}

/// Test entry point.
fn main() {}
