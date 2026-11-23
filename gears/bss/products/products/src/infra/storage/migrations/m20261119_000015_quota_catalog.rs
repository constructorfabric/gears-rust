//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-store:p1
//! Create the quota catalog store (P-D-274): adapter targets, capability snapshots, candidates,
//! mapping versions with their profile, definition versions with their resource types, and
//! availability observations (DESIGN §3.7).
//!
//! # Keys
//!
//! Every key leads with `tenant_id`, and every foreign key is tenant-qualified, so no child row can
//! name another tenant's parent. A target is unique per tenant, plugin instance and target key; a
//! snapshot per tenant, target and revision (P-D-268); a candidate per snapshot and capability key,
//! and a manual candidate per target and record id; a mapping version per lineage
//! `(target, capability_key)` and version, so two first approvals of one lineage cannot fork it,
//! and a definition version per candidate (P-D-270). The revision CHECK holds the canonical decimal
//! of at most 39 digits; the repository compares revisions as numbers (length, then text).
//!
//! # Append-only
//!
//! Every quota table but the candidate's refuses every `UPDATE`, `DELETE` and `TRUNCATE`: on
//! Postgres through one `PL/pgSQL` function per table, `bss.<table>_append_only()`, run by a row
//! trigger and by a statement trigger (a row trigger does not fire on `TRUNCATE`), as the derived
//! usage type versions do (`m20261001_000012`); on `SQLite`, which has no `TRUNCATE`, through two
//! triggers per table with fixed messages. A candidate is never deleted or truncated; until it is
//! decided, an `UPDATE` may move only its review columns (`state`, `version`, the proposal, the
//! submitter, the decider, their times and notes, and the approved definition):
//! `bss.products_quota_candidate_guard()` compares every content column with `IS DISTINCT FROM`,
//! as the audit log's guard does (`m20260925_000004`); `SQLite` uses `IS NOT`, its own null-safe
//! form. An approved or rejected candidate is frozen: every `UPDATE` of it is refused.
//!
//! On `SQLite`, `REPLACE` (and `INSERT OR REPLACE`) settles a key conflict by deleting the stored
//! row, which fires no delete trigger. So every key of every quota table has a `BEFORE INSERT`
//! trigger that refuses a row whose key is already stored, before any conflict is settled. Its
//! message is `SQLite`'s own text for a unique failure on that key, so a plain `INSERT` is refused
//! with the same text as without the trigger. The error itself is not the same: a trigger's abort
//! is `SQLITE_CONSTRAINT_TRIGGER`, which sea-orm does not classify, so the repositories' typed
//! refusals of a duplicate insert rest on toolkit-db's fallback that matches the unique failure's
//! message text. Every quota table is `WITHOUT ROWID`, so no statement can name an implicit row id
//! either: a `REPLACE` or an `UPDATE OR REPLACE` through `rowid` does not parse.
//!
//! # Listing indexes
//!
//! Each list reads the caller's tenant through a tenant-led index in the list's own order, so no
//! list scans or sorts another tenant's rows (P-D-274).
//!
//! # Backend differences
//!
//! `uuid`, `jsonb` and `timestamptz` become `text`, `bigint` becomes `integer`, the `bss.`
//! qualification is dropped, each regular-expression CHECK becomes `length` and `GLOB` tests, and
//! each table is `WITHOUT ROWID` (Postgres has no row id to name). Every key, index and CHECK is
//! on both sides.
//!
//! # Down
//!
//! Reversible only while the catalog is empty, so a revert never loses stored evidence (P-D-274):
//! `down` first reads every quota table that exists and refuses, naming each table that holds a
//! row, before it drops anything. On empty tables it drops them, children first, and on Postgres
//! the trigger functions.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_quota_adapter_target (
            tenant_id           uuid        NOT NULL,
            id                  uuid        NOT NULL,
            plugin_instance_id  text        NOT NULL,
            target_key          text        NOT NULL,
            name                text        NOT NULL,
            created_by          uuid        NOT NULL,
            created_at          timestamptz NOT NULL,
            CONSTRAINT products_quota_adapter_target_pkey PRIMARY KEY (tenant_id, id),
            CONSTRAINT chk_products_quota_adapter_target_key CHECK (length(target_key) BETWEEN 1 \
                AND 255),
            CONSTRAINT chk_products_quota_adapter_target_name CHECK (length(name) BETWEEN 1 AND 200)
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_adapter_target_binding ON \
     bss.products_quota_adapter_target USING btree (tenant_id, plugin_instance_id, target_key)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_adapter_target_name ON \
     bss.products_quota_adapter_target USING btree (tenant_id, name, id)",
    "CREATE TABLE IF NOT EXISTS bss.products_quota_snapshot (
            tenant_id        uuid        NOT NULL,
            id               uuid        NOT NULL,
            target_id        uuid        NOT NULL,
            snapshot_id      uuid        NOT NULL,
            source_revision  text        NOT NULL,
            observed_at      timestamptz NOT NULL,
            item_count       bigint      NOT NULL,
            content_digest   text        NOT NULL,
            snapshot_json    jsonb       NOT NULL,
            evidence_json    jsonb       NOT NULL,
            review_json      jsonb       NOT NULL,
            created_by       uuid        NOT NULL,
            created_at       timestamptz NOT NULL,
            CONSTRAINT products_quota_snapshot_pkey PRIMARY KEY (tenant_id, id),
            CONSTRAINT fk_products_quota_snapshot_target FOREIGN KEY (tenant_id, target_id)
                REFERENCES bss.products_quota_adapter_target (tenant_id, id),
            CONSTRAINT chk_products_quota_snapshot_revision CHECK (source_revision ~ \
                '^(0|[1-9][0-9]{0,38})$'),
            CONSTRAINT chk_products_quota_snapshot_count CHECK (item_count >= 0),
            CONSTRAINT chk_products_quota_snapshot_digest CHECK (content_digest ~ '^[0-9a-f]{64}$')
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_snapshot_revision ON \
     bss.products_quota_snapshot USING btree (tenant_id, target_id, source_revision)",
    "CREATE TABLE IF NOT EXISTS bss.products_quota_candidate (
            tenant_id               uuid        NOT NULL,
            id                      uuid        NOT NULL,
            target_id               uuid        NOT NULL,
            origin                  text        NOT NULL,
            snapshot_row_id         uuid        NULL,
            record_id               uuid        NOT NULL,
            capability_key          text        NOT NULL,
            record_json             jsonb       NOT NULL,
            record_digest           text        NOT NULL,
            evidence_json           jsonb       NOT NULL,
            state                   text        NOT NULL,
            version                 bigint      NOT NULL,
            proposal_json           jsonb       NULL,
            proposal_evidence_json  jsonb       NULL,
            created_by              uuid        NOT NULL,
            created_at              timestamptz NOT NULL,
            submitted_by            uuid        NULL,
            submitted_at            timestamptz NULL,
            submit_note             text        NULL,
            decided_by              uuid        NULL,
            decided_at              timestamptz NULL,
            decided_note            text        NULL,
            definition_id           uuid        NULL,
            definition_version      bigint      NULL,
            CONSTRAINT products_quota_candidate_pkey PRIMARY KEY (tenant_id, id),
            CONSTRAINT fk_products_quota_candidate_target FOREIGN KEY (tenant_id, target_id)
                REFERENCES bss.products_quota_adapter_target (tenant_id, id),
            CONSTRAINT fk_products_quota_candidate_snapshot FOREIGN KEY (tenant_id, snapshot_row_id)
                REFERENCES bss.products_quota_snapshot (tenant_id, id),
            CONSTRAINT chk_products_quota_candidate_origin CHECK (origin IN ('discovery', \
                'manual')),
            CONSTRAINT chk_products_quota_candidate_origin_snapshot CHECK ((origin = 'discovery') \
                = (snapshot_row_id IS NOT NULL)),
            CONSTRAINT chk_products_quota_candidate_state CHECK (state IN ('staged', 'submitted', \
                'approved', 'rejected')),
            CONSTRAINT chk_products_quota_candidate_version CHECK (version >= 1),
            CONSTRAINT chk_products_quota_candidate_digest CHECK (record_digest ~ '^[0-9a-f]{64}$'),
            CONSTRAINT chk_products_quota_candidate_key CHECK (length(capability_key) BETWEEN 1 \
                AND 255)
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_candidate_snapshot_key ON \
     bss.products_quota_candidate USING btree (tenant_id, snapshot_row_id, capability_key) WHERE \
     snapshot_row_id IS NOT NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_candidate_manual_record ON \
     bss.products_quota_candidate USING btree (tenant_id, target_id, record_id) WHERE origin = \
     'manual'",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_candidate_state ON bss.products_quota_candidate \
     USING btree (tenant_id, state, created_at, id)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_candidate_target ON \
     bss.products_quota_candidate USING btree (tenant_id, target_id, created_at, id)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_candidate_created ON \
     bss.products_quota_candidate USING btree (tenant_id, created_at, id)",
    "CREATE TABLE IF NOT EXISTS bss.products_quota_mapping_version (
            tenant_id           uuid        NOT NULL,
            mapping_id          uuid        NOT NULL,
            version             bigint      NOT NULL,
            target_id           uuid        NOT NULL,
            capability_key      text        NOT NULL,
            plugin_instance_id  text        NOT NULL,
            target_key          text        NOT NULL,
            mapping_key         text        NOT NULL,
            mapping_json        jsonb       NOT NULL,
            digest              text        NOT NULL,
            profile_id          uuid        NOT NULL,
            profile_version     bigint      NOT NULL,
            profile_json        jsonb       NOT NULL,
            profile_digest      text        NOT NULL,
            created_by          uuid        NOT NULL,
            created_at          timestamptz NOT NULL,
            CONSTRAINT products_quota_mapping_version_pkey PRIMARY KEY (tenant_id, mapping_id, \
                version),
            CONSTRAINT fk_products_quota_mapping_version_target FOREIGN KEY (tenant_id, target_id)
                REFERENCES bss.products_quota_adapter_target (tenant_id, id),
            CONSTRAINT chk_products_quota_mapping_version_version CHECK (version >= 1 AND \
                profile_version >= 1),
            CONSTRAINT chk_products_quota_mapping_version_digest CHECK (digest ~ '^[0-9a-f]{64}$' \
                AND profile_digest ~ '^[0-9a-f]{64}$')
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_mapping_version_lineage ON \
     bss.products_quota_mapping_version USING btree (tenant_id, target_id, capability_key, \
     version)",
    "CREATE TABLE IF NOT EXISTS bss.products_quota_definition_version (
            tenant_id          uuid        NOT NULL,
            definition_id      uuid        NOT NULL,
            version            bigint      NOT NULL,
            candidate_id       uuid        NOT NULL,
            sku_id             uuid        NOT NULL,
            sku_version        bigint      NOT NULL,
            metric_uuid        uuid        NOT NULL,
            subject_type_uuid  uuid        NOT NULL,
            profile_id         uuid        NOT NULL,
            mapping_id         uuid        NOT NULL,
            mapping_version    bigint      NOT NULL,
            content_json       jsonb       NOT NULL,
            digest             text        NOT NULL,
            created_by         uuid        NOT NULL,
            created_at         timestamptz NOT NULL,
            CONSTRAINT products_quota_definition_version_pkey PRIMARY KEY (tenant_id, \
                definition_id, version),
            CONSTRAINT fk_products_quota_definition_version_mapping FOREIGN KEY (tenant_id, \
                mapping_id, mapping_version)
                REFERENCES bss.products_quota_mapping_version (tenant_id, mapping_id, version),
            CONSTRAINT fk_products_quota_definition_version_candidate FOREIGN KEY (tenant_id, \
                candidate_id)
                REFERENCES bss.products_quota_candidate (tenant_id, id),
            CONSTRAINT chk_products_quota_definition_version_version CHECK (version >= 1 AND \
                sku_version >= 1),
            CONSTRAINT chk_products_quota_definition_version_digest CHECK (digest ~ \
                '^[0-9a-f]{64}$')
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_definition_version_candidate ON \
     bss.products_quota_definition_version USING btree (tenant_id, candidate_id)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_definition_version_sku ON \
     bss.products_quota_definition_version USING btree (tenant_id, sku_id, sku_version)",
    "CREATE TABLE IF NOT EXISTS bss.products_quota_definition_resource_type (
            tenant_id           uuid   NOT NULL,
            definition_id       uuid   NOT NULL,
            version             bigint NOT NULL,
            resource_type_uuid  uuid   NOT NULL,
            CONSTRAINT products_quota_definition_resource_type_pkey
                PRIMARY KEY (tenant_id, definition_id, version, resource_type_uuid),
            CONSTRAINT fk_products_quota_definition_resource_type_definition FOREIGN KEY \
                (tenant_id, definition_id, version)
                REFERENCES bss.products_quota_definition_version (tenant_id, definition_id, version)
        )",
    "CREATE TABLE IF NOT EXISTS bss.products_quota_availability (
            tenant_id              uuid        NOT NULL,
            definition_id          uuid        NOT NULL,
            version                bigint      NOT NULL,
            revision               bigint      NOT NULL,
            state                  text        NOT NULL,
            reason_code            text        NULL,
            qualification_id       uuid        NULL,
            qualification_version  bigint      NULL,
            qualification_digest   text        NULL,
            observed_at            timestamptz NOT NULL,
            decided_by             uuid        NOT NULL,
            CONSTRAINT products_quota_availability_pkey PRIMARY KEY (tenant_id, definition_id, \
                version, revision),
            CONSTRAINT fk_products_quota_availability_definition FOREIGN KEY (tenant_id, \
                definition_id, version)
                REFERENCES bss.products_quota_definition_version (tenant_id, definition_id, \
                    version),
            CONSTRAINT chk_products_quota_availability_state CHECK (state IN ('enabled', \
                'disabled', 'withdrawn')),
            CONSTRAINT chk_products_quota_availability_reason CHECK ((state = 'enabled') = \
                (reason_code IS NULL)),
            CONSTRAINT chk_products_quota_availability_revision CHECK (revision >= 1),
            CONSTRAINT chk_products_quota_availability_qualification CHECK (
                (qualification_id IS NULL) = (qualification_version IS NULL)
                AND (qualification_id IS NULL) = (qualification_digest IS NULL))
        )",
    "CREATE OR REPLACE FUNCTION bss.products_quota_adapter_target_append_only() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_quota_adapter_target is append-only: % \
     is not permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_quota_adapter_target_append_only ON \
     bss.products_quota_adapter_target",
    "CREATE TRIGGER products_quota_adapter_target_append_only BEFORE UPDATE OR DELETE ON \
     bss.products_quota_adapter_target FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_adapter_target_append_only()",
    "DROP TRIGGER IF EXISTS products_quota_adapter_target_no_truncate ON \
     bss.products_quota_adapter_target",
    "CREATE TRIGGER products_quota_adapter_target_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_adapter_target FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_adapter_target_append_only()",
    "CREATE OR REPLACE FUNCTION bss.products_quota_snapshot_append_only() RETURNS trigger LANGUAGE \
     plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_quota_snapshot is append-only: % is not \
     permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_quota_snapshot_append_only ON bss.products_quota_snapshot",
    "CREATE TRIGGER products_quota_snapshot_append_only BEFORE UPDATE OR DELETE ON \
     bss.products_quota_snapshot FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_snapshot_append_only()",
    "DROP TRIGGER IF EXISTS products_quota_snapshot_no_truncate ON \
     bss.products_quota_snapshot",
    "CREATE TRIGGER products_quota_snapshot_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_snapshot FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_snapshot_append_only()",
    "CREATE OR REPLACE FUNCTION bss.products_quota_mapping_version_append_only() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_quota_mapping_version is append-only: \
     % is not permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_quota_mapping_version_append_only ON \
     bss.products_quota_mapping_version",
    "CREATE TRIGGER products_quota_mapping_version_append_only BEFORE UPDATE OR DELETE ON \
     bss.products_quota_mapping_version FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_mapping_version_append_only()",
    "DROP TRIGGER IF EXISTS products_quota_mapping_version_no_truncate ON \
     bss.products_quota_mapping_version",
    "CREATE TRIGGER products_quota_mapping_version_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_mapping_version FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_mapping_version_append_only()",
    "CREATE OR REPLACE FUNCTION bss.products_quota_definition_version_append_only() RETURNS \
     trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_quota_definition_version is \
     append-only: % is not permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_quota_definition_version_append_only ON \
     bss.products_quota_definition_version",
    "CREATE TRIGGER products_quota_definition_version_append_only BEFORE UPDATE OR DELETE ON \
     bss.products_quota_definition_version FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_definition_version_append_only()",
    "DROP TRIGGER IF EXISTS products_quota_definition_version_no_truncate ON \
     bss.products_quota_definition_version",
    "CREATE TRIGGER products_quota_definition_version_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_definition_version FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_definition_version_append_only()",
    "CREATE OR REPLACE FUNCTION bss.products_quota_definition_resource_type_append_only() RETURNS \
     trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_quota_definition_resource_type \
     is append-only: % is not permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_quota_definition_resource_type_append_only ON \
     bss.products_quota_definition_resource_type",
    "CREATE TRIGGER products_quota_definition_resource_type_append_only BEFORE UPDATE OR DELETE ON \
     bss.products_quota_definition_resource_type FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_definition_resource_type_append_only()",
    "DROP TRIGGER IF EXISTS products_quota_definition_resource_type_no_truncate ON \
     bss.products_quota_definition_resource_type",
    "CREATE TRIGGER products_quota_definition_resource_type_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_definition_resource_type FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_definition_resource_type_append_only()",
    "CREATE OR REPLACE FUNCTION bss.products_quota_availability_append_only() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_quota_availability is append-only: % \
     is not permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_quota_availability_append_only ON \
     bss.products_quota_availability",
    "CREATE TRIGGER products_quota_availability_append_only BEFORE UPDATE OR DELETE ON \
     bss.products_quota_availability FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_availability_append_only()",
    "DROP TRIGGER IF EXISTS products_quota_availability_no_truncate ON \
     bss.products_quota_availability",
    "CREATE TRIGGER products_quota_availability_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_availability FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_availability_append_only()",
    "CREATE OR REPLACE FUNCTION bss.products_quota_candidate_guard() RETURNS trigger AS $$
        BEGIN
          IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
            RAISE EXCEPTION 'products_quota_candidate is append-only: % is not permitted', TG_OP;
          END IF;

          IF OLD.state IN ('approved', 'rejected') THEN
            RAISE EXCEPTION 'products_quota_candidate is decided: UPDATE is not permitted';
          END IF;

          IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
             OR NEW.id IS DISTINCT FROM OLD.id
             OR NEW.target_id IS DISTINCT FROM OLD.target_id
             OR NEW.origin IS DISTINCT FROM OLD.origin
             OR NEW.snapshot_row_id IS DISTINCT FROM OLD.snapshot_row_id
             OR NEW.record_id IS DISTINCT FROM OLD.record_id
             OR NEW.capability_key IS DISTINCT FROM OLD.capability_key
             OR NEW.record_json IS DISTINCT FROM OLD.record_json
             OR NEW.record_digest IS DISTINCT FROM OLD.record_digest
             OR NEW.evidence_json IS DISTINCT FROM OLD.evidence_json
             OR NEW.created_by IS DISTINCT FROM OLD.created_by
             OR NEW.created_at IS DISTINCT FROM OLD.created_at
          THEN
            RAISE EXCEPTION 'products_quota_candidate content is immutable';
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "DROP TRIGGER IF EXISTS products_quota_candidate_guard ON bss.products_quota_candidate",
    "CREATE TRIGGER products_quota_candidate_guard BEFORE UPDATE OR DELETE ON \
     bss.products_quota_candidate FOR EACH ROW EXECUTE FUNCTION \
     bss.products_quota_candidate_guard()",
    "DROP TRIGGER IF EXISTS products_quota_candidate_no_truncate ON bss.products_quota_candidate",
    "CREATE TRIGGER products_quota_candidate_no_truncate BEFORE TRUNCATE ON \
     bss.products_quota_candidate FOR EACH STATEMENT EXECUTE FUNCTION \
     bss.products_quota_candidate_guard()",
];

const PG_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss.products_quota_availability",
    "DROP TABLE IF EXISTS bss.products_quota_definition_resource_type",
    "DROP TABLE IF EXISTS bss.products_quota_definition_version",
    "DROP TABLE IF EXISTS bss.products_quota_mapping_version",
    "DROP TABLE IF EXISTS bss.products_quota_candidate",
    "DROP TABLE IF EXISTS bss.products_quota_snapshot",
    "DROP TABLE IF EXISTS bss.products_quota_adapter_target",
    "DROP FUNCTION IF EXISTS bss.products_quota_availability_append_only()",
    "DROP FUNCTION IF EXISTS bss.products_quota_definition_resource_type_append_only()",
    "DROP FUNCTION IF EXISTS bss.products_quota_definition_version_append_only()",
    "DROP FUNCTION IF EXISTS bss.products_quota_mapping_version_append_only()",
    "DROP FUNCTION IF EXISTS bss.products_quota_candidate_guard()",
    "DROP FUNCTION IF EXISTS bss.products_quota_snapshot_append_only()",
    "DROP FUNCTION IF EXISTS bss.products_quota_adapter_target_append_only()",
];

const SQLITE_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_quota_adapter_target (
            tenant_id           text NOT NULL,
            id                  text NOT NULL,
            plugin_instance_id  text NOT NULL,
            target_key          text NOT NULL,
            name                text NOT NULL,
            created_by          text NOT NULL,
            created_at          text NOT NULL,
            PRIMARY KEY (tenant_id, id),
            CONSTRAINT chk_products_quota_adapter_target_key CHECK (length(target_key) BETWEEN 1 \
                AND 255),
            CONSTRAINT chk_products_quota_adapter_target_name CHECK (length(name) BETWEEN 1 AND 200)
        ) WITHOUT ROWID",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_adapter_target_binding ON \
     products_quota_adapter_target (tenant_id, plugin_instance_id, target_key)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_adapter_target_name ON \
     products_quota_adapter_target (tenant_id, name, id)",
    "CREATE TABLE IF NOT EXISTS products_quota_snapshot (
            tenant_id        text    NOT NULL,
            id               text    NOT NULL,
            target_id        text    NOT NULL,
            snapshot_id      text    NOT NULL,
            source_revision  text    NOT NULL,
            observed_at      text    NOT NULL,
            item_count       integer NOT NULL,
            content_digest   text    NOT NULL,
            snapshot_json    text    NOT NULL,
            evidence_json    text    NOT NULL,
            review_json      text    NOT NULL,
            created_by       text    NOT NULL,
            created_at       text    NOT NULL,
            PRIMARY KEY (tenant_id, id),
            CONSTRAINT fk_products_quota_snapshot_target FOREIGN KEY (tenant_id, target_id)
                REFERENCES products_quota_adapter_target (tenant_id, id),
            CONSTRAINT chk_products_quota_snapshot_revision CHECK (
                source_revision = '0'
                OR (
                    length(source_revision) BETWEEN 1 AND 39
                    AND source_revision GLOB '[1-9]*'
                    AND source_revision NOT GLOB '*[^0-9]*'
                )
            ),
            CONSTRAINT chk_products_quota_snapshot_count CHECK (item_count >= 0),
            CONSTRAINT chk_products_quota_snapshot_digest CHECK (
                length(content_digest) = 64 AND content_digest NOT GLOB '*[^0-9a-f]*'
            )
        ) WITHOUT ROWID",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_snapshot_revision ON \
     products_quota_snapshot (tenant_id, target_id, source_revision)",
    "CREATE TABLE IF NOT EXISTS products_quota_candidate (
            tenant_id               text    NOT NULL,
            id                      text    NOT NULL,
            target_id               text    NOT NULL,
            origin                  text    NOT NULL,
            snapshot_row_id         text    NULL,
            record_id               text    NOT NULL,
            capability_key          text    NOT NULL,
            record_json             text    NOT NULL,
            record_digest           text    NOT NULL,
            evidence_json           text    NOT NULL,
            state                   text    NOT NULL,
            version                 integer NOT NULL,
            proposal_json           text    NULL,
            proposal_evidence_json  text    NULL,
            created_by              text    NOT NULL,
            created_at              text    NOT NULL,
            submitted_by            text    NULL,
            submitted_at            text    NULL,
            submit_note             text    NULL,
            decided_by              text    NULL,
            decided_at              text    NULL,
            decided_note            text    NULL,
            definition_id           text    NULL,
            definition_version      integer NULL,
            PRIMARY KEY (tenant_id, id),
            CONSTRAINT fk_products_quota_candidate_target FOREIGN KEY (tenant_id, target_id)
                REFERENCES products_quota_adapter_target (tenant_id, id),
            CONSTRAINT fk_products_quota_candidate_snapshot FOREIGN KEY (tenant_id, snapshot_row_id)
                REFERENCES products_quota_snapshot (tenant_id, id),
            CONSTRAINT chk_products_quota_candidate_origin CHECK (origin IN ('discovery', \
                'manual')),
            CONSTRAINT chk_products_quota_candidate_origin_snapshot CHECK ((origin = 'discovery') \
                = (snapshot_row_id IS NOT NULL)),
            CONSTRAINT chk_products_quota_candidate_state CHECK (state IN ('staged', 'submitted', \
                'approved', 'rejected')),
            CONSTRAINT chk_products_quota_candidate_version CHECK (version >= 1),
            CONSTRAINT chk_products_quota_candidate_digest CHECK (
                length(record_digest) = 64 AND record_digest NOT GLOB '*[^0-9a-f]*'
            ),
            CONSTRAINT chk_products_quota_candidate_key CHECK (length(capability_key) BETWEEN 1 \
                AND 255)
        ) WITHOUT ROWID",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_candidate_snapshot_key ON \
     products_quota_candidate (tenant_id, snapshot_row_id, capability_key) WHERE snapshot_row_id \
     IS NOT NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_candidate_manual_record ON \
     products_quota_candidate (tenant_id, target_id, record_id) WHERE origin = 'manual'",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_candidate_state ON products_quota_candidate \
     (tenant_id, state, created_at, id)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_candidate_target ON products_quota_candidate \
     (tenant_id, target_id, created_at, id)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_candidate_created ON products_quota_candidate \
     (tenant_id, created_at, id)",
    "CREATE TABLE IF NOT EXISTS products_quota_mapping_version (
            tenant_id           text    NOT NULL,
            mapping_id          text    NOT NULL,
            version             integer NOT NULL,
            target_id           text    NOT NULL,
            capability_key      text    NOT NULL,
            plugin_instance_id  text    NOT NULL,
            target_key          text    NOT NULL,
            mapping_key         text    NOT NULL,
            mapping_json        text    NOT NULL,
            digest              text    NOT NULL,
            profile_id          text    NOT NULL,
            profile_version     integer NOT NULL,
            profile_json        text    NOT NULL,
            profile_digest      text    NOT NULL,
            created_by          text    NOT NULL,
            created_at          text    NOT NULL,
            PRIMARY KEY (tenant_id, mapping_id, version),
            CONSTRAINT fk_products_quota_mapping_version_target FOREIGN KEY (tenant_id, target_id)
                REFERENCES products_quota_adapter_target (tenant_id, id),
            CONSTRAINT chk_products_quota_mapping_version_version CHECK (version >= 1 AND \
                profile_version >= 1),
            CONSTRAINT chk_products_quota_mapping_version_digest CHECK (
                length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'
                AND length(profile_digest) = 64 AND profile_digest NOT GLOB '*[^0-9a-f]*'
            )
        ) WITHOUT ROWID",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_mapping_version_lineage ON \
     products_quota_mapping_version (tenant_id, target_id, capability_key, version)",
    "CREATE TABLE IF NOT EXISTS products_quota_definition_version (
            tenant_id          text    NOT NULL,
            definition_id      text    NOT NULL,
            version            integer NOT NULL,
            candidate_id       text    NOT NULL,
            sku_id             text    NOT NULL,
            sku_version        integer NOT NULL,
            metric_uuid        text    NOT NULL,
            subject_type_uuid  text    NOT NULL,
            profile_id         text    NOT NULL,
            mapping_id         text    NOT NULL,
            mapping_version    integer NOT NULL,
            content_json       text    NOT NULL,
            digest             text    NOT NULL,
            created_by         text    NOT NULL,
            created_at         text    NOT NULL,
            PRIMARY KEY (tenant_id, definition_id, version),
            CONSTRAINT fk_products_quota_definition_version_mapping FOREIGN KEY (tenant_id, \
                mapping_id, mapping_version)
                REFERENCES products_quota_mapping_version (tenant_id, mapping_id, version),
            CONSTRAINT fk_products_quota_definition_version_candidate FOREIGN KEY (tenant_id, \
                candidate_id)
                REFERENCES products_quota_candidate (tenant_id, id),
            CONSTRAINT chk_products_quota_definition_version_version CHECK (version >= 1 AND \
                sku_version >= 1),
            CONSTRAINT chk_products_quota_definition_version_digest CHECK (
                length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'
            )
        ) WITHOUT ROWID",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_quota_definition_version_candidate ON \
     products_quota_definition_version (tenant_id, candidate_id)",
    "CREATE INDEX IF NOT EXISTS idx_products_quota_definition_version_sku ON \
     products_quota_definition_version (tenant_id, sku_id, sku_version)",
    "CREATE TABLE IF NOT EXISTS products_quota_definition_resource_type (
            tenant_id           text    NOT NULL,
            definition_id       text    NOT NULL,
            version             integer NOT NULL,
            resource_type_uuid  text    NOT NULL,
            PRIMARY KEY (tenant_id, definition_id, version, resource_type_uuid),
            CONSTRAINT fk_products_quota_definition_resource_type_definition FOREIGN KEY \
                (tenant_id, definition_id, version)
                REFERENCES products_quota_definition_version (tenant_id, definition_id, version)
        ) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS products_quota_availability (
            tenant_id              text    NOT NULL,
            definition_id          text    NOT NULL,
            version                integer NOT NULL,
            revision               integer NOT NULL,
            state                  text    NOT NULL,
            reason_code            text    NULL,
            qualification_id       text    NULL,
            qualification_version  integer NULL,
            qualification_digest   text    NULL,
            observed_at            text    NOT NULL,
            decided_by             text    NOT NULL,
            PRIMARY KEY (tenant_id, definition_id, version, revision),
            CONSTRAINT fk_products_quota_availability_definition FOREIGN KEY (tenant_id, \
                definition_id, version)
                REFERENCES products_quota_definition_version (tenant_id, definition_id, version),
            CONSTRAINT chk_products_quota_availability_state CHECK (state IN ('enabled', \
                'disabled', 'withdrawn')),
            CONSTRAINT chk_products_quota_availability_reason CHECK ((state = 'enabled') = \
                (reason_code IS NULL)),
            CONSTRAINT chk_products_quota_availability_revision CHECK (revision >= 1),
            CONSTRAINT chk_products_quota_availability_qualification CHECK (
                (qualification_id IS NULL) = (qualification_version IS NULL)
                AND (qualification_id IS NULL) = (qualification_digest IS NULL))
        ) WITHOUT ROWID",
    "DROP TRIGGER IF EXISTS products_quota_adapter_target_no_update",
    "CREATE TRIGGER products_quota_adapter_target_no_update BEFORE UPDATE ON \
     products_quota_adapter_target BEGIN SELECT RAISE(ABORT, 'products_quota_adapter_target is \
     append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_adapter_target_no_delete",
    "CREATE TRIGGER products_quota_adapter_target_no_delete BEFORE DELETE ON \
     products_quota_adapter_target BEGIN SELECT RAISE(ABORT, 'products_quota_adapter_target is \
     append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_snapshot_no_update",
    "CREATE TRIGGER products_quota_snapshot_no_update BEFORE UPDATE ON products_quota_snapshot \
     BEGIN SELECT RAISE(ABORT, 'products_quota_snapshot is append-only: UPDATE is not permitted'); \
     END",
    "DROP TRIGGER IF EXISTS products_quota_snapshot_no_delete",
    "CREATE TRIGGER products_quota_snapshot_no_delete BEFORE DELETE ON products_quota_snapshot \
     BEGIN SELECT RAISE(ABORT, 'products_quota_snapshot is append-only: DELETE is not permitted'); \
     END",
    "DROP TRIGGER IF EXISTS products_quota_mapping_version_no_update",
    "CREATE TRIGGER products_quota_mapping_version_no_update BEFORE UPDATE ON \
     products_quota_mapping_version BEGIN SELECT RAISE(ABORT, 'products_quota_mapping_version is \
     append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_mapping_version_no_delete",
    "CREATE TRIGGER products_quota_mapping_version_no_delete BEFORE DELETE ON \
     products_quota_mapping_version BEGIN SELECT RAISE(ABORT, 'products_quota_mapping_version is \
     append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_version_no_update",
    "CREATE TRIGGER products_quota_definition_version_no_update BEFORE UPDATE ON \
     products_quota_definition_version BEGIN SELECT RAISE(ABORT, \
     'products_quota_definition_version is append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_version_no_delete",
    "CREATE TRIGGER products_quota_definition_version_no_delete BEFORE DELETE ON \
     products_quota_definition_version BEGIN SELECT RAISE(ABORT, \
     'products_quota_definition_version is append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_resource_type_no_update",
    "CREATE TRIGGER products_quota_definition_resource_type_no_update BEFORE UPDATE ON \
     products_quota_definition_resource_type BEGIN SELECT RAISE(ABORT, \
     'products_quota_definition_resource_type is append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_resource_type_no_delete",
    "CREATE TRIGGER products_quota_definition_resource_type_no_delete BEFORE DELETE ON \
     products_quota_definition_resource_type BEGIN SELECT RAISE(ABORT, \
     'products_quota_definition_resource_type is append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_availability_no_update",
    "CREATE TRIGGER products_quota_availability_no_update BEFORE UPDATE ON \
     products_quota_availability BEGIN SELECT RAISE(ABORT, 'products_quota_availability is \
     append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_availability_no_delete",
    "CREATE TRIGGER products_quota_availability_no_delete BEFORE DELETE ON \
     products_quota_availability BEGIN SELECT RAISE(ABORT, 'products_quota_availability is \
     append-only: DELETE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_candidate_no_delete",
    "CREATE TRIGGER products_quota_candidate_no_delete BEFORE DELETE ON products_quota_candidate \
     BEGIN SELECT RAISE(ABORT, 'products_quota_candidate is append-only: DELETE is not \
     permitted'); END",
    "DROP TRIGGER IF EXISTS products_quota_candidate_content",
    "CREATE TRIGGER products_quota_candidate_content BEFORE UPDATE ON products_quota_candidate FOR \
     EACH ROW WHEN (
            NEW.tenant_id IS NOT OLD.tenant_id
            OR NEW.id IS NOT OLD.id
            OR NEW.target_id IS NOT OLD.target_id
            OR NEW.origin IS NOT OLD.origin
            OR NEW.snapshot_row_id IS NOT OLD.snapshot_row_id
            OR NEW.record_id IS NOT OLD.record_id
            OR NEW.capability_key IS NOT OLD.capability_key
            OR NEW.record_json IS NOT OLD.record_json
            OR NEW.record_digest IS NOT OLD.record_digest
            OR NEW.evidence_json IS NOT OLD.evidence_json
            OR NEW.created_by IS NOT OLD.created_by
            OR NEW.created_at IS NOT OLD.created_at
        ) BEGIN SELECT RAISE(ABORT, 'products_quota_candidate content is immutable'); END",
    "DROP TRIGGER IF EXISTS products_quota_adapter_target_no_replace",
    "CREATE TRIGGER products_quota_adapter_target_no_replace BEFORE INSERT ON \
     products_quota_adapter_target FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_adapter_target WHERE tenant_id = NEW.tenant_id AND id = NEW.id) BEGIN SELECT \
     RAISE(ABORT, 'UNIQUE constraint failed: products_quota_adapter_target.tenant_id, \
     products_quota_adapter_target.id (products_quota_adapter_target is append-only: a stored row \
     is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_adapter_target_no_replace_binding",
    "CREATE TRIGGER products_quota_adapter_target_no_replace_binding BEFORE INSERT ON \
     products_quota_adapter_target FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_adapter_target WHERE tenant_id = NEW.tenant_id AND plugin_instance_id = \
     NEW.plugin_instance_id AND target_key = NEW.target_key) BEGIN SELECT RAISE(ABORT, 'UNIQUE \
     constraint failed: products_quota_adapter_target.tenant_id, \
     products_quota_adapter_target.plugin_instance_id, products_quota_adapter_target.target_key \
     (products_quota_adapter_target is append-only: a stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_snapshot_no_replace",
    "CREATE TRIGGER products_quota_snapshot_no_replace BEFORE INSERT ON products_quota_snapshot \
     FOR EACH ROW WHEN EXISTS (SELECT 1 FROM products_quota_snapshot WHERE tenant_id = \
     NEW.tenant_id AND id = NEW.id) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
     products_quota_snapshot.tenant_id, products_quota_snapshot.id (products_quota_snapshot is \
     append-only: a stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_snapshot_no_replace_revision",
    "CREATE TRIGGER products_quota_snapshot_no_replace_revision BEFORE INSERT ON \
     products_quota_snapshot FOR EACH ROW WHEN EXISTS (SELECT 1 FROM products_quota_snapshot \
     WHERE tenant_id = NEW.tenant_id AND target_id = NEW.target_id AND source_revision = \
     NEW.source_revision) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
     products_quota_snapshot.tenant_id, products_quota_snapshot.target_id, \
     products_quota_snapshot.source_revision (products_quota_snapshot is append-only: a stored \
     row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_candidate_no_replace",
    "CREATE TRIGGER products_quota_candidate_no_replace BEFORE INSERT ON products_quota_candidate \
     FOR EACH ROW WHEN EXISTS (SELECT 1 FROM products_quota_candidate WHERE tenant_id = \
     NEW.tenant_id AND id = NEW.id) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
     products_quota_candidate.tenant_id, products_quota_candidate.id (products_quota_candidate is \
     append-only: a stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_candidate_no_replace_snapshot_key",
    "CREATE TRIGGER products_quota_candidate_no_replace_snapshot_key BEFORE INSERT ON \
     products_quota_candidate FOR EACH ROW WHEN EXISTS (SELECT 1 FROM products_quota_candidate \
     WHERE tenant_id = NEW.tenant_id AND snapshot_row_id = NEW.snapshot_row_id AND capability_key \
     = NEW.capability_key) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
     products_quota_candidate.tenant_id, products_quota_candidate.snapshot_row_id, \
     products_quota_candidate.capability_key (products_quota_candidate is append-only: a stored \
     row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_candidate_no_replace_manual_record",
    "CREATE TRIGGER products_quota_candidate_no_replace_manual_record BEFORE INSERT ON \
     products_quota_candidate FOR EACH ROW WHEN EXISTS (SELECT 1 FROM products_quota_candidate \
     WHERE origin = 'manual' AND NEW.origin = 'manual' AND tenant_id = NEW.tenant_id AND \
     target_id = NEW.target_id AND record_id = NEW.record_id) BEGIN SELECT RAISE(ABORT, 'UNIQUE \
     constraint failed: products_quota_candidate.tenant_id, products_quota_candidate.target_id, \
     products_quota_candidate.record_id (products_quota_candidate is append-only: a stored row is \
     never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_mapping_version_no_replace",
    "CREATE TRIGGER products_quota_mapping_version_no_replace BEFORE INSERT ON \
     products_quota_mapping_version FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_mapping_version WHERE tenant_id = NEW.tenant_id AND mapping_id = \
     NEW.mapping_id AND version = NEW.version) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint \
     failed: products_quota_mapping_version.tenant_id, products_quota_mapping_version.mapping_id, \
     products_quota_mapping_version.version (products_quota_mapping_version is append-only: a \
     stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_mapping_version_no_replace_lineage",
    "CREATE TRIGGER products_quota_mapping_version_no_replace_lineage BEFORE INSERT ON \
     products_quota_mapping_version FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_mapping_version WHERE tenant_id = NEW.tenant_id AND target_id = \
     NEW.target_id AND capability_key = NEW.capability_key AND version = NEW.version) BEGIN \
     SELECT RAISE(ABORT, 'UNIQUE constraint failed: products_quota_mapping_version.tenant_id, \
     products_quota_mapping_version.target_id, products_quota_mapping_version.capability_key, \
     products_quota_mapping_version.version (products_quota_mapping_version is append-only: a \
     stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_version_no_replace",
    "CREATE TRIGGER products_quota_definition_version_no_replace BEFORE INSERT ON \
     products_quota_definition_version FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_definition_version WHERE tenant_id = NEW.tenant_id AND definition_id = \
     NEW.definition_id AND version = NEW.version) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint \
     failed: products_quota_definition_version.tenant_id, \
     products_quota_definition_version.definition_id, products_quota_definition_version.version \
     (products_quota_definition_version is append-only: a stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_version_no_replace_candidate",
    "CREATE TRIGGER products_quota_definition_version_no_replace_candidate BEFORE INSERT ON \
     products_quota_definition_version FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_definition_version WHERE tenant_id = NEW.tenant_id AND candidate_id = \
     NEW.candidate_id) BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
     products_quota_definition_version.tenant_id, products_quota_definition_version.candidate_id \
     (products_quota_definition_version is append-only: a stored row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_definition_resource_type_no_replace",
    "CREATE TRIGGER products_quota_definition_resource_type_no_replace BEFORE INSERT ON \
     products_quota_definition_resource_type FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_definition_resource_type WHERE tenant_id = NEW.tenant_id AND definition_id = \
     NEW.definition_id AND version = NEW.version AND resource_type_uuid = NEW.resource_type_uuid) \
     BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
     products_quota_definition_resource_type.tenant_id, \
     products_quota_definition_resource_type.definition_id, \
     products_quota_definition_resource_type.version, \
     products_quota_definition_resource_type.resource_type_uuid \
     (products_quota_definition_resource_type is append-only: a stored row is never replaced)'); \
     END",
    "DROP TRIGGER IF EXISTS products_quota_availability_no_replace",
    "CREATE TRIGGER products_quota_availability_no_replace BEFORE INSERT ON \
     products_quota_availability FOR EACH ROW WHEN EXISTS (SELECT 1 FROM \
     products_quota_availability WHERE tenant_id = NEW.tenant_id AND definition_id = \
     NEW.definition_id AND version = NEW.version AND revision = NEW.revision) BEGIN SELECT \
     RAISE(ABORT, 'UNIQUE constraint failed: products_quota_availability.tenant_id, \
     products_quota_availability.definition_id, products_quota_availability.version, \
     products_quota_availability.revision (products_quota_availability is append-only: a stored \
     row is never replaced)'); END",
    "DROP TRIGGER IF EXISTS products_quota_candidate_decided",
    "CREATE TRIGGER products_quota_candidate_decided BEFORE UPDATE ON products_quota_candidate FOR \
     EACH ROW WHEN OLD.state IN ('approved', 'rejected') BEGIN SELECT RAISE(ABORT, \
     'products_quota_candidate is decided: UPDATE is not permitted'); END",
];

const SQLITE_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS products_quota_availability",
    "DROP TABLE IF EXISTS products_quota_definition_resource_type",
    "DROP TABLE IF EXISTS products_quota_definition_version",
    "DROP TABLE IF EXISTS products_quota_mapping_version",
    "DROP TABLE IF EXISTS products_quota_candidate",
    "DROP TABLE IF EXISTS products_quota_snapshot",
    "DROP TABLE IF EXISTS products_quota_adapter_target",
];

/// The quota tables, parents first: the order in which a refused `down` names them.
const QUOTA_TABLES: [&str; 7] = [
    "products_quota_adapter_target",
    "products_quota_snapshot",
    "products_quota_candidate",
    "products_quota_mapping_version",
    "products_quota_definition_version",
    "products_quota_definition_resource_type",
    "products_quota_availability",
];

/// Refuses while any quota table that exists holds a row, naming each such table; a table that
/// does not exist (a second `down`) holds none.
async fn refuse_while_rows(migration: &str, manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let backend = manager.get_database_backend();
    let conn = manager.get_connection();
    let mut held = Vec::new();
    for table in QUOTA_TABLES {
        let (exists, qualified) = match backend {
            DbBackend::Postgres => (
                format!("SELECT to_regclass('bss.{table}') IS NOT NULL AS present"),
                format!("bss.{table}"),
            ),
            DbBackend::Sqlite => (
                format!(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = \
                     '{table}') AS present"
                ),
                table.to_owned(),
            ),
            _ => {
                return Err(DbErr::Migration(format!(
                    "{backend:?} is not a supported backend for bss-products"
                )));
            }
        };
        let present = conn
            .query_one_raw(Statement::from_string(backend, exists))
            .await?
            .map(|row| row.try_get::<bool>("", "present"))
            .transpose()?
            .unwrap_or(false);
        if !present {
            continue;
        }
        let row = conn
            .query_one_raw(Statement::from_string(
                backend,
                format!("SELECT 1 AS held FROM {qualified} LIMIT 1"),
            ))
            .await?;
        if row.is_some() {
            held.push(format!("{table} holds a row"));
        }
    }
    if held.is_empty() {
        return Ok(());
    }
    Err(DbErr::Migration(format!(
        "{migration}: down refused: {}; reverting would lose the quota catalog's stored \
         evidence, so it runs only on empty quota tables",
        held.join(", ")
    )))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        refuse_while_rows(self.name(), manager).await?;
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}

#[cfg(test)]
#[path = "m20261119_000015_quota_catalog_tests.rs"]
mod tests;
