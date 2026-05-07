//! Microchat-flavoured wiring on top of the base `TestEnv`. Wraps the
//! `cf-file-storage` `Service<R>` in a `LocalClient`, runs the
//! microchat migration on the same `Db`, and constructs a `Microchat`
//! ready to drive lifecycle and race tests.

use std::sync::Arc;

use file_storage::domain::local_client::LocalClient;
use file_storage_sdk::FileStorageClient;
use microchat_test::{Microchat, MicrochatLimits, Migrator as MicrochatMigrator};
use modkit_db::migration_runner::run_migrations_for_testing;
use sea_orm_migration::MigratorTrait;

use super::{EnvSpec, TestEnv, make_env};

pub struct MicrochatEnv {
    pub fs_env: TestEnv,
    pub microchat: Microchat,
    pub fs_client: Arc<dyn FileStorageClient>,
}

pub async fn make_microchat_env(spec: EnvSpec) -> MicrochatEnv {
    let fs_env = make_env(spec).await;
    let local: Arc<dyn FileStorageClient> =
        Arc::new(LocalClient::new(fs_env.service.clone()));
    run_migrations_for_testing(&fs_env.db, MicrochatMigrator::migrations())
        .await
        .expect("run microchat migrations");
    let microchat = Microchat::new(local.clone(), fs_env.db.clone(), MicrochatLimits::default());
    MicrochatEnv {
        fs_env,
        microchat,
        fs_client: local,
    }
}
