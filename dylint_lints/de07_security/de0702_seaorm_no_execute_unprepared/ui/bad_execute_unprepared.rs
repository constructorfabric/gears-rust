// Test file for DE0702: execute_unprepared detection
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unused_variables)]

use sea_orm::ConnectionTrait;
use sea_orm::DbBackend;

// Simulate execute_unprepared — in real code this comes from ConnectionTrait
struct FakeConn;

impl FakeConn {
    async fn execute_unprepared(&self, _sql: &str) -> Result<(), ()> {
        Ok(())
    }
}

/// Raw SQL via `execute_unprepared()` — should trigger DE0702.
async fn bad_execute_unprepared() {
    let conn = FakeConn;
    // Should trigger DE0702 - raw SQL
    conn.execute_unprepared("DELETE FROM users WHERE id = 1").await.ok();
}

/// Test entry point.
fn main() {}
