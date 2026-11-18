//! Migrations of the gear store (DESIGN §3.7).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

/// Unique indexes of evidence; the store reads them from a violation.
pub const UQ_EVIDENCE_REPLAY_KEY: &str = "uq_license_enforcement__evidence_replay_key";
pub const UQ_EVIDENCE_EXTERNAL_ID: &str = "uq_license_enforcement__evidence_external_id";
pub const UQ_EVIDENCE_REPLACES: &str = "uq_license_enforcement__evidence_replaces";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20261026_000001_initial::Migration)]
    }
}

mod m20261026_000001_initial {
    use super::{
        ConnectionTrait, DatabaseBackend, DbErr, MigrationName, MigrationTrait, SchemaManager,
    };

    pub struct Migration;

    // The runner orders migrations by name: one file holds them all, so each
    // names itself.
    impl MigrationName for Migration {
        fn name(&self) -> &'static str {
            "m20261026_000001_initial"
        }
    }

    const POSTGRES: &str = r"
CREATE TABLE IF NOT EXISTS license_enforcement__grants (
    id UUID PRIMARY KEY NOT NULL,
    holder_tenant_id UUID NOT NULL,
    resource_type TEXT NOT NULL,
    resource_type_key TEXT NOT NULL,
    resource_id TEXT,
    subject_type TEXT NOT NULL,
    binding_kind TEXT NOT NULL,
    binding_subject_type TEXT,
    binding_subject_type_key TEXT,
    binding_subject_id TEXT,
    binding_grant_id UUID,
    binding_tenant_id UUID,
    ended_subject_type_key TEXT,
    ended_subject_id TEXT,
    binding_fixed BOOLEAN NOT NULL,
    validity_kind TEXT NOT NULL,
    not_before TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    grace_until TIMESTAMPTZ,
    effective_end TIMESTAMPTZ NOT NULL,
    conditions JSONB,
    recipient_rules JSONB,
    may_bind BOOLEAN NOT NULL,
    max_depth INTEGER NOT NULL,
    requires_acceptance BOOLEAN NOT NULL,
    root_grant_id UUID NOT NULL,
    parent_grant_id UUID,
    grantor_tenant_id UUID,
    delegation_id UUID,
    pack_id UUID,
    evidence_id UUID NOT NULL,
    state TEXT NOT NULL,
    termination_reason TEXT,
    version BIGINT NOT NULL,
    delegation_version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__grants_one_live_child
    ON license_enforcement__grants (parent_grant_id) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_resource
    ON license_enforcement__grants (holder_tenant_id, resource_type_key, effective_end) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_subject
    ON license_enforcement__grants (holder_tenant_id, binding_subject_id, resource_type_key, id) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_by_subject
    ON license_enforcement__grants (holder_tenant_id, binding_subject_id, updated_at) WHERE state = 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_by_ended_subject
    ON license_enforcement__grants (holder_tenant_id, ended_subject_id, updated_at)
    WHERE state = 'terminated' AND ended_subject_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_pack ON license_enforcement__grants (pack_id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_tenant ON license_enforcement__grants (holder_tenant_id, id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_root ON license_enforcement__grants (root_grant_id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_live_by_root
    ON license_enforcement__grants (root_grant_id) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_parent ON license_enforcement__grants (parent_grant_id)
    WHERE parent_grant_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_delegation ON license_enforcement__grants (delegation_id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_roots
    ON license_enforcement__grants (updated_at, id) WHERE parent_grant_id IS NULL AND state = 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_branches
    ON license_enforcement__grants (updated_at, id) WHERE parent_grant_id IS NOT NULL AND state = 'terminated';

CREATE TABLE IF NOT EXISTS license_enforcement__packs (
    id UUID PRIMARY KEY NOT NULL,
    holder_tenant_id UUID NOT NULL,
    version BIGINT NOT NULL,
    counts JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__packs_by_tenant ON license_enforcement__packs (holder_tenant_id, id);

CREATE TABLE IF NOT EXISTS license_enforcement__evidence (
    id UUID PRIMARY KEY NOT NULL,
    holder_tenant_id UUID NOT NULL,
    plugin_id TEXT NOT NULL,
    intake TEXT NOT NULL,
    bytes BYTEA NOT NULL,
    replay_key TEXT,
    external_id TEXT,
    replaces_evidence_id UUID,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__evidence_replay_key ON license_enforcement__evidence (plugin_id, replay_key);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__evidence_external_id ON license_enforcement__evidence (plugin_id, external_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__evidence_replaces ON license_enforcement__evidence (plugin_id, replaces_evidence_id);

CREATE TABLE IF NOT EXISTS license_enforcement__idempotency (
    tenant_id UUID NOT NULL,
    subject_id UUID NOT NULL,
    token TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    response JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, subject_id, token)
);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__idempotency_by_expiry ON license_enforcement__idempotency (expires_at);

CREATE TABLE IF NOT EXISTS license_enforcement__plugin_material (
    plugin_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value BYTEA NOT NULL,
    PRIMARY KEY (plugin_id, key)
);
";

    const SQLITE: &str = r"
CREATE TABLE IF NOT EXISTS license_enforcement__grants (
    id TEXT PRIMARY KEY NOT NULL,
    holder_tenant_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_type_key TEXT NOT NULL,
    resource_id TEXT,
    subject_type TEXT NOT NULL,
    binding_kind TEXT NOT NULL,
    binding_subject_type TEXT,
    binding_subject_type_key TEXT,
    binding_subject_id TEXT,
    binding_grant_id TEXT,
    binding_tenant_id TEXT,
    ended_subject_type_key TEXT,
    ended_subject_id TEXT,
    binding_fixed INTEGER NOT NULL,
    validity_kind TEXT NOT NULL,
    not_before TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    grace_until TEXT,
    effective_end TEXT NOT NULL,
    conditions TEXT,
    recipient_rules TEXT,
    may_bind INTEGER NOT NULL,
    max_depth INTEGER NOT NULL,
    requires_acceptance INTEGER NOT NULL,
    root_grant_id TEXT NOT NULL,
    parent_grant_id TEXT,
    grantor_tenant_id TEXT,
    delegation_id TEXT,
    pack_id TEXT,
    evidence_id TEXT NOT NULL,
    state TEXT NOT NULL,
    termination_reason TEXT,
    version INTEGER NOT NULL,
    delegation_version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__grants_one_live_child
    ON license_enforcement__grants (parent_grant_id) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_resource
    ON license_enforcement__grants (holder_tenant_id, resource_type_key, effective_end) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_subject
    ON license_enforcement__grants (holder_tenant_id, binding_subject_id, resource_type_key, id) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_by_subject
    ON license_enforcement__grants (holder_tenant_id, binding_subject_id, updated_at) WHERE state = 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_by_ended_subject
    ON license_enforcement__grants (holder_tenant_id, ended_subject_id, updated_at)
    WHERE state = 'terminated' AND ended_subject_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_pack ON license_enforcement__grants (pack_id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_tenant ON license_enforcement__grants (holder_tenant_id, id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_root ON license_enforcement__grants (root_grant_id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_live_by_root
    ON license_enforcement__grants (root_grant_id) WHERE state <> 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_parent ON license_enforcement__grants (parent_grant_id)
    WHERE parent_grant_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_by_delegation ON license_enforcement__grants (delegation_id);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_roots
    ON license_enforcement__grants (updated_at, id) WHERE parent_grant_id IS NULL AND state = 'terminated';
CREATE INDEX IF NOT EXISTS idx_license_enforcement__grants_terminated_branches
    ON license_enforcement__grants (updated_at, id) WHERE parent_grant_id IS NOT NULL AND state = 'terminated';

CREATE TABLE IF NOT EXISTS license_enforcement__packs (
    id TEXT PRIMARY KEY NOT NULL,
    holder_tenant_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    counts TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__packs_by_tenant ON license_enforcement__packs (holder_tenant_id, id);

CREATE TABLE IF NOT EXISTS license_enforcement__evidence (
    id TEXT PRIMARY KEY NOT NULL,
    holder_tenant_id TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    intake TEXT NOT NULL,
    bytes BLOB NOT NULL,
    replay_key TEXT,
    external_id TEXT,
    replaces_evidence_id TEXT,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__evidence_replay_key ON license_enforcement__evidence (plugin_id, replay_key);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__evidence_external_id ON license_enforcement__evidence (plugin_id, external_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_license_enforcement__evidence_replaces ON license_enforcement__evidence (plugin_id, replaces_evidence_id);

CREATE TABLE IF NOT EXISTS license_enforcement__idempotency (
    tenant_id TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    token TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    response TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, subject_id, token)
);
CREATE INDEX IF NOT EXISTS idx_license_enforcement__idempotency_by_expiry ON license_enforcement__idempotency (expires_at);

CREATE TABLE IF NOT EXISTS license_enforcement__plugin_material (
    plugin_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (plugin_id, key)
);
";

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let sql = match manager.get_database_backend() {
                DatabaseBackend::Postgres => POSTGRES,
                DatabaseBackend::Sqlite => SQLITE,
                other => {
                    return Err(DbErr::Custom(format!(
                        "migration has no DDL for database backend {other:?}"
                    )));
                }
            };
            manager.get_connection().execute_unprepared(sql).await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared(
                    "DROP TABLE IF EXISTS license_enforcement__plugin_material; DROP TABLE IF EXISTS license_enforcement__idempotency; \
                     DROP TABLE IF EXISTS license_enforcement__evidence; DROP TABLE IF EXISTS license_enforcement__packs; \
                     DROP TABLE IF EXISTS license_enforcement__grants;",
                )
                .await?;
            Ok(())
        }
    }
}
