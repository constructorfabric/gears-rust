// Rewrites the two built-in plugin instance ids that were persisted with an
// invalid GTS shape (`gtx.` prefix, trailing `~` making them type ids) to
// valid GTS instance ids of `gts.cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~`.
//
// Columns holding a plugin instance id: `session_types.plugin_instance_id`
// and `plugin_configs.plugin_instance_id` (part of the composite PK
// `(plugin_instance_id, session_type_id)`).
//
// The literals are frozen on purpose: a migration must not follow later
// changes to the runtime constants. The SQL is backend-neutral (no quoting,
// no UPDATE ... FROM), so it runs unchanged on Postgres, SQLite and MySQL.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

const ID_PAIRS: [(&str, &str); 2] = [
    (
        "gtx.cf.chat_engine.webhook_compat_plugin.v1~",
        "gts.cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~cf.chat_engine._.webhook_compat.v1",
    ),
    (
        "gtx.cf.chat_engine.llm_gateway_plugin.v1~",
        "gts.cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~cf.chat_engine._.llm_gateway.v1",
    ),
];

#[derive(DeriveMigrationName)]
pub struct Migration;

async fn rewrite(manager: &SchemaManager<'_>, from: &str, to: &str) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = manager.get_database_backend();
    let run = |sql: String| db.execute_raw(Statement::from_string(backend, sql));

    // plugin_configs: if a row under the target id already exists for the same
    // session type, it wins (it is the newer write); drop the stale row so the
    // UPDATE cannot violate the composite primary key.
    run(format!(
        "DELETE FROM plugin_configs WHERE plugin_instance_id = '{from}' \
         AND EXISTS (SELECT 1 FROM (SELECT session_type_id FROM plugin_configs \
         WHERE plugin_instance_id = '{to}') AS t \
         WHERE t.session_type_id = plugin_configs.session_type_id)"
    ))
    .await?;
    run(format!(
        "UPDATE plugin_configs SET plugin_instance_id = '{to}' \
         WHERE plugin_instance_id = '{from}'"
    ))
    .await?;
    run(format!(
        "UPDATE session_types SET plugin_instance_id = '{to}' \
         WHERE plugin_instance_id = '{from}'"
    ))
    .await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (old, new) in ID_PAIRS {
            rewrite(manager, old, new).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (old, new) in ID_PAIRS {
            rewrite(manager, new, old).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use super::ID_PAIRS;
    use crate::infra::db::Migrator;

    async fn exec(db: &sea_orm::DatabaseConnection, sql: String) {
        db.execute_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
            .await
            .expect("exec");
    }

    async fn ids(db: &sea_orm::DatabaseConnection, sql: &str) -> Vec<String> {
        db.query_all_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.try_get::<String>("", "id").unwrap())
            .collect()
    }

    #[tokio::test]
    async fn rewrites_old_ids_and_resolves_composite_pk_conflicts() {
        let mut opts = ConnectOptions::new("sqlite::memory:".to_string());
        opts.max_connections(1);
        let db = Database::connect(opts).await.unwrap();
        Migrator::up(&db, Some(6)).await.unwrap();

        let (old_llm, new_llm) = ID_PAIRS[1];
        let (old_wh, new_wh) = ID_PAIRS[0];
        let now = "2026-01-01T00:00:00Z";
        let (st1, st2, st3) = (
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000003",
        );
        for (st, plugin) in [(st1, old_llm), (st2, old_wh), (st3, "echo")] {
            exec(
                &db,
                format!(
                    "INSERT INTO session_types (session_type_id, name, plugin_instance_id, \
                 created_at, updated_at) VALUES ('{st}', 'n', '{plugin}', '{now}', '{now}')"
                ),
            )
            .await;
        }
        // st1: old row + already-present new row (conflict, new must win).
        // st2: only an old row. st3: unrelated plugin, untouched.
        for (plugin, st, cfg) in [
            (old_llm, st1, "old"),
            (new_llm, st1, "new"),
            (old_wh, st2, "old"),
            ("echo", st3, "keep"),
        ] {
            exec(
                &db,
                format!(
                    "INSERT INTO plugin_configs (plugin_instance_id, session_type_id, config, \
                 created_at, updated_at) VALUES ('{plugin}', '{st}', '\"{cfg}\"', '{now}', '{now}')"
                ),
            )
            .await;
        }

        Migrator::up(&db, None).await.unwrap();

        assert_eq!(
            ids(
                &db,
                "SELECT plugin_instance_id AS id FROM session_types ORDER BY session_type_id"
            )
            .await,
            vec![new_llm.to_owned(), new_wh.to_owned(), "echo".to_owned()]
        );
        assert_eq!(
            ids(
                &db,
                "SELECT plugin_instance_id || '|' || session_type_id || '|' || config AS id \
                      FROM plugin_configs ORDER BY session_type_id"
            )
            .await,
            vec![
                format!("{new_llm}|{st1}|\"new\""),
                format!("{new_wh}|{st2}|\"old\""),
                format!("echo|{st3}|\"keep\""),
            ]
        );
    }
}
