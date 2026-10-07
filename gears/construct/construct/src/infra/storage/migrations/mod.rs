use sea_orm_migration::prelude::*;

pub mod initial_001;
pub mod m002_subject_settings;
pub mod m003_record_ids;

pub struct Migrator;

/// The runner applies migrations in the order of their names, not of this
/// list, so each name must sort after the one before it: `initial_001`, then
/// `mNNN_<what>`.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(initial_001::Migration),
            Box::new(m002_subject_settings::Migration),
            Box::new(m003_record_ids::Migration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use sea_orm_migration::MigratorTrait;

    use super::Migrator;

    #[test]
    fn the_names_sort_in_the_order_the_migrations_must_run() {
        let names: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
