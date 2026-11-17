//! Initial schema (DESIGN §3.7): `model_registry__providers` and
//! `model_registry__models` with their named unique keys, the composite
//! cascading foreign key, the value checks, and one B-tree index per
//! filterable field. PostgreSQL and SQLite share the same shape.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const PROVIDERS: &str = "model_registry__providers";
pub const MODELS: &str = "model_registry__models";

/// Logical type names per backend.
struct Types {
    uuid: &'static str,
    timestamp: &'static str,
    json: &'static str,
    bigint: &'static str,
}

fn types_for(backend: sea_orm::DatabaseBackend) -> Result<Types, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(Types {
            uuid: "UUID",
            timestamp: "TIMESTAMPTZ",
            json: "JSONB",
            bigint: "BIGINT",
        }),
        sea_orm::DatabaseBackend::Sqlite => Ok(Types {
            uuid: "TEXT",
            timestamp: "TEXT",
            json: "TEXT",
            bigint: "INTEGER",
        }),
        other => Err(DbErr::Custom(format!(
            "model-registry supports PostgreSQL and SQLite, not {other:?}"
        ))),
    }
}

/// Filterable provider columns of DESIGN §3.3 that get a single-column index
/// (the unique `(tenant_id, slug)` key covers `tenant_id`, the PK covers `id`,
/// `(slug, id)` covers `slug`).
const PROVIDER_INDEXED: &[&str] = &[
    "name",
    "gts_type",
    "enabled",
    "sharing_mode",
    "managed",
    "created_at",
    "updated_at",
];

/// Filterable model columns of DESIGN §3.3 that get a single-column index
/// (the unique `(tenant_id, canonical_id)` key covers `tenant_id`, the unique
/// `(provider_id, name)` key covers `provider_id`, `(canonical_id, id)` covers
/// `canonical_id`).
const MODEL_INDEXED: &[&str] = &[
    "name",
    "provider_model_id",
    "gts_type",
    "sharing_mode",
    "approval_state",
    "approval_decided_at",
    "cap_vision",
    "cap_reasoning_effort",
    "cap_tools",
    "cap_json_schema",
    "cap_streaming",
    "cap_file_input",
    "cap_image_generation",
    "cap_audio_input",
    "cap_audio_output",
    "cap_code_interpreter",
    "cap_web_search",
    "api_completion",
    "api_embedding",
    "api_batch",
    "display_name",
    "family",
    "vendor",
    "hosted_by",
    "region",
    "model_version",
    "managed",
    "architecture",
    "format",
    "sort_order",
    "last_release_at",
    "ctx_max_input_tokens",
    "ctx_max_output_tokens",
    "created_at",
    "updated_at",
];

fn index_statements(table: &str, columns: &[&str]) -> String {
    use std::fmt::Write as _;
    columns.iter().fold(String::new(), |mut sql, c| {
        // Writing to a `String` cannot fail.
        writeln!(
            sql,
            "CREATE INDEX IF NOT EXISTS idx_{table}__{c} ON {table} ({c});"
        )
        .ok();
        sql
    })
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let Types {
            uuid,
            timestamp,
            json,
            bigint,
        } = types_for(manager.get_database_backend())?;

        let providers = format!(
            r"
CREATE TABLE IF NOT EXISTS {PROVIDERS} (
    id             {uuid} NOT NULL,
    tenant_id      {uuid} NOT NULL,
    slug           VARCHAR(64) NOT NULL,
    name           VARCHAR(255) NOT NULL,
    gts_type       VARCHAR(512) NOT NULL,
    enabled        BOOLEAN NOT NULL,
    sharing_mode   VARCHAR(16) NOT NULL,
    managed        BOOLEAN NOT NULL,
    metadata       {json},
    version        {bigint} NOT NULL,
    created_at     {timestamp} NOT NULL,
    created_by     {uuid} NOT NULL,
    updated_at     {timestamp} NOT NULL,
    updated_by     {uuid} NOT NULL,
    PRIMARY KEY (id),
    CONSTRAINT uq_{PROVIDERS}__tenant_slug UNIQUE (tenant_id, slug),
    CONSTRAINT uq_{PROVIDERS}__id_tenant UNIQUE (id, tenant_id),
    CONSTRAINT ck_{PROVIDERS}__sharing_mode CHECK (sharing_mode IN ('tenant', 'shared'))
);
CREATE INDEX IF NOT EXISTS idx_{PROVIDERS}__slug_id ON {PROVIDERS} (slug, id);
"
        );

        let models = format!(
            r"
CREATE TABLE IF NOT EXISTS {MODELS} (
    id                        {uuid} NOT NULL,
    tenant_id                 {uuid} NOT NULL,
    provider_id               {uuid} NOT NULL,
    name                      VARCHAR(128) NOT NULL,
    canonical_id              VARCHAR(194) NOT NULL,
    provider_model_id         VARCHAR(255) NOT NULL,
    gts_type                  VARCHAR(512) NOT NULL,
    sharing_mode              VARCHAR(16) NOT NULL,
    approval_state            VARCHAR(16) NOT NULL,
    approval_decided_by       {uuid} NOT NULL,
    approval_decided_at       {timestamp} NOT NULL,
    base_url                  VARCHAR(2048),
    secret_ref                VARCHAR(255),
    display_name              VARCHAR(255) NOT NULL,
    description               TEXT,
    family                    VARCHAR(128),
    vendor                    VARCHAR(128),
    managed                   BOOLEAN NOT NULL,
    architecture              VARCHAR(64),
    size_bytes                {bigint},
    format                    VARCHAR(64),
    region                    VARCHAR(64),
    hosted_by                 VARCHAR(128),
    last_release_at           {timestamp},
    reasoning_level           VARCHAR(64),
    model_version             VARCHAR(64),
    sort_order                INTEGER NOT NULL DEFAULT 0,
    icon                      VARCHAR(2048),
    multiplier_display        VARCHAR(32),
    cost                      {json} NOT NULL,
    performance               {json} NOT NULL,
    additional_info           {json} NOT NULL,
    supported_api             {json} NOT NULL,
    api_completion            BOOLEAN NOT NULL,
    api_embedding             BOOLEAN NOT NULL,
    api_batch                 BOOLEAN NOT NULL,
    capabilities              {json} NOT NULL,
    disabled_capabilities     {json} NOT NULL,
    cap_vision                BOOLEAN NOT NULL,
    cap_reasoning_effort      BOOLEAN NOT NULL,
    cap_tools                 BOOLEAN NOT NULL,
    cap_json_schema           BOOLEAN NOT NULL,
    cap_streaming             BOOLEAN NOT NULL,
    cap_file_input            BOOLEAN NOT NULL,
    cap_image_generation      BOOLEAN NOT NULL,
    cap_audio_input           BOOLEAN NOT NULL,
    cap_audio_output          BOOLEAN NOT NULL,
    cap_code_interpreter      BOOLEAN NOT NULL,
    cap_web_search            BOOLEAN NOT NULL,
    ctx_max_input_tokens      {bigint} NOT NULL,
    ctx_max_output_tokens     {bigint},
    ctx_output_vector_size    {bigint},
    default_parameters        {json} NOT NULL,
    allow_parameter_override  BOOLEAN NOT NULL,
    allow_extra_params        {json} NOT NULL,
    provider_settings         {json} NOT NULL,
    version                   {bigint} NOT NULL,
    created_at                {timestamp} NOT NULL,
    created_by                {uuid} NOT NULL,
    updated_at                {timestamp} NOT NULL,
    updated_by                {uuid} NOT NULL,
    PRIMARY KEY (id),
    CONSTRAINT uq_{MODELS}__tenant_canonical_id UNIQUE (tenant_id, canonical_id),
    CONSTRAINT uq_{MODELS}__provider_name UNIQUE (provider_id, name),
    CONSTRAINT fk_{MODELS}__provider FOREIGN KEY (provider_id, tenant_id) REFERENCES {PROVIDERS} (id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT ck_{MODELS}__sharing_mode CHECK (sharing_mode IN ('tenant', 'shared')),
    CONSTRAINT ck_{MODELS}__approval_state CHECK (approval_state IN ('pending', 'approved', 'rejected', 'revoked'))
);
CREATE INDEX IF NOT EXISTS idx_{MODELS}__canonical_id_id ON {MODELS} (canonical_id, id);
"
        );

        let sql = format!(
            "{providers}{}{models}{}",
            index_statements(PROVIDERS, PROVIDER_INDEXED),
            index_statements(MODELS, MODEL_INDEXED)
        );
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {MODELS}"))
            .await?;
        conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {PROVIDERS}"))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement};

    async fn migrated() -> sea_orm::DatabaseConnection {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        Migration::up(&Migration, &SchemaManager::new(&conn))
            .await
            .unwrap();
        conn
    }

    async fn exec(conn: &sea_orm::DatabaseConnection, sql: &str) -> Result<(), DbErr> {
        conn.execute_unprepared(sql).await.map(|_| ())
    }

    fn provider() -> String {
        format!(
            "INSERT INTO {PROVIDERS} (id, tenant_id, slug, name, gts_type, enabled, sharing_mode, managed, metadata, version, created_at, created_by, updated_at, updated_by) VALUES ('p1', 't1', 'openai', 'OpenAI', 'gts.x~', 1, 'shared', 0, NULL, 1, 'now', 's', 'now', 's')"
        )
    }

    fn model(id: &str, provider: &str, tenant: &str, name: &str) -> String {
        format!(
            "INSERT INTO {MODELS} (id, tenant_id, provider_id, name, canonical_id, provider_model_id, gts_type, sharing_mode, approval_state, approval_decided_by, approval_decided_at, display_name, managed, cost, performance, additional_info, supported_api, api_completion, api_embedding, api_batch, capabilities, disabled_capabilities, cap_vision, cap_reasoning_effort, cap_tools, cap_json_schema, cap_streaming, cap_file_input, cap_image_generation, cap_audio_input, cap_audio_output, cap_code_interpreter, cap_web_search, ctx_max_input_tokens, default_parameters, allow_parameter_override, allow_extra_params, provider_settings, version, created_at, created_by, updated_at, updated_by) VALUES ('{id}', '{tenant}', '{provider}', '{name}', 'openai::{name}', 'gpt', 'gts.x~', 'tenant', 'approved', 's', 'now', 'GPT', 0, '{{}}', '{{}}', '{{}}', '[]', 0, 0, 0, '{{}}', '{{}}', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, '{{}}', 0, '[]', '{{}}', 1, 'now', 's', 'now', 's')"
        )
    }

    #[tokio::test]
    async fn up_down_roundtrip() {
        let conn = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        let manager = SchemaManager::new(&conn);
        assert!(!manager.has_table(PROVIDERS).await.unwrap());
        Migration::up(&Migration, &manager).await.unwrap();
        assert!(manager.has_table(PROVIDERS).await.unwrap());
        assert!(manager.has_table(MODELS).await.unwrap());
        Migration::up(&Migration, &manager).await.unwrap();
        Migration::down(&Migration, &manager).await.unwrap();
        assert!(!manager.has_table(MODELS).await.unwrap());
    }

    #[tokio::test]
    async fn foreign_key_is_enforced_and_cascades() {
        let conn = migrated().await;
        exec(&conn, &provider()).await.unwrap();
        let err = exec(&conn, &model("m0", "missing", "t1", "gpt"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_foreign_key_violation(&err), "{err}");
        let err = exec(&conn, &model("m0", "p1", "other-tenant", "gpt"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_foreign_key_violation(&err), "{err}");
        exec(&conn, &model("m1", "p1", "t1", "gpt")).await.unwrap();
        exec(&conn, &format!("DELETE FROM {PROVIDERS} WHERE id = 'p1'"))
            .await
            .unwrap();
        let rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT id FROM {MODELS}"),
            ))
            .await
            .unwrap();
        assert!(rows.is_empty(), "models must cascade with their provider");
    }

    #[tokio::test]
    async fn unique_keys_and_checks_hold() {
        let conn = migrated().await;
        exec(&conn, &provider()).await.unwrap();
        let err = exec(&conn, &provider().replace("'p1'", "'p2'"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_unique_violation(&err), "{err}");
        let err = exec(
            &conn,
            &provider()
                .replace("'p1', 't1', 'openai'", "'p3', 't1', 'x'")
                .replace("'shared'", "'children'"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");

        exec(&conn, &model("m1", "p1", "t1", "gpt")).await.unwrap();
        // Same `name` under the provider: rejected by `(provider_id, name)`.
        let err = exec(&conn, &model("m2", "p1", "t1", "gpt"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_unique_violation(&err), "{err}");
        // Same `provider_model_id` with a different `name`: accepted.
        exec(&conn, &model("m2", "p1", "t1", "gpt-eu"))
            .await
            .unwrap();
        let err = exec(
            &conn,
            &model("m3", "p1", "t1", "gpt2").replace("'approved'", "'maybe'"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");
    }

    async fn index_columns(conn: &sea_orm::DatabaseConnection, table: &str) -> Vec<Vec<String>> {
        let idx_rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT name FROM pragma_index_list('{table}');"),
            ))
            .await
            .unwrap();
        let mut out = Vec::new();
        for row in &idx_rows {
            let Ok(idx) = row.try_get_by::<String, _>("name") else {
                continue;
            };
            let cols = conn
                .query_all_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    format!("SELECT name FROM pragma_index_info('{idx}') ORDER BY seqno;"),
                ))
                .await
                .unwrap()
                .iter()
                .filter_map(|r| r.try_get_by::<String, _>("name").ok())
                .collect();
            out.push(cols);
        }
        out
    }

    /// Every filterable field has an index that starts with its column
    /// (DESIGN §3.3), derived from the filter surface itself.
    #[tokio::test]
    async fn every_filterable_column_leads_an_index() {
        use crate::infra::storage::odata_mapper::{ModelODataMapper, ProviderODataMapper};
        use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
        use toolkit_db::odata::sea_orm_filter::FieldToColumn;
        use toolkit_odata::filter::FilterField;

        let conn = migrated().await;
        let leads = |indexes: &[Vec<String>], column: &str| {
            indexes
                .iter()
                .any(|cols| cols.first().is_some_and(|c| c == column))
        };

        let model_indexes = index_columns(&conn, MODELS).await;
        for field in ModelFilterField::FIELDS {
            let column = ModelODataMapper::map_field(*field).to_string();
            assert!(
                leads(&model_indexes, &column),
                "models field `{}` maps to unindexed column `{column}`",
                field.name()
            );
        }
        let provider_indexes = index_columns(&conn, PROVIDERS).await;
        for field in ProviderFilterField::FIELDS {
            let column = ProviderODataMapper::map_field(*field).to_string();
            assert!(
                leads(&provider_indexes, &column),
                "providers field `{}` maps to unindexed column `{column}`",
                field.name()
            );
        }
    }
}
