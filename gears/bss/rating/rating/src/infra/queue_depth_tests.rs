#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::OutboxQueueDepth;
use crate::domain::usage::reader::QueueDepth;
use crate::test_support::migrated_db;

#[tokio::test]
async fn an_unused_queue_has_no_depth() {
    let depth = OutboxQueueDepth::new(migrated_db().await);
    assert_eq!(depth.child_work_depth().await.unwrap(), 0);
}
