pub use sea_orm_migration::prelude::*;

pub(crate) mod m20260417_000001_create_session_tables;
pub(crate) mod m20260417_000002_create_messages_table;
pub(crate) mod m20260417_000003_create_plugin_configs_table;
pub(crate) mod m20260417_000004_create_message_reactions_table;
pub(crate) mod m20260417_000005_create_messages_fts_index;

pub use m20260417_000002_create_messages_table::UQ_VARIANT_INDEX;
pub use m20260417_000005_create_messages_fts_index::MESSAGES_FTS_INDEX;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260417_000001_create_session_tables::Migration),
            Box::new(m20260417_000002_create_messages_table::Migration),
            Box::new(m20260417_000003_create_plugin_configs_table::Migration),
            Box::new(m20260417_000004_create_message_reactions_table::Migration),
            Box::new(m20260417_000005_create_messages_fts_index::Migration),
        ]
    }
}
