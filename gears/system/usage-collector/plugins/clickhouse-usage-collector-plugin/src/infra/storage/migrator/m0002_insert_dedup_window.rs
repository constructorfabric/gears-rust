//! `m0002_insert_dedup_window`: retrofit the insert dedup window onto a
//! `usage_records` created before `m0001_init` declared it.

use async_trait::async_trait;

use super::{ChMigration, SchemaManager};

pub(super) struct M0002InsertDedupWindow;

#[async_trait]
impl ChMigration for M0002InsertDedupWindow {
    fn name(&self) -> &'static str {
        "m0002_insert_dedup_window"
    }

    /// A no-op on a fresh table, and on a cluster (`SchemaManager::
    /// ensure_insert_dedup_window`).
    async fn up(&self, m: &SchemaManager<'_>) -> anyhow::Result<()> {
        m.ensure_insert_dedup_window().await
    }
}
