use sea_orm_migration::prelude::*;

mod m20260302_000001_initial;
mod m20260329_000001_add_last_progress_at;
mod m20260330_000001_thread_summary_composite_frontier;
mod m20260330_000002_add_web_search_enabled_to_turns;
mod m20260402_000003_add_tool_counts_to_turns;
mod m20260417_000004_add_secondary_upload_fields;
mod m20260512_000005_add_file_search_count;
mod m20260927_000006_add_stale_upload_index;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260302_000001_initial::Migration),
            Box::new(m20260329_000001_add_last_progress_at::Migration),
            Box::new(m20260330_000002_add_web_search_enabled_to_turns::Migration),
            Box::new(m20260330_000001_thread_summary_composite_frontier::Migration),
            Box::new(m20260402_000003_add_tool_counts_to_turns::Migration),
            Box::new(m20260417_000004_add_secondary_upload_fields::Migration),
            Box::new(m20260512_000005_add_file_search_count::Migration),
            Box::new(m20260927_000006_add_stale_upload_index::Migration),
        ]
    }
}
