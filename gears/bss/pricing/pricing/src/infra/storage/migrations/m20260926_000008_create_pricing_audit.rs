//! Audit schema, following Products.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.pricing_audit (
            audit_id          uuid        NOT NULL,
            tenant_id         uuid        NOT NULL,
            actor_ref         uuid        NOT NULL,
            action            text        NOT NULL,
            subject_kind      text        NOT NULL,
            subject_id        uuid,
            subject_revision  bigint,
            error_code        text,
            attempted_key     text,
            reason            text,
            correlation_id    text,
            written_at        timestamptz NOT NULL,
            session_id        uuid,
            ceremony_ref      uuid,
            seal_state        text        NOT NULL,
            chain_id          uuid,
            seq               bigint,
            prev_hash         bytea,
            row_hash          bytea,
            CONSTRAINT pricing_audit_pkey PRIMARY KEY (audit_id),
            CONSTRAINT chk_pricing_audit_seal_state CHECK (seal_state IN ('unsealed', 'sealed')),
            CONSTRAINT chk_pricing_audit_seal_group CHECK (
                (seal_state = 'unsealed' AND chain_id IS NULL AND seq IS NULL AND prev_hash IS NULL AND row_hash IS NULL)
                OR
                (seal_state = 'sealed' AND chain_id IS NOT NULL AND seq IS NOT NULL AND row_hash IS NOT NULL)
            ),
            CONSTRAINT chk_pricing_audit_seq CHECK (seq IS NULL OR seq >= 0),
            CONSTRAINT chk_pricing_audit_subject_ref CHECK (subject_id IS NOT NULL OR attempted_key IS NOT NULL OR session_id IS NOT NULL)
        )",
    "CREATE INDEX IF NOT EXISTS idx_pricing_audit_tenant_time ON bss.pricing_audit USING btree (tenant_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_pricing_audit_subject ON bss.pricing_audit USING btree (tenant_id, subject_kind, subject_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_pricing_audit_actor ON bss.pricing_audit USING btree (tenant_id, actor_ref, written_at)",
    "CREATE OR REPLACE FUNCTION bss.pricing_audit_append_only() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION 'pricing_audit is append-only: DELETE is not permitted';
          END IF;

          IF OLD.seal_state = 'unsealed'
             AND NEW.seal_state = 'sealed'
             AND NEW.chain_id IS NOT NULL
             AND NEW.seq IS NOT NULL
             AND NEW.row_hash IS NOT NULL
             AND NEW.audit_id IS NOT DISTINCT FROM OLD.audit_id
             AND NEW.tenant_id IS NOT DISTINCT FROM OLD.tenant_id
             AND NEW.actor_ref IS NOT DISTINCT FROM OLD.actor_ref
             AND NEW.action IS NOT DISTINCT FROM OLD.action
             AND NEW.subject_kind IS NOT DISTINCT FROM OLD.subject_kind
             AND NEW.subject_id IS NOT DISTINCT FROM OLD.subject_id
             AND NEW.subject_revision IS NOT DISTINCT FROM OLD.subject_revision
             AND NEW.error_code IS NOT DISTINCT FROM OLD.error_code
             AND NEW.attempted_key IS NOT DISTINCT FROM OLD.attempted_key
             AND NEW.reason IS NOT DISTINCT FROM OLD.reason
             AND NEW.correlation_id IS NOT DISTINCT FROM OLD.correlation_id
             AND NEW.written_at IS NOT DISTINCT FROM OLD.written_at
             AND NEW.session_id IS NOT DISTINCT FROM OLD.session_id
             AND NEW.ceremony_ref IS NOT DISTINCT FROM OLD.ceremony_ref
          THEN
            RETURN NEW;
          END IF;

          RAISE EXCEPTION 'pricing_audit is append-only: % is not permitted', TG_OP;
        END;
     $$ LANGUAGE plpgsql",
    "DROP TRIGGER IF EXISTS trg_pricing_audit_append_only ON bss.pricing_audit",
    "CREATE TRIGGER trg_pricing_audit_append_only BEFORE DELETE OR UPDATE ON bss.pricing_audit FOR EACH ROW EXECUTE FUNCTION bss.pricing_audit_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_audit",
    "DROP FUNCTION IF EXISTS bss.pricing_audit_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS pricing_audit (
            audit_id          text   NOT NULL,
            tenant_id         text   NOT NULL,
            actor_ref         text   NOT NULL,
            action            text   NOT NULL,
            subject_kind      text   NOT NULL,
            subject_id        text,
            subject_revision  bigint,
            error_code        text,
            attempted_key     text,
            reason            text,
            correlation_id    text,
            written_at        text   NOT NULL,
            session_id        text,
            ceremony_ref      text,
            seal_state        text   NOT NULL,
            chain_id          text,
            seq               bigint,
            prev_hash         blob,
            row_hash          blob,
            PRIMARY KEY (audit_id),
            CONSTRAINT chk_pricing_audit_seal_state CHECK (seal_state IN ('unsealed', 'sealed')),
            CONSTRAINT chk_pricing_audit_seal_group CHECK (
                (seal_state = 'unsealed' AND chain_id IS NULL AND seq IS NULL AND prev_hash IS NULL AND row_hash IS NULL)
                OR
                (seal_state = 'sealed' AND chain_id IS NOT NULL AND seq IS NOT NULL AND row_hash IS NOT NULL)
            ),
            CONSTRAINT chk_pricing_audit_seq CHECK (seq IS NULL OR seq >= 0),
            CONSTRAINT chk_pricing_audit_subject_ref CHECK (subject_id IS NOT NULL OR attempted_key IS NOT NULL OR session_id IS NOT NULL)
        )",
    "CREATE INDEX IF NOT EXISTS idx_pricing_audit_tenant_time ON pricing_audit (tenant_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_pricing_audit_subject ON pricing_audit (tenant_id, subject_kind, subject_id, written_at)",
    "CREATE INDEX IF NOT EXISTS idx_pricing_audit_actor ON pricing_audit (tenant_id, actor_ref, written_at)",
    "DROP TRIGGER IF EXISTS trg_pricing_audit_no_delete",
    "CREATE TRIGGER trg_pricing_audit_no_delete BEFORE DELETE ON pricing_audit FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_audit is append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS trg_pricing_audit_no_update",
    "CREATE TRIGGER trg_pricing_audit_no_update BEFORE UPDATE ON pricing_audit FOR EACH ROW WHEN NOT (
            OLD.seal_state IS 'unsealed'
            AND NEW.seal_state IS 'sealed'
            AND NEW.chain_id IS NOT NULL
            AND NEW.seq IS NOT NULL
            AND NEW.row_hash IS NOT NULL
        ) BEGIN SELECT RAISE(ABORT, 'pricing_audit is append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS trg_pricing_audit_seal_unchanged",
    "CREATE TRIGGER trg_pricing_audit_seal_unchanged BEFORE UPDATE ON pricing_audit FOR EACH ROW WHEN (
            OLD.seal_state IS 'unsealed'
            AND NEW.seal_state IS 'sealed'
            AND NEW.chain_id IS NOT NULL
            AND NEW.seq IS NOT NULL
            AND NEW.row_hash IS NOT NULL
        ) AND NOT (
            NEW.audit_id IS OLD.audit_id
            AND NEW.tenant_id IS OLD.tenant_id
            AND NEW.actor_ref IS OLD.actor_ref
            AND NEW.action IS OLD.action
            AND NEW.subject_kind IS OLD.subject_kind
            AND NEW.subject_id IS OLD.subject_id
            AND NEW.subject_revision IS OLD.subject_revision
            AND NEW.error_code IS OLD.error_code
            AND NEW.attempted_key IS OLD.attempted_key
            AND NEW.reason IS OLD.reason
            AND NEW.correlation_id IS OLD.correlation_id
            AND NEW.written_at IS OLD.written_at
            AND NEW.session_id IS OLD.session_id
            AND NEW.ceremony_ref IS OLD.ceremony_ref
        ) BEGIN SELECT RAISE(ABORT, 'pricing_audit is append-only: UPDATE is not permitted'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_audit"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP_STATEMENTS, SQLITE_UP_STATEMENTS).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            PG_DOWN_STATEMENTS,
            SQLITE_DOWN_STATEMENTS,
        )
        .await
    }
}
