use sea_orm_migration::prelude::*;

pub mod initial_001;
pub mod named_settings_002;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(initial_001::Migration),
            Box::new(named_settings_002::Migration),
        ]
    }
}
