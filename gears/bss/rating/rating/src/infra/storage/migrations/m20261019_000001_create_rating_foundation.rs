//! The foundation's tables (feature 01 §5.1): inbox, exception register, operation keys and
//! operator audit. Every other Rating table is created by the feature that first reads or writes
//! it. Names follow the `bss_rating` namespace grammar (T-D-60): `bss_rating__<table>` and
//! `idx_/uq_/ck_<table>__<purpose>`, each at most 63 bytes.
//!
//! PostgreSQL holds digests as text checked against `^[0-9a-f]{64}$`; SQLite, used in tests and
//! local development, stores the same columns as text with a length check.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss_rating__inbox (
        entry_id uuid PRIMARY KEY,
        tenant_id uuid NOT NULL,
        source text NOT NULL,
        business_id text NOT NULL,
        version bigint NOT NULL,
        payload_digest text NOT NULL,
        payload jsonb NOT NULL,
        received_via text NOT NULL,
        state text NOT NULL,
        quarantine_reason text NULL,
        conflict_of uuid NULL,
        received_at timestamptz NOT NULL,
        applied_at timestamptz NULL,
        CONSTRAINT ck_bss_rating__inbox__source CHECK (source IN
            ('fact', 'segment', 'scope', 'coverage', 'subscription_version', 'billing_hint')),
        CONSTRAINT ck_bss_rating__inbox__received_via CHECK (received_via IN ('pull', 'event')),
        CONSTRAINT ck_bss_rating__inbox__state CHECK (state IN
            ('accepted', 'applied', 'quarantined', 'discarded')),
        CONSTRAINT ck_bss_rating__inbox__version CHECK (version >= 0),
        CONSTRAINT ck_bss_rating__inbox__business_id CHECK (length(business_id) BETWEEN 1 AND 255),
        CONSTRAINT ck_bss_rating__inbox__digest CHECK (payload_digest ~ '^[0-9a-f]{64}$'),
        CONSTRAINT ck_bss_rating__inbox__quarantine_reason CHECK (quarantine_reason IS NULL
            OR quarantine_reason IN ('digest_conflict', 'unsupported_schema_version')),
        CONSTRAINT ck_bss_rating__inbox__quarantined_has_reason CHECK (state <> 'quarantined'
            OR quarantine_reason IS NOT NULL),
        CONSTRAINT ck_bss_rating__inbox__conflict CHECK (
            (COALESCE(quarantine_reason, '') = 'digest_conflict') = (conflict_of IS NOT NULL)),
        CONSTRAINT ck_bss_rating__inbox__applied_at CHECK ((state = 'applied') = (applied_at IS NOT NULL))
    )",
    // One arrival per key and digest; one primary (non-conflict) entry per key.
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__inbox__key_digest
        ON bss_rating__inbox (tenant_id, source, business_id, version, payload_digest)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__inbox__primary
        ON bss_rating__inbox (tenant_id, source, business_id, version) WHERE conflict_of IS NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__inbox__quarantined
        ON bss_rating__inbox (tenant_id, received_at) WHERE state = 'quarantined'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__exception (
        exception_id uuid PRIMARY KEY,
        tenant_id uuid NOT NULL,
        subject_kind text NOT NULL,
        subject_ref text NOT NULL,
        reason_code text NOT NULL,
        detail text NOT NULL,
        first_seen_at timestamptz NOT NULL,
        last_attempt_at timestamptz NOT NULL,
        attempts integer NOT NULL,
        resolved_at timestamptz NULL,
        resolution text NULL,
        CONSTRAINT ck_bss_rating__exception__subject_kind CHECK (subject_kind IN
            ('usage_record', 'child', 'fact', 'inbox', 'source_loss')),
        CONSTRAINT ck_bss_rating__exception__attempts CHECK (attempts >= 1),
        CONSTRAINT ck_bss_rating__exception__resolution CHECK ((resolved_at IS NULL) = (resolution IS NULL))
    )",
    // One open exception per subject and reason.
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__exception__open_subject
        ON bss_rating__exception (tenant_id, subject_kind, subject_ref, reason_code)
        WHERE resolved_at IS NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__exception__open
        ON bss_rating__exception (tenant_id, exception_id) WHERE resolved_at IS NULL",
    r"CREATE TABLE IF NOT EXISTS bss_rating__operation (
        tenant_id uuid NOT NULL,
        actor uuid NOT NULL,
        idempotency_key text NOT NULL,
        operation_id uuid NOT NULL,
        request_digest text NOT NULL,
        response jsonb NOT NULL,
        created_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, actor, idempotency_key),
        CONSTRAINT ck_bss_rating__operation__key CHECK (length(idempotency_key) BETWEEN 1 AND 255),
        CONSTRAINT ck_bss_rating__operation__digest CHECK (request_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__operation__created_at
        ON bss_rating__operation (tenant_id, created_at)",
    r"CREATE TABLE IF NOT EXISTS bss_rating__operator_audit (
        tenant_id uuid NOT NULL,
        audit_id uuid NOT NULL,
        actor uuid NOT NULL,
        operation_id text NOT NULL,
        target_kind text NOT NULL,
        target_id text NOT NULL,
        request_digest text NOT NULL,
        reason text NOT NULL,
        approved_by uuid NULL,
        request_id text NULL,
        at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, audit_id),
        CONSTRAINT ck_bss_rating__operator_audit__digest CHECK (request_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__operator_audit__target
        ON bss_rating__operator_audit (tenant_id, target_kind, target_id)",
];

const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss_rating__inbox (
        entry_id text PRIMARY KEY,
        tenant_id text NOT NULL,
        source text NOT NULL,
        business_id text NOT NULL,
        version integer NOT NULL,
        payload_digest text NOT NULL,
        payload text NOT NULL,
        received_via text NOT NULL,
        state text NOT NULL,
        quarantine_reason text NULL,
        conflict_of text NULL,
        received_at text NOT NULL,
        applied_at text NULL,
        CONSTRAINT ck_bss_rating__inbox__source CHECK (source IN
            ('fact', 'segment', 'scope', 'coverage', 'subscription_version', 'billing_hint')),
        CONSTRAINT ck_bss_rating__inbox__received_via CHECK (received_via IN ('pull', 'event')),
        CONSTRAINT ck_bss_rating__inbox__state CHECK (state IN
            ('accepted', 'applied', 'quarantined', 'discarded')),
        CONSTRAINT ck_bss_rating__inbox__version CHECK (version >= 0),
        CONSTRAINT ck_bss_rating__inbox__business_id CHECK (length(business_id) BETWEEN 1 AND 255),
        CONSTRAINT ck_bss_rating__inbox__digest CHECK (length(payload_digest) = 64),
        CONSTRAINT ck_bss_rating__inbox__quarantine_reason CHECK (quarantine_reason IS NULL
            OR quarantine_reason IN ('digest_conflict', 'unsupported_schema_version')),
        CONSTRAINT ck_bss_rating__inbox__quarantined_has_reason CHECK (state <> 'quarantined'
            OR quarantine_reason IS NOT NULL),
        CONSTRAINT ck_bss_rating__inbox__conflict CHECK (
            (COALESCE(quarantine_reason, '') = 'digest_conflict') = (conflict_of IS NOT NULL)),
        CONSTRAINT ck_bss_rating__inbox__applied_at CHECK ((state = 'applied') = (applied_at IS NOT NULL))
    )",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__inbox__key_digest
        ON bss_rating__inbox (tenant_id, source, business_id, version, payload_digest)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__inbox__primary
        ON bss_rating__inbox (tenant_id, source, business_id, version) WHERE conflict_of IS NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__inbox__quarantined
        ON bss_rating__inbox (tenant_id, received_at) WHERE state = 'quarantined'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__exception (
        exception_id text PRIMARY KEY,
        tenant_id text NOT NULL,
        subject_kind text NOT NULL,
        subject_ref text NOT NULL,
        reason_code text NOT NULL,
        detail text NOT NULL,
        first_seen_at text NOT NULL,
        last_attempt_at text NOT NULL,
        attempts integer NOT NULL,
        resolved_at text NULL,
        resolution text NULL,
        CONSTRAINT ck_bss_rating__exception__subject_kind CHECK (subject_kind IN
            ('usage_record', 'child', 'fact', 'inbox', 'source_loss')),
        CONSTRAINT ck_bss_rating__exception__attempts CHECK (attempts >= 1),
        CONSTRAINT ck_bss_rating__exception__resolution CHECK ((resolved_at IS NULL) = (resolution IS NULL))
    )",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__exception__open_subject
        ON bss_rating__exception (tenant_id, subject_kind, subject_ref, reason_code)
        WHERE resolved_at IS NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__exception__open
        ON bss_rating__exception (tenant_id, exception_id) WHERE resolved_at IS NULL",
    r"CREATE TABLE IF NOT EXISTS bss_rating__operation (
        tenant_id text NOT NULL,
        actor text NOT NULL,
        idempotency_key text NOT NULL,
        operation_id text NOT NULL,
        request_digest text NOT NULL,
        response text NOT NULL,
        created_at text NOT NULL,
        PRIMARY KEY (tenant_id, actor, idempotency_key),
        CONSTRAINT ck_bss_rating__operation__key CHECK (length(idempotency_key) BETWEEN 1 AND 255),
        CONSTRAINT ck_bss_rating__operation__digest CHECK (length(request_digest) = 64)
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__operation__created_at
        ON bss_rating__operation (tenant_id, created_at)",
    r"CREATE TABLE IF NOT EXISTS bss_rating__operator_audit (
        tenant_id text NOT NULL,
        audit_id text NOT NULL,
        actor text NOT NULL,
        operation_id text NOT NULL,
        target_kind text NOT NULL,
        target_id text NOT NULL,
        request_digest text NOT NULL,
        reason text NOT NULL,
        approved_by text NULL,
        request_id text NULL,
        at text NOT NULL,
        PRIMARY KEY (tenant_id, audit_id),
        CONSTRAINT ck_bss_rating__operator_audit__digest CHECK (length(request_digest) = 64)
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__operator_audit__target
        ON bss_rating__operator_audit (tenant_id, target_kind, target_id)",
];

const DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss_rating__operator_audit",
    "DROP TABLE IF EXISTS bss_rating__operation",
    "DROP TABLE IF EXISTS bss_rating__exception",
    "DROP TABLE IF EXISTS bss_rating__inbox",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let dialects = super::Dialects {
            postgres: PG_UP,
            sqlite: SQLITE_UP,
        };
        super::exec_backend(self.name(), manager, dialects).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, super::Dialects::both(DOWN)).await
    }
}
