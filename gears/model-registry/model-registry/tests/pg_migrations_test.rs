//! The initial schema on a real `PostgreSQL` (DESIGN §3.7): the `PostgreSQL`
//! twin of the SQLite tests beside `initial_001`. Docker required; run with
//! `make test-integration LANE=model-registry-pg`.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use model_registry::infra::storage::migrations::initial_001::{MODELS, Migration, PROVIDERS};
use model_registry::infra::storage::odata_mapper::{ModelODataMapper, ProviderODataMapper};
use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::prelude::*;
use toolkit_db::odata::sea_orm_filter::FieldToColumn;
use toolkit_odata::filter::FilterField;

mod common;
use common::{PgServer, pg_server};

const P1: &str = "00000000-0000-0000-0000-0000000000a1";
const P2: &str = "00000000-0000-0000-0000-0000000000a2";
const P3: &str = "00000000-0000-0000-0000-0000000000a3";
const MISSING: &str = "00000000-0000-0000-0000-0000000000af";
const T1: &str = "00000000-0000-0000-0000-0000000000b1";
const T2: &str = "00000000-0000-0000-0000-0000000000b2";
const M1: &str = "00000000-0000-0000-0000-0000000000c1";
const M2: &str = "00000000-0000-0000-0000-0000000000c2";
const M3: &str = "00000000-0000-0000-0000-0000000000c3";
const SUBJECT: &str = "00000000-0000-0000-0000-0000000000d1";

/// The server and a plain connection to its empty database.
async fn empty() -> Option<(PgServer, DatabaseConnection)> {
    let server = pg_server("migration").await?;
    let conn = Database::connect(&server.url)
        .await
        .expect("a plain connection to the container");
    Some((server, conn))
}

async fn migrated() -> Option<(PgServer, DatabaseConnection)> {
    let (server, conn) = empty().await?;
    Migration::up(&Migration, &SchemaManager::new(&conn))
        .await
        .unwrap();
    Some((server, conn))
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    conn.execute_unprepared(sql).await.map(|_| ())
}

fn provider(id: &str, slug: &str, sharing_mode: &str) -> String {
    format!(
        "INSERT INTO {PROVIDERS} (id, tenant_id, slug, name, gts_type, enabled, sharing_mode, managed, metadata, version, created_at, created_by, updated_at, updated_by) VALUES ('{id}', '{T1}', '{slug}', 'OpenAI', 'gts.x~', TRUE, '{sharing_mode}', FALSE, NULL, 1, now(), '{SUBJECT}', now(), '{SUBJECT}')"
    )
}

fn model(id: &str, provider: &str, tenant: &str, name: &str, approval_state: &str) -> String {
    format!(
        "INSERT INTO {MODELS} (id, tenant_id, provider_id, name, canonical_id, provider_model_id, gts_type, sharing_mode, approval_state, approval_decided_by, approval_decided_at, display_name, managed, cost, performance, additional_info, supported_api, api_completion, api_embedding, api_batch, capabilities, disabled_capabilities, cap_vision, cap_reasoning_effort, cap_tools, cap_json_schema, cap_streaming, cap_file_input, cap_image_generation, cap_audio_input, cap_audio_output, cap_code_interpreter, cap_web_search, ctx_max_input_tokens, default_parameters, allow_parameter_override, allow_extra_params, provider_settings, version, created_at, created_by, updated_at, updated_by) VALUES ('{id}', '{tenant}', '{provider}', '{name}', 'openai::{name}', 'gpt', 'gts.x~', 'tenant', '{approval_state}', '{SUBJECT}', now(), 'GPT', FALSE, '{{}}', '{{}}', '{{}}', '[]', FALSE, FALSE, FALSE, '{{}}', '{{}}', FALSE, FALSE, FALSE, FALSE, FALSE, FALSE, FALSE, FALSE, FALSE, FALSE, FALSE, 1, '{{}}', FALSE, '[]', '{{}}', 1, now(), '{SUBJECT}', now(), '{SUBJECT}')"
    )
}

#[tokio::test]
async fn up_down_roundtrip() {
    let Some((_server, conn)) = empty().await else {
        return;
    };
    let manager = SchemaManager::new(&conn);
    assert!(!manager.has_table(PROVIDERS).await.unwrap());
    Migration::up(&Migration, &manager).await.unwrap();
    assert!(manager.has_table(PROVIDERS).await.unwrap());
    assert!(manager.has_table(MODELS).await.unwrap());
    Migration::up(&Migration, &manager).await.unwrap();
    Migration::down(&Migration, &manager).await.unwrap();
    assert!(!manager.has_table(MODELS).await.unwrap());
    assert!(!manager.has_table(PROVIDERS).await.unwrap());
}

#[tokio::test]
async fn foreign_key_is_enforced_and_cascades() {
    let Some((_server, conn)) = migrated().await else {
        return;
    };
    exec(&conn, &provider(P1, "openai", "shared"))
        .await
        .unwrap();
    let err = exec(&conn, &model(M1, MISSING, T1, "gpt", "approved"))
        .await
        .unwrap_err();
    assert!(toolkit_db::secure::is_foreign_key_violation(&err), "{err}");
    let err = exec(&conn, &model(M1, P1, T2, "gpt", "approved"))
        .await
        .unwrap_err();
    assert!(toolkit_db::secure::is_foreign_key_violation(&err), "{err}");
    exec(&conn, &model(M1, P1, T1, "gpt", "approved"))
        .await
        .unwrap();
    exec(&conn, &format!("DELETE FROM {PROVIDERS} WHERE id = '{P1}'"))
        .await
        .unwrap();
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT id FROM {MODELS}"),
        ))
        .await
        .unwrap();
    assert!(rows.is_empty(), "models must cascade with their provider");
}

#[tokio::test]
async fn unique_keys_and_checks_hold() {
    let Some((_server, conn)) = migrated().await else {
        return;
    };
    exec(&conn, &provider(P1, "openai", "shared"))
        .await
        .unwrap();
    let err = exec(&conn, &provider(P2, "openai", "shared"))
        .await
        .unwrap_err();
    assert!(toolkit_db::secure::is_unique_violation(&err), "{err}");
    let err = exec(&conn, &provider(P3, "x", "children"))
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("check"), "{err}");

    exec(&conn, &model(M1, P1, T1, "gpt", "approved"))
        .await
        .unwrap();
    // Same `name` under the provider: rejected by `(provider_id, name)`.
    let err = exec(&conn, &model(M2, P1, T1, "gpt", "approved"))
        .await
        .unwrap_err();
    assert!(toolkit_db::secure::is_unique_violation(&err), "{err}");
    // Same `provider_model_id` with a different `name`: accepted.
    exec(&conn, &model(M2, P1, T1, "gpt-eu", "approved"))
        .await
        .unwrap();
    let err = exec(&conn, &model(M3, P1, T1, "gpt2", "maybe"))
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("check"), "{err}");
}

/// The leading column of every index on `table`.
async fn leading_index_columns(conn: &DatabaseConnection, table: &str) -> BTreeSet<String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "SELECT a.attname::text AS col FROM pg_index i \
             JOIN pg_class t ON t.oid = i.indrelid \
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = i.indkey[0] \
             WHERE t.relname = '{table}'"
        ),
    ))
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get::<String>("", "col").unwrap())
    .collect()
}

/// Every filterable field has an index that starts with its column
/// (DESIGN §3.3), derived from the filter surface itself.
#[tokio::test]
async fn every_filterable_column_leads_an_index() {
    let Some((_server, conn)) = migrated().await else {
        return;
    };
    let model_indexes = leading_index_columns(&conn, MODELS).await;
    for field in ModelFilterField::FIELDS {
        let column = ModelODataMapper::map_field(*field).to_string();
        assert!(
            model_indexes.contains(&column),
            "models field `{}` maps to unindexed column `{column}`",
            field.name()
        );
    }
    let provider_indexes = leading_index_columns(&conn, PROVIDERS).await;
    for field in ProviderFilterField::FIELDS {
        let column = ProviderODataMapper::map_field(*field).to_string();
        assert!(
            provider_indexes.contains(&column),
            "providers field `{}` maps to unindexed column `{column}`",
            field.name()
        );
    }
}
