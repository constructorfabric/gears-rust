-- Types Registry managed-state schema (P1).
--
-- PostgreSQL reference schema, not a migration. Backend migrations map identity,
-- UUID, binary, boolean, timestamp, and binary-collation types to their SQLite,
-- PostgreSQL, or MySQL equivalents. Boolean CHECKs must also work after lowering
-- to SQLite INTEGER 0/1 or MySQL TINYINT(1).
--
-- JSON documents are stored as canonical UTF-8 text. Types Registry stores no
-- externally managed entity identifiers, content, revisions, mappings, or tenant
-- state. Table names are not prefixed with `registry_` a second time: the
-- `types_registry__` prefix already namespaces them.
--
--
-- Identifier columns
-- ------------------
-- Every GTS Identifier or pattern column MUST use varchar(1024), binary collation,
-- and, where the default is multi-byte, an ASCII character set: entity.gts_id,
-- version_family.family_key, operation_item.gts_id,
-- source_claim.gts_id_pattern, and source_claim.plugin_entity_gts_id.
--
-- Binary collation makes pattern and derivation prefix ranges exact across all
-- backends, so a pattern compiles to explicit range bounds rather than a LIKE: a
-- base is a literal prefix of its derivatives, `~` (0x7E) sorts after every segment
-- character, and `.` (0x2E) before them.
--
-- ASCII keeps MySQL indexes within InnoDB's 3072-byte key limit; varchar(1024) in
-- utf8mb4 reserves 4096 bytes. It is exact because the GTS grammar is ASCII-only.
--
--
-- Enumerations
-- ------------
-- Stored as smallint because no native enum representation is shared by SQLite,
-- PostgreSQL, and MySQL. The meaning of every value is written beside its column.
-- Rules:
--
--   * Integers are storage-only; SDK and REST expose the string vocabulary.
--   * After the first release, numbering is append-only, because renumbering is a
--     data migration: values follow the order the governing ADR lists them, a new
--     value takes the next free number, and a retired number is never reused.
--   * Numbering is per column and deliberately NOT aligned between columns. `3` is
--     `completed` in operation.status and `succeeded` in operation_item.status.
--     Where two columns agree that is coincidence rather than a contract and MUST
--     NOT be turned into one: aligning would force gaps where the vocabularies
--     diverge, and a gap reads as a mistake.
--   * CHECK constraints enumerate allowed values rather than accepting a range.
--
--
-- Operations
-- ----------
-- Durable dispatch uses toolkit-db outbox with the `types_registry_outbox` table
-- prefix. Those ToolKit-owned tables are created by outbox migrations and are
-- intentionally not duplicated here. Outbox messages contain only an operation
-- UUID.
--
-- Registration and deletion are asynchronous (ADR-0012). An accepted POST creates
-- one operation, one operation_item per candidate, and one outbox message. The
-- operation is both the scoped idempotency receipt and the client-visible workflow
-- state. Dry run uses the same path but commits no managed state; it is a mode, not
-- another operation kind.
--
-- Purge is a synchronous platform-plane job (ADR-0013). It returns its report
-- directly and creates no operation, item, or outbox message.


-- A version family binds a family key to one ownership scope. It has no newest
-- member, current pointer, count, highest major, or family-wide version. The
-- unique row enforces common ownership and serializes concurrent first admission.
--
-- `family_key` is the canonical GTS Identifier with the WHOLE version of its LAST
-- segment removed - the major and, where one is present, the minor - every
-- preceding segment held exactly as written, and the trailing `~` of a type
-- identifier normalized away (ADR-0004):
--
--   gts.acme.crm.customer.type.v1~   -> gts.acme.crm.customer.type
--   gts.acme.crm.customer.type.v1.3~ -> gts.acme.crm.customer.type
--   gts.cf.core.events.type.v1~acme.crm.order.type.v1~   -> gts.cf.core.events.type.v1~acme.crm.order.type
--   gts.cf.core.events.type.v2~acme.crm.order.type.v1~   -> gts.cf.core.events.type.v2~acme.crm.order.type
--   gts.cf.core.events.topic.v1~acme.crm.orders.topic.v1 -> gts.cf.core.events.topic.v1~acme.crm.orders.topic
--
-- A minor in a PRECEDING segment survives verbatim, exactly as a major does:
-- `gts.A.v1.2~B.v3~` keys on `gts.A.v1.2~B`.
--
-- A family key is not a GTS Identifier: it has neither version nor kind and MUST
-- NOT be parsed as one. The encoding is total because every managed identifier's
-- last segment carries a version - ADR-0004 permits it a minor, and ADR-0001
-- forbids a managed identifier an explicit UUID tail - so removing the whole
-- version, rather than only the major, is total now that a minor is admissible.
--
-- Removing the kind marker deliberately maps otherwise identical type and Instance
-- spellings to one family key: the derived type `gts.A~acme.orders.v1~` and the
-- well-known Instance `gts.A~acme.orders.v1` differ by one character, denote
-- unrelated things, collide here, and the second is refused. Nothing needs both:
-- an Instance of that derived type is `gts.A~acme.orders.v1~<segment>`. No kind
-- column is needed either - under the family lock admission reads any existing
-- member, whose identifier already carries its kind, and rejects a candidate whose
-- kind differs.
--
-- Minor shape - within one major either every member carries a minor or none does -
-- and minor contiguity - the minors of a major are CONTIGUOUS and open at M.0 - are
-- checked under that same lock. Both are scoped to one MAJOR, as the compatibility
-- chain is, so a family may hold a major-only v1~ beside a minor-bearing v2.0~.
-- Contiguity fixes which single identifier decides each question, so each rule is
-- an exact lookup through uq_tr_entity_gts_id:
--
--   shape, minor-bearing candidate vM.n~   -> refuse while vM~ exists
--   shape, major-only candidate vM~        -> refuse while vM.0~ exists
--   contiguity, candidate vM.n~ with n > 0 -> refuse unless vM.(n-1)~ exists
--
-- A DELETED predecessor still counts because its definition remains the
-- compatibility baseline until purge, so skipping it would let an ordinary deletion
-- move the baseline. Recheck it inside the commit transaction: a concurrent
-- delete-and-purge can remove it during validation, and the candidate does not
-- exist yet and so pins nothing.
--
-- Predecessors are not dependency edges: such an edge would forbid deleting v1.0~
-- while v1.1~ exists, which ADR-0008 permits and ADR-0004 relies on. With no edge
-- between two minors there is no other row an admission and a purge conflict on, so
-- this row is their only serialization point for these rules. Purge therefore locks
-- the families its pattern touches, in deterministic order, before checking
-- eligibility and holds the locks through commit (ADR-0013); otherwise it could
-- release a predecessor between an admission's check and its commit, or release a
-- minor under a successor admitted concurrently.
--
-- Minor-version policy follows from the identifier and is not stored here.
--
-- Ownership must be fixed before the first member, hence the stored owner columns.
-- Family and first entity commit together, so a concurrent loser sees the winner's
-- member after acquiring this lock. Purging every member releases the family;
-- ordinary deletion does not.
--
-- No owner index: P1 discovery filters the owner copy on entity. Family ownership
-- is write-once; correction is delete, purge, and re-register (ADR-0013).
CREATE TABLE types_registry__version_family (
    id               bigint        GENERATED BY DEFAULT AS IDENTITY,
    family_key       varchar(1024) COLLATE "C" NOT NULL,
    ownership_scope  smallint      NOT NULL, -- 1 global, 2 tenant
    owner_tenant_id  uuid          NULL,
    created_at       timestamptz   NOT NULL,

    CONSTRAINT pk_tr_version_family PRIMARY KEY (id),
    CONSTRAINT uq_tr_version_family_key UNIQUE (family_key),
    CONSTRAINT ck_tr_version_family_owner CHECK (
        (ownership_scope = 1 AND owner_tenant_id IS NULL)
        OR
        (ownership_scope = 2 AND owner_tenant_id IS NOT NULL)
    )
);


-- Request identity and client-visible workflow state share one row because every
-- accepted request creates an operation. `unchanged` guarantees that a redundant
-- submission creates no revision and does not advance resource_version.
--
-- `idempotency_scope_hash` digests (plane, tenant_id, principal_id). Including the
-- principal prevents cross-subject replay. Digesting also removes nullable
-- tenant_id from the unique key: all three backends permit multiple NULLs there.
--
-- `kind` is registration/revision or deletion. Purge creates no operation, and
-- ownership correction is delete, purge, re-register (ADR-0009). Request bodies
-- are per candidate in operation_item.request_payload.
--
-- `dry_run` is orthogonal to `kind` and is included in request_fingerprint. Reusing
-- one Idempotency-Key for dry run and commit is therefore a fingerprint mismatch,
-- not a replay of the dry-run result.
--
-- Dry-run operations are unpinned; see idx_tr_operation_status. Worker leases,
-- attempts, retries, and dead letters remain in ToolKit outbox tables.
CREATE TABLE types_registry__operation (
    id                       uuid         NOT NULL,
    kind                     smallint     NOT NULL, -- 1 registration, 2 deletion
    dry_run                  boolean      NOT NULL,
    plane                    smallint     NOT NULL, -- 1 platform, 2 tenant
    tenant_id                uuid         NULL,
    -- The subject of the SecurityContext, which is a UUID there.
    principal_id             uuid         NOT NULL,
    idempotency_key          varchar(255) NOT NULL,
    idempotency_scope_hash   bytea        NOT NULL,
    request_fingerprint      bytea        NOT NULL,
    -- 1 pending, 2 running, 3 completed.
    --
    -- Progress only: completed means every item is terminal. Outcomes remain on
    -- operation_item and are not aggregated here.
    --
    -- There is no cancellation or expiry state. Outbox redelivery retries
    -- idempotently; after exhaustion or timeout, existing terminal outcomes stay
    -- intact and unfinished items fail with an error_payload.
    status                   smallint     NOT NULL,
    created_at               timestamptz  NOT NULL,
    started_at               timestamptz  NULL,
    completed_at             timestamptz  NULL,

    CONSTRAINT pk_tr_operation PRIMARY KEY (id),
    CONSTRAINT uq_tr_operation_idem
        UNIQUE (idempotency_scope_hash, idempotency_key),
    -- Redundant over pk_tr_operation and present only as the target of
    -- fk_tr_operation_item_operation, which ties an item's copied `kind` and
    -- `dry_run` to its parent's.
    CONSTRAINT uq_tr_operation_kind_mode UNIQUE (id, kind, dry_run),
    CONSTRAINT ck_tr_operation_kind CHECK (kind IN (1, 2)),
    CONSTRAINT ck_tr_operation_plane CHECK (
        (plane = 1 AND tenant_id IS NULL)            -- platform
        OR
        (plane = 2 AND tenant_id IS NOT NULL)        -- tenant
    ),
    CONSTRAINT ck_tr_operation_status CHECK (
        status IN (1, 2, 3)
    ),
    CONSTRAINT ck_tr_operation_state CHECK (
        (status = 1                                  -- pending
            AND started_at IS NULL
            AND completed_at IS NULL)
        OR
        (status = 2                                  -- running
            AND started_at IS NOT NULL
            AND completed_at IS NULL)
        OR
        (status = 3                                  -- completed
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL)
    )
);

-- Serves retention by (status, completed_at) and stalled-operation recovery by the
-- status prefix. Retention is measured from terminality, not acceptance. Recovery
-- filters the bounded non-terminal set on created_at/started_at; indexing those
-- timestamps would burden every operation. The full index is used because MySQL
-- has no portable partial-index equivalent.
--
-- Retention removes a completed operation only when no revision pins any item.
-- Revisions use ON DELETE RESTRICT and live until purge. Unpinned cases include
-- dry runs, operations producing no revision, successful deletions, and operations
-- whose revisions were purged (ADR-0012).
--
-- Retention MUST test revision foreign keys and MUST NOT infer pinning from item
-- status: dry-run successes and successful deletions create no revision.
--
--   NOT EXISTS (SELECT 1 FROM types_registry__operation_item i
--               WHERE i.operation_id = o.id
--                 AND (EXISTS (SELECT 1 FROM types_registry__type_schema_revision r
--                              WHERE r.operation_item_id = i.id)
--                   OR EXISTS (SELECT 1 FROM types_registry__instance_revision r
--                              WHERE r.operation_item_id = i.id)))
--
-- The inner lookups use the unique operation_item_id indexes. Deleting an
-- operation cascades to its bounded item set and releases its idempotency key, so
-- replay after retention executes afresh. Sweeping pinned operations requires a
-- different home for admitting-principal provenance; see DESIGN §4, D4.
CREATE INDEX idx_tr_operation_status
    ON types_registry__operation (status, completed_at, id);


-- One durable candidate and public result per exact GTS Identifier. Entity kind
-- and Registry Reference are omitted because both derive from gts_id; unlike on
-- entity, neither serves an index or constraint here.
--
-- Each transition has its own timestamp, so no updated_at is needed.
--
-- expected_resource_version encodes the closed precondition vocabulary: 0 means
-- must_not_exist; >=1 is the version to match. On the wire, an absent field means
-- must_not_exist and literal 0 is rejected. Entity resource versions start at 1.
--
-- `dry_run` and `kind` are copied from the parent because the state CHECK cannot
-- read another table. A successful item may lack a revision when:
--
--   * dry run writes nothing (`unchanged` still reports the version read);
--   * deletion advances resource_version but creates no content revision;
--   * `unchanged` creates no revision and advances no version.
--
-- The CHECK constrains field shape, not revision existence: the item has no entity
-- id for such a foreign key. The worker writes the item and revision in one
-- transaction. Result revision and resource version are write-once.
--
-- Dry-run success is `succeeded`, not a separate "would succeed" status: the
-- operation already exposes the mode, and restating it per item would be the second
-- vocabulary `cpt-cf-types-registry-principle-single-vocabulary` forbids.
--
-- request_payload is dropped at terminality. Failed and dry-run receipts therefore
-- retain the structured outcome but not submitted content; keeping it could retain
-- rejected content for the lifetime of unrelated successful revisions.
--
-- Purge does not delete request receipts (ADR-0013): they describe past requests
-- and are reached only by operation or idempotency key. Purge removes revisions,
-- which unpins the operation for retention.
CREATE TABLE types_registry__operation_item (
    id                        bigint        GENERATED BY DEFAULT AS IDENTITY,
    operation_id              uuid          NOT NULL,
    item_no                   integer       NOT NULL,
    gts_id                    varchar(1024) COLLATE "C" NOT NULL,
    -- Copied from operation.dry_run and operation.kind; see the comment above.
    dry_run                   boolean       NOT NULL,
    kind                      smallint      NOT NULL, -- 1 registration, 2 deletion
    -- 0 means the candidate must not exist; otherwise the version to match
    expected_resource_version bigint        NOT NULL,
    -- 1 pending, 2 running, 3 succeeded, 4 unchanged, 5 failed.
    --
    -- Status distinguishes effects; error_payload distinguishes causes. For a
    -- committed item, `succeeded` changed entity state; for dry run it means every
    -- check passed but nothing was written. `unchanged` proved redundancy.
    --
    -- No separate `blocked` status: it has the same stored effect as failure and
    -- uses a `blocked_by_dependency` error reason.
    status                    smallint      NOT NULL,
    request_payload           text          NULL,
    result_revision_no        integer       NULL,
    result_resource_version   bigint        NULL,
    error_payload             text          NULL,
    created_at                timestamptz   NOT NULL,
    started_at                timestamptz   NULL,
    completed_at              timestamptz   NULL,

    CONSTRAINT pk_tr_operation_item PRIMARY KEY (id),
    CONSTRAINT uq_tr_operation_item_no
        UNIQUE (operation_id, item_no),
    CONSTRAINT uq_tr_operation_item_gts
        UNIQUE (operation_id, gts_id),
    -- The composite FK makes copied kind/dry_run agree with the parent. Its target
    -- must be explicitly unique on all three backends.
    CONSTRAINT fk_tr_operation_item_operation
        FOREIGN KEY (operation_id, kind, dry_run)
        REFERENCES types_registry__operation (id, kind, dry_run) ON DELETE CASCADE,
    CONSTRAINT ck_tr_operation_item_no CHECK (item_no >= 0),
    CONSTRAINT ck_tr_operation_item_precondition
        CHECK (expected_resource_version >= 0),
    CONSTRAINT ck_tr_operation_item_revision
        CHECK (result_revision_no IS NULL OR result_revision_no >= 1),
    CONSTRAINT ck_tr_operation_item_resource_version
        CHECK (
            result_resource_version IS NULL
            OR result_resource_version >= 1
        ),
    CONSTRAINT ck_tr_operation_item_kind CHECK (kind IN (1, 2)),
    CONSTRAINT ck_tr_operation_item_status CHECK (
        status IN (1, 2, 3, 4, 5)
    ),
    CONSTRAINT ck_tr_operation_item_state CHECK (
        (status = 1                                  -- pending
            AND request_payload IS NOT NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NULL
            AND started_at IS NULL
            AND completed_at IS NULL)
        OR
        (status = 2                                  -- running
            AND request_payload IS NOT NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NULL
            AND started_at IS NOT NULL
            AND completed_at IS NULL)
        OR
        (status IN (3, 4)                            -- succeeded, unchanged
            AND request_payload IS NULL
            AND error_payload IS NULL
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL
            -- Only a committed, changed registration creates a content revision.
            AND (result_revision_no IS NOT NULL) = (NOT dry_run AND kind = 1 AND status = 3)
            -- `unchanged` is equal canonical authored content (ADR-0012), valid
            -- only for a registration update with an expected existing version.
            AND (status <> 4 OR (kind = 1 AND expected_resource_version >= 1))
            -- Every committed result has a resource version. Dry-run `unchanged`
            -- reports the existing version; dry-run `succeeded` allocates none.
            AND (result_resource_version IS NOT NULL) = (NOT dry_run OR status = 4))
        OR
        (status = 5                                  -- failed
            AND request_payload IS NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NOT NULL
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL)
    )
);

-- No index by status. Polling reads every item of one operation ordered by
-- item_no, which uq_tr_operation_item_no serves, and a batch is bounded, so
-- scanning its items costs less than maintaining a second index.


-- One logical entity per admitted managed GTS Identifier. Deleted rows remain as
-- tombstones so issued Registry References stay reverse-resolvable.
--
-- Two derivable columns are materialized for indexing and constraints:
--
--   * `gts_uuid` is the UUIDv5 Registry Reference derived from gts_id. Storage
--     enables reverse lookup and collision rejection (ADR-0001); portable UUIDv5
--     expression indexes are unavailable. SDK clients MUST NOT derive it locally.
--   * `entity_kind` follows from trailing `~`, but suffix predicates are not
--     portably indexable and the value drives kind-specific constraints.
--
-- lifecycle_status is only active/deleted. P1 has no managed `deprecated` state,
-- and externally managed entities are not stored here (ADR-0008).
--
-- Ownership is copied from version_family for SecureORM scoping and join-free
-- visibility checks. Admission verifies the copy under the family lock. A
-- composite FK would not validate global rows because owner_tenant_id is NULL
-- under MATCH SIMPLE.
--
-- `owning_gear` answers "who do I ask about this contract", which a global entity
-- otherwise cannot answer at all, ck_tr_entity_owner leaving its whole owner side
-- null. It is the `#[toolkit::gear(name = ...)]` name, required for global entities
-- and optional for tenant-owned ones, whose owner is already a tenant, and it is
-- rewritten on each admission rather than being write-once. See DESIGN §3.3.
--
-- It is caller-declared attribution and MUST NOT authorize, because it cannot be
-- verified: in a single-process deployment every gear shares the process workload
-- identity, so the platform cannot tell which gear inside it is registering. No P1
-- query selects by it, so it has no index.
CREATE TABLE types_registry__entity (
    id                       bigint        GENERATED BY DEFAULT AS IDENTITY,
    gts_uuid                 uuid          NOT NULL,
    gts_id                   varchar(1024) COLLATE "C" NOT NULL,
    -- 1 type_schema, 2 instance
    entity_kind              smallint      NOT NULL,
    family_id                bigint        NOT NULL,
    ownership_scope          smallint      NOT NULL, -- 1 global, 2 tenant
    owner_tenant_id          uuid          NULL,
    owning_gear              varchar(1024) NULL,
    lifecycle_status         smallint      NOT NULL, -- 1 active, 2 deleted
    resource_version         bigint        NOT NULL,
    deleted_at               timestamptz   NULL,
    created_at               timestamptz   NOT NULL,
    updated_at               timestamptz   NOT NULL,

    CONSTRAINT pk_tr_entity PRIMARY KEY (id),
    CONSTRAINT uq_tr_entity_gts_id UNIQUE (gts_id),
    CONSTRAINT uq_tr_entity_gts_uuid UNIQUE (gts_uuid),
    CONSTRAINT fk_tr_entity_family
        FOREIGN KEY (family_id)
        REFERENCES types_registry__version_family (id) ON DELETE RESTRICT,
    CONSTRAINT ck_tr_entity_kind
        CHECK (entity_kind IN (1, 2)),
    CONSTRAINT ck_tr_entity_owner CHECK (
        (ownership_scope = 1                          -- global
            AND owner_tenant_id IS NULL
            AND owning_gear IS NOT NULL)
        OR
        (ownership_scope = 2                          -- tenant
            AND owner_tenant_id IS NOT NULL)
    ),
    CONSTRAINT ck_tr_entity_lifecycle CHECK (
        (lifecycle_status = 1 AND deleted_at IS NULL)     -- active
        OR
        (lifecycle_status = 2 AND deleted_at IS NOT NULL) -- deleted
    ),
    CONSTRAINT ck_tr_entity_resource_version
        CHECK (resource_version >= 1)
);

-- Exact, covering family enumeration. A GTS wildcard is greedy across the chain
-- separator and would also capture types derived from a member.
CREATE INDEX idx_tr_entity_family
    ON types_registry__entity (family_id, lifecycle_status, gts_id);

-- Main read index: one global range plus one range per tenant ancestor, each with
-- its own gts_id pattern bounds. Tenant ancestor chains are shallow.
--
-- No kind-led index: every read must filter visibility first. If kind filtering
-- becomes hot, add entity_kind to this index.
CREATE INDEX idx_tr_entity_visibility
    ON types_registry__entity (
        ownership_scope,
        owner_tenant_id,
        lifecycle_status,
        gts_id
    );


-- Immutable Type Schema admission snapshot: authored document, hash, and engine
-- provenance.
--
-- Neither the effective artifacts nor the dependency revision vector are kept, for
-- the same reason: nothing reads the admission-time resolution, since compatibility
-- compares a candidate against its baseline and no P1 operation looks backwards.
-- The vector exists only during one validation attempt, as the concurrency control
-- the commit re-checks; a redelivered outbox message revalidates from scratch.
--
-- gts_spec_version and gts_impl_version identify the admission engine for every
-- revision, including the cases with no compatibility comparison at all - a first
-- admission, an M.0 opening a Minor-Bearing Major, and a candidate whose own last
-- segment carries major 0 (`cpt-cf-types-registry-fr-validate-schema-compat`).
-- Where comparison did happen they identify the rules that produced the verdict,
-- which is what a checker upgrade can change for an unchanged pair of schemas. This
-- provenance cannot be reconstructed later and supports future checker-upgrade
-- policy (ADR-0003).
--
-- operation_item_id reaches the operation and admitting principal. ON DELETE
-- RESTRICT pins that provenance until the revision is purged; terminal items have
-- already dropped request_payload.
--
-- The natural (entity_id, revision_no) key is also the fact every dependent row
-- needs and clusters one entity's history. A surrogate would add a lookup without
-- replacing those two values.
CREATE TABLE types_registry__type_schema_revision (
    entity_id                  bigint       NOT NULL,
    revision_no                integer      NOT NULL,
    raw_schema                 text         NOT NULL,
    content_hash               bytea        NOT NULL,
    gts_spec_version           varchar(32)  NOT NULL,
    gts_impl_version           varchar(32)  NOT NULL,
    -- True when ADR-0004 `force` waived ADR-0003 cross-minor compatibility. This
    -- non-derived admission fact is exposed in the provenance projection.
    --
    -- Always false for major-only entities and the first minor of a major. Safe
    -- upgrade across several minors requires every traversed value to be false.
    compat_forced              boolean      NOT NULL,
    operation_item_id          bigint       NOT NULL,
    created_at                 timestamptz  NOT NULL,
    updated_at                 timestamptz  NOT NULL,

    CONSTRAINT pk_tr_type_schema_revision PRIMARY KEY (entity_id, revision_no),
    CONSTRAINT uq_tr_type_schema_revision_item UNIQUE (operation_item_id),
    CONSTRAINT fk_tr_type_schema_revision_entity
        FOREIGN KEY (entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_type_schema_revision_item
        FOREIGN KEY (operation_item_id)
        REFERENCES types_registry__operation_item (id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_tr_type_schema_revision_no CHECK (revision_no >= 1)
);

-- No (entity_id, content_hash) index: no-op comparison reads the current revision.
-- Content equal to an older non-current revision is a new revision (ADR-0005).


-- Immutable Instance admission snapshot with the exact validating Type Schema
-- revision (ADR-0006). Vector, principal, and natural-key choices follow
-- type_schema_revision.
--
-- type_schema_entity_id is the Instance identifier through its last `~` (GTS
-- spec 11.1), materialized for the composite foreign key.
CREATE TABLE types_registry__instance_revision (
    entity_id                     bigint       NOT NULL,
    revision_no                   integer      NOT NULL,
    canonical_value               text         NOT NULL,
    content_hash                  bytea        NOT NULL,
    type_schema_entity_id         bigint       NOT NULL,
    type_schema_revision_no       integer      NOT NULL,
    gts_spec_version              varchar(32)  NOT NULL,
    gts_impl_version              varchar(32)  NOT NULL,
    operation_item_id             bigint       NOT NULL,
    created_at                    timestamptz  NOT NULL,
    updated_at                    timestamptz  NOT NULL,

    CONSTRAINT pk_tr_instance_revision PRIMARY KEY (entity_id, revision_no),
    CONSTRAINT uq_tr_instance_revision_item UNIQUE (operation_item_id),
    CONSTRAINT fk_tr_instance_revision_entity
        FOREIGN KEY (entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_instance_revision_schema
        FOREIGN KEY (
            type_schema_entity_id,
            type_schema_revision_no
        )
        REFERENCES types_registry__type_schema_revision (
            entity_id,
            revision_no
        )
        ON DELETE RESTRICT,
    CONSTRAINT fk_tr_instance_revision_item
        FOREIGN KEY (operation_item_id)
        REFERENCES types_registry__operation_item (id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_tr_instance_revision_no CHECK (revision_no >= 1)
);

-- No content-hash index, same rationale as Type Schema revisions, and no conforming-
-- schema index: revalidation arrives through the reverse dependency traversal.


-- Current Type Schema state. Authored content, hash, and checker versions remain in
-- the referenced immutable revision.
--
-- Resolved artifacts depend on current floating dependencies and may be recomputed
-- without creating an authored revision; they are current facts, not history.
--
-- Per-level content-model classification is derived from resolved_schema and used
-- only off the hot path, so it is not stored.
--
-- resolution_fingerprint digests canonical bytes of all three artifacts and is
-- rewritten on recompute. Its input MUST be canonical and independent of the
-- serializer's map iteration order, or the value flaps while the artifacts stand
-- still. It supports equality only, never ordering. A digest,
-- unlike a counter, remains stable when recomputation yields identical artifacts.
--
-- It detects dependency-driven read changes without moving entity.resource_version,
-- which is reserved for optimistic writes. Resolution validators combine this
-- digest with resource_version and tenant ancestor-chain version.
CREATE TABLE types_registry__type_schema (
    entity_id                  bigint      NOT NULL,
    revision_no                integer     NOT NULL,
    resolved_schema            text        NOT NULL,
    effective_traits           text        NOT NULL,
    effective_traits_schema    text        NOT NULL,
    resolution_fingerprint     bytea       NOT NULL,
    created_at                 timestamptz NOT NULL,
    updated_at                 timestamptz NOT NULL,

    CONSTRAINT pk_tr_type_schema PRIMARY KEY (entity_id),
    CONSTRAINT fk_tr_type_schema_revision_ptr
        FOREIGN KEY (entity_id, revision_no)
        REFERENCES types_registry__type_schema_revision (
            entity_id,
            revision_no
        )
        ON DELETE CASCADE
);


-- Current registered Instance state: the current-revision pointer and nothing else.
-- Authored value, hash, admitting schema revision, and checker versions stay in
-- instance_revision; unlike type_schema there is no resolved artifact to hold.
--
-- No conforming-schema columns: that schema is the identifier prefix through the last
-- `~` (GTS spec 11.1), the immutable pair on instance_revision, and a kind 4
-- dependency edge, which is how revalidation reaches the Instance. No last-revalidated
-- revision either — activation keeps a current value valid against its schema's
-- current revision, and a rolled-back attempt commits nothing, so it could neither
-- inform nor shorten a retry (ADR-0006).
CREATE TABLE types_registry__instance (
    entity_id   bigint      NOT NULL,
    revision_no integer     NOT NULL,
    created_at  timestamptz NOT NULL,
    updated_at  timestamptz NOT NULL,

    CONSTRAINT pk_tr_instance PRIMARY KEY (entity_id),
    CONSTRAINT fk_tr_instance_revision_ptr
        FOREIGN KEY (entity_id, revision_no)
        REFERENCES types_registry__instance_revision (
            entity_id,
            revision_no
        )
        ON DELETE CASCADE
);


-- Direct dependency relation: from_entity_id depends on to_entity_id. Nothing
-- transitive is stored: transitive reachability is answered by a recursive CTE over
-- these rows rather than by a second materialized table, and deletion safety reads
-- the direct rows and only those, since a transitive-only dependent must not
-- block: it would disappear the moment the intermediate entity did.
--
-- Derivation and Instance conformance are materialized although derivable from the
-- identifier. A portable recursive CTE has one recursive reference; mixing prefix
-- derivation with stored edges would require another branch or an index-defeating
-- OR. These edges are written once from immutable identifiers.
--
-- Traversal MUST use UNION, never UNION ALL. Cycles are valid (ADR-0012), and
-- UNION ALL would not terminate consistently across the three backends.
--
-- The recursive term MUST NOT carry depth or another per-row accumulator: UNION
-- deduplicates whole rows, so an accumulator makes cyclic revisits distinct.
-- Fetch affected edges separately, then order SCC condensation in the worker
-- (ADR-0012).
--
-- Admission replaces only the admitted entity's outgoing rows.
--
-- `x-gts-ref` constrains what a value may name but is not itself a schema-resolution
-- dependency, which is why the strict reference extractor in gts-rust excludes it
-- from schema resolution. Its edge protects the entity the value or the constraint
-- **names**:
--
--   * an exact identifier names that entity;
--   * a wildcard names its longest prefix that is a valid GTS Identifier;
--     `...topic.v1~*` and `...topic.v1~x.core.*` both name
--     `gts.cf.core.events.topic.v1~`;
--   * a pattern naming nothing valid, such as `gts.*`, produces no edge, and so
--     does the `/$id` self-reference of GTS spec 9.6.
--
-- Edges do not expand a pattern's open match set, so new matching entities require
-- no edge update or dependency_pattern table.
--
-- The edge protects the named target, not constraint satisfiability. Deleting
-- `topic.v1~` is refused; deleting every current match of `...~x.core.*` while its
-- named base survives is allowed.
--
-- One asymmetry follows and is accepted: a named base is a dependency for deletion
-- but not for revalidation, since the subject holds a pattern string rather than
-- the base's content, so admitting a revision of the base does not change the
-- subject's effective form. The traversal reaches the subject anyway, recomputes
-- it, finds an identical digest, and stops there.
--
-- Materialized transitive closure was rejected because drift could silently skip
-- revalidation (ADR-0011). It may later be added only as a cache over these rows.
CREATE TABLE types_registry__dependency (
    from_entity_id bigint      NOT NULL,
    -- 1 schema_ref ($ref), 2 gts_ref (x-gts-ref target), 3 derivation
    -- (immediate base), 4 instance_of (conforming Type Schema)
    kind           smallint    NOT NULL,
    to_entity_id   bigint      NOT NULL,

    CONSTRAINT pk_tr_dependency
        PRIMARY KEY (from_entity_id, kind, to_entity_id),
    CONSTRAINT fk_tr_dependency_from
        FOREIGN KEY (from_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_dependency_to
        FOREIGN KEY (to_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT ck_tr_dependency_kind
        CHECK (kind IN (1, 2, 3, 4))
);

-- Serves recursive reverse traversal and direct-dependent deletion checks.
CREATE INDEX idx_tr_dependency_to
    ON types_registry__dependency (to_entity_id, from_entity_id);


-- Singleton routing state. Lock it for every claim mutation because wildcard
-- overlap is not expressible as a unique constraint and the conflicting row may
-- not yet exist. The lock serializes validation and commit.
--
-- Bump generation in the same transaction. Federated cursors bind to it and the
-- in-memory claim set uses it for reload detection. A counter is correct here
-- because every claim mutation changes routing.
CREATE TABLE types_registry__routing_config (
    id          smallint    NOT NULL,
    generation  bigint      NOT NULL,
    updated_at  timestamptz NOT NULL,

    CONSTRAINT pk_tr_routing_config PRIMARY KEY (id),
    CONSTRAINT ck_tr_routing_config_singleton CHECK (id = 1),
    CONSTRAINT ck_tr_routing_config_generation CHECK (generation >= 1)
);


-- Active claims and retired reservations share one row because both block managed
-- registration in the claimed identifier space (ADR-0011). `retired_at IS NULL` is
-- the active predicate: a claim has no third state, so there is no status column.
--
-- The row projects a registered Types Registry source-plugin Instance. priority
-- and plugin_entity_gts_id mirror its plugin base fields, keeping deterministic
-- resolver ordering join-free, and make overlap checks independent of
-- plugin-document parsing.
--
-- Lifecycle mirrors the projected plugin Instance:
--
--   Instance active   -> claim active
--   Instance deleted  -> claim retired, a reservation over the same space
--   Instance purged   -> claim row removed, the space released
--
-- Deleting the Instance writes retired_at and nothing else. The projection stays
-- intact, so a reservation still records which plugin revision issued it. Purge
-- removes the claim row before the revision it references, under the same
-- routing_config lock, so ON DELETE RESTRICT never fires and no column has to be
-- nulled in advance. Retirement and removal bump routing_config.generation under
-- that lock.
--
-- Liveness does not retire claims. An unreachable plugin keeps them and requests
-- fail closed; only Instance deletion writes retired_at. Therefore this table has
-- no health or last-seen field (ADR-0011).
--
-- There is no claim takeover: active claims cannot replace retired reservations
-- (ADR-0011). Manual retargeting must bump generation under lock and keep the
-- successor plugin document consistent with this projection; see DESIGN §3.2.
--
-- priority belongs to the plugin, so all its projected claims repeat one value;
-- projection maintenance keeps them equal (ADR-0007).
--
-- A claim carries no entity-kind restriction. Kind follows from the identifier's
-- trailing `~`, so a pattern covers both kinds in its space and every admission
-- check is kind-blind: a candidate claim is matched by pattern against active
-- claims and retired reservations, and by prefix range over entity.gts_id against
-- already managed identifiers (DESIGN §3.2). A source answers NotFound for a kind it does
-- not hold (ADR-0007).
--
-- No stored upper bound or pattern index: claim counts are expected to be single
-- digits, and the authoritative GTS matcher must confirm every string prefilter.
CREATE TABLE types_registry__source_claim (
    id                         bigint        GENERATED BY DEFAULT AS IDENTITY,
    gts_id_pattern             varchar(1024) COLLATE "C" NOT NULL,
    plugin_entity_gts_id       varchar(1024) COLLATE "C" NOT NULL,
    plugin_entity_id           bigint        NOT NULL,
    plugin_entity_revision_no  integer       NOT NULL,
    priority                   smallint      NOT NULL,
    created_at                 timestamptz   NOT NULL,
    updated_at                 timestamptz   NOT NULL,
    retired_at                 timestamptz   NULL,  -- NULL means active

    CONSTRAINT pk_tr_source_claim PRIMARY KEY (id),
    CONSTRAINT uq_tr_source_claim_gts_id_pattern UNIQUE (gts_id_pattern),
    CONSTRAINT fk_tr_source_claim_plugin
        FOREIGN KEY (
            plugin_entity_id,
            plugin_entity_revision_no
        )
        REFERENCES types_registry__instance_revision (
            entity_id,
            revision_no
        )
        ON DELETE RESTRICT
);
