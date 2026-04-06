use super::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DatabaseBackend;

#[test]
fn test_sanitize_module_name() {
    assert_eq!(sanitize_module_name("my_module"), "my_module");
    assert_eq!(sanitize_module_name("my-module"), "my_module");
    assert_eq!(sanitize_module_name("MyModule123"), "MyModule123");
    assert_eq!(sanitize_module_name("my.module"), "my_module");
    assert_eq!(sanitize_module_name("my/module"), "my_module");
    assert_eq!(sanitize_module_name(""), "_");
}

#[test]
fn test_migration_table_name() {
    let users_info_table_1 = migration_table_name("users-info");
    let users_info_table_2 = migration_table_name("users-info");
    assert_eq!(users_info_table_1, users_info_table_2, "deterministic");
    assert!(users_info_table_1.starts_with("modkit_migrations__"));
    assert!(users_info_table_1.len() <= 63);

    let simple_settings_table = migration_table_name("simple-user-settings");
    assert!(simple_settings_table.contains("simple_user_settings"));
    assert!(simple_settings_table.len() <= 63);
}

#[allow(dead_code)]
struct TestMigration {
    name: String,
}

impl MigrationName for TestMigration {
    fn name(&self) -> &str {
        &self.name
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TestMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let table_name = format!("test_{}", self.name.replace('-', "_"));

        let sql = match backend {
            DatabaseBackend::Sqlite => {
                format!("CREATE TABLE IF NOT EXISTS \"{table_name}\" (id INTEGER PRIMARY KEY)")
            }
            DatabaseBackend::Postgres => {
                format!("CREATE TABLE IF NOT EXISTS \"{table_name}\" (id SERIAL PRIMARY KEY)")
            }
            DatabaseBackend::MySql => format!(
                "CREATE TABLE IF NOT EXISTS `{table_name}` (id INT AUTO_INCREMENT PRIMARY KEY)"
            ),
        };

        manager
            .get_connection()
            .execute(Statement::from_string(backend, sql))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
mod sqlite_tests {
    use super::*;
    use crate::{ConnectOpts, Db, connect_db};

    async fn setup_test_db() -> Db {
        connect_db("sqlite::memory:", ConnectOpts::default())
            .await
            .expect("Failed to create test database")
    }

    #[tokio::test]
    async fn test_run_module_migrations_empty() {
        let db = setup_test_db().await;

        let result = run_migrations_for_module(&db, "test_module", vec![])
            .await
            .expect("Migration should succeed");

        assert_eq!(result.applied, 0);
        assert_eq!(result.skipped, 0);
        assert!(result.applied_names.is_empty());
    }

    #[tokio::test]
    async fn test_run_module_migrations_single() {
        let db = setup_test_db().await;

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_initial".to_owned(),
        })];

        let result = run_migrations_for_module(&db, "test_module_single", migrations)
            .await
            .expect("Migration should succeed");

        assert_eq!(result.applied, 1);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.applied_names, vec!["m001_initial"]);
    }

    #[tokio::test]
    async fn test_run_module_migrations_idempotent() {
        let db = setup_test_db().await;

        let module_name = "test_module_idempotent";

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_initial".to_owned(),
        })];

        let result1 = run_migrations_for_module(&db, module_name, migrations)
            .await
            .expect("First migration run should succeed");

        assert_eq!(result1.applied, 1);

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_initial".to_owned(),
        })];

        let result2 = run_migrations_for_module(&db, module_name, migrations)
            .await
            .expect("Second migration run should succeed");

        assert_eq!(result2.applied, 0);
        assert_eq!(result2.skipped, 1);
    }

    #[tokio::test]
    async fn test_run_module_migrations_deterministic_ordering() {
        let db = setup_test_db().await;

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![
            Box::new(TestMigration {
                name: "m003_third".to_owned(),
            }),
            Box::new(TestMigration {
                name: "m001_first".to_owned(),
            }),
            Box::new(TestMigration {
                name: "m002_second".to_owned(),
            }),
        ];

        let result = run_migrations_for_module(&db, "test_ordering", migrations)
            .await
            .expect("Migration should succeed");

        assert_eq!(
            result.applied_names,
            vec!["m001_first", "m002_second", "m003_third"]
        );
    }

    #[tokio::test]
    async fn test_per_module_table_separation() {
        let db = setup_test_db().await;

        let migrations_a: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_initial".to_owned(),
        })];

        let result_a = run_migrations_for_module(&db, "module_a", migrations_a)
            .await
            .expect("Module A migration should succeed");

        assert_eq!(result_a.applied, 1);

        let migrations_b: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_initial".to_owned(),
        })];

        let result_b = run_migrations_for_module(&db, "module_b", migrations_b)
            .await
            .expect("Module B migration should succeed");

        assert_eq!(result_b.applied, 1);

        let table_a = migration_table_name("module_a");
        let table_b = migration_table_name("module_b");
        let conn = db.sea_internal();
        let backend = conn.get_database_backend();
        let check_a = conn
            .query_one(Statement::from_string(
                backend,
                format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table_a}'"
                ),
            ))
            .await
            .expect("Query should succeed")
            .expect("Result should exist");

        let count_a: i32 = check_a.try_get_by_index(0).expect("Should get count");
        assert_eq!(count_a, 1);

        let check_b = conn
            .query_one(Statement::from_string(
                backend,
                format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table_b}'"
                ),
            ))
            .await
            .expect("Query should succeed")
            .expect("Result should exist");

        let count_b: i32 = check_b.try_get_by_index(0).expect("Should get count");
        assert_eq!(count_b, 1);
    }

    #[tokio::test]
    async fn test_duplicate_migration_name_rejected() {
        let db = setup_test_db().await;

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![
            Box::new(TestMigration {
                name: "m001_dup".to_owned(),
            }),
            Box::new(TestMigration {
                name: "m001_dup".to_owned(),
            }),
        ];

        let err = run_migrations_for_module(&db, "dup_module", migrations)
            .await
            .unwrap_err();

        match err {
            MigrationError::DuplicateMigrationName { module, name } => {
                assert_eq!(module, "dup_module");
                assert_eq!(name, "m001_dup");
            }
            other => panic!("expected DuplicateMigrationName, got: {other:?}"),
        }
    }

    #[test]
    fn test_table_name_length_limit() {
        let long = "this-is-a-very-long-module-name/with.weird.chars/and-more-and-more-and-more";
        let t = migration_table_name(long);
        assert!(t.len() <= 63);
        assert!(t.starts_with("modkit_migrations__"));
    }

    #[tokio::test]
    async fn test_get_pending_migrations() {
        let db = setup_test_db().await;

        let module_name = "test_pending";

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![
            Box::new(TestMigration {
                name: "m001_first".to_owned(),
            }),
            Box::new(TestMigration {
                name: "m002_second".to_owned(),
            }),
        ];

        let pending = get_pending_migrations(&db, module_name, &migrations)
            .await
            .expect("Should succeed");

        assert_eq!(pending.len(), 2);

        let first: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_first".to_owned(),
        })];

        run_migrations_for_module(&db, module_name, first)
            .await
            .expect("Should succeed");

        let pending = get_pending_migrations(&db, module_name, &migrations)
            .await
            .expect("Should succeed");

        assert_eq!(pending, vec!["m002_second"]);
    }

    #[tokio::test]
    async fn test_run_migrations_for_testing() {
        let db = setup_test_db().await;

        let migrations: Vec<Box<dyn MigrationTrait>> = vec![Box::new(TestMigration {
            name: "m001_test".to_owned(),
        })];

        let result = run_migrations_for_testing(&db, migrations)
            .await
            .expect("Test migrations should succeed");

        assert_eq!(result.applied, 1);
    }
}
