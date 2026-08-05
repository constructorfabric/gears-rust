-- Types Registry managed-state schema (P1).
--
-- This is a PostgreSQL reference schema, not a migration. Backend migrations
-- map identity, UUID, binary, boolean, timestamp, and binary-collation types to
-- the corresponding SQLite / PostgreSQL / MySQL representation. Boolean is the
-- one that has no native form on two of the three: SQLite stores 0/1 in an
-- INTEGER and MySQL aliases BOOLEAN to TINYINT(1), so the CHECK constraints
-- below that read a boolean must survive that lowering.
--
-- JSON documents are stored as canonical UTF-8 text. Types Registry stores no
-- externally managed entity identifiers, content, revisions, mappings, or
-- tenant state.
--
-- Every column holding a GTS Identifier or GTS pattern is varchar(1024) with a
-- binary collation, and MUST be declared with an ASCII character set on a
-- backend whose default is multi-byte. Both halves are load-bearing.
--
-- The binary collation makes prefix ranges exact and identical on all three
-- backends. It is what lets a pattern compile to explicit bounds rather than a
-- LIKE, and what makes the derivation reverse lookup a range scan: every base
-- is a literal string prefix of the identifiers derived from it, and `~`
-- (0x7E) sorts after every character a segment may contain, while `.` (0x2E)
-- sorts before them, so a prefix range is clean in both directions.
--
-- The ASCII declaration is a portability requirement, not an optimization.
-- InnoDB caps an index key at 3072 bytes; varchar(1024) in utf8mb4 reserves
-- 4096, so the unique index on entity.gts_id and every composite index that
-- ends in an identifier would be rejected outright on MySQL. The GTS grammar
-- admits only lowercase ASCII - segments are `[a-z_][a-z0-9_]*`, separators are
-- `.` and `~`, versions are digits, an anonymous tail is hex - so one byte per
-- character is exact rather than a truncation. This applies to entity.gts_id,
-- version_family.family_key, operation_item.gts_id, source_claim.gts_id_pattern,
-- and source_claim.plugin_entity_gts_id.
--
-- Durable dispatch uses toolkit-db outbox with the `types_registry_outbox`
-- table prefix. Those ToolKit-owned tables are created by outbox migrations
-- and are intentionally not duplicated here. Outbox messages contain only an
-- operation UUID.
--
-- Every registration and deletion is asynchronous. An accepted POST always
-- creates one operation row, which carries both the scoped Idempotency-Key and
-- the client-visible workflow state, one operation_item per candidate GTS
-- Identifier, and one outbox message. There is no synchronous acceptance path
-- for those two kinds and no separate request-receipt table.
--
-- A dry run uses that same path and commits nothing. It is a mode of the
-- operation rather than a kind of it, so it is a boolean orthogonal to `kind`
-- and not two more values in that enumeration.
--
-- Purge is outside all of that: it is a synchronous platform-plane job
-- (ADR-0013) that returns its report in the response and creates no operation,
-- no operation_item, and no outbox message. Nothing in this file records a
-- purge; what it records is the effect, which is rows no longer being here.
--
-- Table names are not prefixed with `registry_` a second time: the
-- `types_registry__` prefix already namespaces them.
--
-- Enumerations are stored as smallint, with the meaning of every value written
-- beside the column. MySQL ENUM and PostgreSQL CREATE TYPE are both unavailable
-- as a common representation and SQLite has neither, so an enumeration was
-- always going to be emulated; a fixed 2-byte integer emulates it more cheaply
-- than a varchar, carries no collation or character-set question, and keeps the
-- indexes that lead with a status or a scope narrow.
--
-- Three rules govern those values.
--
-- They are storage encoding only. The SDK and REST contracts keep the string
-- vocabulary - a response says `"status": "completed"`, never `3` - and the
-- mapping lives in the storage layer. A numeric value must never reach a public
-- payload.
--
-- Numbering is append-only. Renumbering is a data migration, unlike renaming a
-- string that no row stores. Values are assigned in the order the governing ADR
-- lists them, a new value takes the next free number, and a retired value's
-- number is never reused.
--
-- Numbering is per column and deliberately not aligned between columns. Giving
-- `pending` the same number in operation.status and operation_item.status would
-- imply a relationship between two distinct vocabularies that does not exist,
-- and it would force gaps wherever they diverge - and a gap reads as a mistake.
--
-- CHECK constraints list the admissible values rather than bounding a range, so
-- that a number outside the vocabulary is rejected instead of being accepted as
-- a value nothing has defined yet.
--
-- One rule governs the comments in this file, because the three documents
-- describing this gear divide the work and a comment that forgets which one it
-- is in becomes a stale copy of an argument made better elsewhere. A comment
-- here says what a column means and the one invariant a reader would otherwise
-- violate. Where the reason is a decision, it points at the ADR that took it
-- instead of restating the argument - the ADR owns the alternatives and what
-- would reverse them, and DESIGN owns how the decisions compose into a working
-- system. What stays here is what only the schema knows: the ASCII and
-- binary-collation rules, the enumeration rules above, the `family_key`
-- encoding, why each index exists, the traversal rules on `dependency`, the
-- `x-gts-ref` edge-naming rules that decide which rows exist at all, and every
-- place where an invariant is held by application code rather than by a
-- constraint.


-- A version family binds a family key to one ownership scope, and holds nothing
-- else. Under ADR-0008 the registry names no newest member and keeps no
-- current-member pointer, so there is no member count, no highest major, and no
-- family-scoped compare-and-swap. The single job of the row is to make
-- `owner_scope(version_successor) == owner_scope(version_family_root)`
-- enforceable by a uniqueness constraint plus an ordinary read, and to keep
-- concurrent first registration from creating one family under two owners.
--
-- `family_key` is the canonical GTS Identifier with the major version of its
-- LAST segment removed, every preceding segment held exactly as written, and the
-- trailing `~` of a type identifier normalized away (ADR-0004):
--
--   gts.acme.crm.customer.v1~                 -> gts.acme.crm.customer
--   gts.cf.core.events.type.v1~acme.order.v1~  -> gts.cf.core.events.type.v1~acme.order
--   gts.cf.core.events.type.v2~acme.order.v1~  -> gts.cf.core.events.type.v2~acme.order
--   gts.cf.core.events.topic.v1~acme.orders.v1 -> gts.cf.core.events.topic.v1~acme.orders
--
-- Dropping the kind marker is deliberate: it makes a name either a type family
-- or an Instance family and never both. A derived type
-- `gts.A~acme.orders.v1~` and a well-known Instance `gts.A~acme.orders.v1`
-- differ by one character and denote entirely unrelated things, and nothing
-- needs both - an Instance of that derived type is
-- `gts.A~acme.orders.v1~<segment>`, not the colliding form. A registry that
-- exists partly to catch naming accidents should catch this one.
--
-- Enforcement needs no kind column here. Both spellings map to one key, so the
-- second registrant finds this row, and admission - under the family lock it
-- already holds for the ownership check - reads any member and rejects a
-- candidate whose kind differs. Every member's identifier already carries its
-- kind, so a column would duplicate it and need an invariant to stay true.
--
-- Ownership is different, and the asymmetry is the reason it does get columns:
-- it must be fixed BEFORE any member exists, so that two concurrent first
-- registrations cannot assign one family to two owners. The kind constraint only
-- bites once a member exists, and the loser of that race blocks on this row
-- until the winner's member is visible, since family row and entity row commit
-- together.
--
-- With no members there is no constraint, which is the correct release after the
-- purge of ADR-0013. Ordinary deletion leaves member rows in place, so
-- it does not free the name.
--
-- A family key is NOT a GTS Identifier - it carries no version and no kind - so
-- it must not be parsed as one. The encoding is total: every managed identifier's
-- last segment carries a major version, because ADR-0004 forbids minor versions
-- and ADR-0001 forbids an explicit UUID tail.
--
-- There is no index on the owner columns. No P1 flow asks for the families a
-- tenant owns: discovery and search filter on entity, which carries its own
-- owner copy for exactly that reason. Ownership is also write-once - there is no
-- correction operation, since ADR-0013's purge lets a mis-assigned owner be
-- repaired by delete, purge, re-register - so nothing ever updates these columns
-- after the family is created.
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


-- Request identity and client-visible workflow state are one row: ADR-0012
-- admits no synchronous acceptance path, so there is no request without an
-- operation and nothing left for a second table to hold. `unchanged` survives
-- as the guarantee that a redundant submission creates no revision and does not
-- advance resource_version, not as a path a correct caller takes.
--
-- `idempotency_scope_hash` is a digest over (plane, tenant_id, principal_id).
-- The principal participates so that one
-- subject's key cannot return another subject's response, and with it another
-- subject's Registry References and resource versions, inside one tenant.
--
-- The digest is a correctness device, not a way to narrow the unique index. The
-- direct alternative, UNIQUE over the three scope columns plus the key, would
-- not enforce anything on the platform plane: tenant_id is NULL there, and
-- all three backends treat NULLs in a unique index as distinct, so two platform
-- operations with the same key and principal would both be admitted. Folding
-- the scope into a digest removes the NULL from the constraint.
--
-- `kind` names the mutation family: registration and its revisions (ADR-0012)
-- and deletion (ADR-0008). Additions extend the CHECK rather than bypassing it.
--
-- Purge is deliberately not a third value. It is a synchronous platform-plane
-- job (ADR-0013) that creates no operation at all, so it has nothing to store
-- here - which is also why this table needs no column for a request body: the
-- input of both kinds above is per candidate and lives in
-- operation_item.request_payload.
--
-- `dry_run` is orthogonal to `kind`, not a member of it: all three kinds have
-- the mode, and folding it in would double the vocabulary.
--
-- It is part of `request_fingerprint`, which is what keeps a dry run and the
-- real submission that follows it distinct requests under one Idempotency-Key.
-- Were it excluded, the real submission would replay the dry run's stored
-- operation and silently never execute.
--
-- Dry-run operations are one of the two classes nothing pins, the other being
-- an operation in which no candidate succeeded. A real successful one is
-- reachable from every revision it produced through operation_item_id, whose
-- foreign key is RESTRICT, so it is retained as long as those revisions are; a
-- dry run produces no revision and therefore no such reference. The unpinned
-- classes are what the retention sweep removes; see the comment on
-- idx_tr_operation_status.
--
-- There is no ownership-correction kind: ADR-0009 repairs a mis-assigned owner
-- by delete, purge, re-register, so ownership is immutable for an entity's life.
--
-- Worker leases, attempts, retries, and dead letters belong to the ToolKit
-- outbox processor tables and are not duplicated here.
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
    -- 1 pending, 2 running, 3 succeeded, 4 unchanged, 5 partially_succeeded,
    -- 6 failed.
    --
    -- The vocabulary has no cancellation and no expiry. Nothing asks to cancel a
    -- mutation. An operation whose worker dies is redelivered by the outbox and
    -- its commits are idempotent, so it becomes terminal only once retries are
    -- exhausted - at which point `failed` or `partially_succeeded` already says
    -- what happened and the per-item error_payload says why. A stalled operation
    -- past its timeout is failed for the same reason.
    status                   smallint     NOT NULL,
    created_at               timestamptz  NOT NULL,
    started_at               timestamptz  NULL,
    completed_at             timestamptz  NULL,

    CONSTRAINT pk_tr_operation PRIMARY KEY (id),
    CONSTRAINT uq_tr_operation_idem
        UNIQUE (idempotency_scope_hash, idempotency_key),
    CONSTRAINT ck_tr_operation_kind CHECK (kind IN (1, 2)),
    CONSTRAINT ck_tr_operation_plane CHECK (
        (plane = 1 AND tenant_id IS NULL)            -- platform
        OR
        (plane = 2 AND tenant_id IS NOT NULL)        -- tenant
    ),
    CONSTRAINT ck_tr_operation_status CHECK (
        status IN (1, 2, 3, 4, 5, 6)
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
        (status IN (3, 4, 5, 6)                      -- terminal outcomes
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL)
    )
);

-- Two jobs, both scans over the same two leading columns. It finds non-terminal
-- operations that stopped progressing, so they can be failed; and it finds
-- terminal operations old enough for the retention sweep below to remove. It
-- covers terminal rows, which will be the overwhelming majority over time; a
-- partial index would be far smaller but is not portable to MySQL, so the full
-- index is the deliberate choice.
--
-- Retention removes a terminal operation only when nothing points at it. A
-- successful one is pinned by every revision it produced, through
-- operation_item_id with ON DELETE RESTRICT, so it lives as long as those
-- revisions - which is until purge. What the sweep reaches is the unpinned
-- remainder: dry runs, which produce no revision by construction, and
-- operations in which no candidate succeeded. Deleting one cascades to its
-- items and releases its (idempotency_scope_hash, idempotency_key) pair, so a
-- replay after the retention window executes afresh instead of returning the
-- stored result. Sweeping the pinned majority as well would first require the
-- admitting principal to stop being reachable only through this table; see
-- DESIGN §4, open question D4.
CREATE INDEX idx_tr_operation_status
    ON types_registry__operation (status, created_at, id);


-- One durable candidate and public result per exact GTS Identifier.
--
-- The entity kind and the resulting Registry Reference are both absent because
-- both follow from `gts_id` in this same row - the kind from its trailing `~`,
-- the reference from its deterministic derivation. `entity` stores each of them
-- for a reason that does not apply here: there they lead an index or back a
-- uniqueness constraint, while these rows are only ever read by operation_id.
--
-- Each transition carries its own timestamp, so there is no `updated_at`. That
-- differs from `entity`, where transitions are not individually dated and only
-- resource_version orders them.
--
-- The optimistic precondition is one column, not a kind plus a value. Zero means
-- `must_not_exist`; any other value is the entity resource version to match. The
-- sentinel is injective only because ck_tr_entity_resource_version requires a
-- real version to be at least 1 - permitting 0 there would break this encoding
-- silently, so the two constraints belong together in the reader's mind.
-- Splitting it in two would make "must not exist, at version 7" representable
-- and then need a constraint to forbid it. ADR-0012 closes the vocabulary at
-- two, which is what makes a sentinel safe.
--
-- The contract carries the same value as one optional field, differing only in
-- the spelling of absence: the wire omits the field and rejects a literal 0.
--
-- `dry_run` is copied from the parent operation and is the one denormalized
-- column in this table. It exists because ck_tr_operation_item_state has to
-- branch on it and a CHECK cannot read another table: a dry-run item that
-- succeeded produced no revision, so the arm that normally requires
-- result_revision_no and result_resource_version has to require their absence
-- instead. Enforcing it here rather than trusting the worker is worth a column,
-- because the failure it prevents is a stored result claiming a revision that
-- was never written. It is written once with the row and never updated.
--
-- The public per-candidate vocabulary is deliberately not extended. A dry-run
-- item that passed every check terminates `succeeded`, not a fourth "would have
-- succeeded" value: the mode is a property of the operation the caller already
-- submitted, so restating it per item would be the second vocabulary that
-- cpt-cf-types-registry-principle-single-vocabulary forbids.
--
-- `request_payload` is dropped when an item reaches a terminal state. For a
-- successful item the content has moved into a revision; for a failed one it is
-- genuinely lost, which is the case where seeing the submission would help most.
-- That is accepted rather than overlooked: operations are retained for as long
-- as the revisions referencing them, so keeping rejected content would keep it
-- forever, the structured reason survives in `error_payload`, and the submitter
-- holds its own copy.
--
-- A purge deletes the rows naming the identifiers it releases (ADR-0013), so
-- that a later re-registration cannot produce a history in which one identifier
-- string spans two logical entities. Nothing here is exempt and no ordering
-- question arises: purge is synchronous and creates no operation of its own, so
-- it can never be deleting its own record.
CREATE TABLE types_registry__operation_item (
    id                        bigint        GENERATED BY DEFAULT AS IDENTITY,
    operation_id              uuid          NOT NULL,
    item_no                   integer       NOT NULL,
    gts_id                    varchar(1024) COLLATE "C" NOT NULL,
    -- Copied from operation.dry_run; see the comment above.
    dry_run                   boolean       NOT NULL,
    -- 0 means the candidate must not exist; otherwise the version to match
    expected_resource_version bigint        NOT NULL,
    -- 1 pending, 2 running, 3 succeeded, 4 unchanged, 5 failed.
    --
    -- There is no separate `blocked`. A status distinguishes outcomes that
    -- differ in effect and a reason distinguishes causes: `succeeded` and
    -- `unchanged` differ in whether a revision now exists, while a candidate
    -- rejected on its own merits and one never evaluated because an in-batch
    -- dependency failed differ in neither - both leave no entity, no revision
    -- and no resource_version increment, so they would share this table's CHECK
    -- arm verbatim. The second carries a `blocked_by_dependency` reason in
    -- error_payload, which a caller needs, since it may pass unchanged once the
    -- dependency is fixed.
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
    CONSTRAINT fk_tr_operation_item_operation
        FOREIGN KEY (operation_id)
        REFERENCES types_registry__operation (id) ON DELETE CASCADE,
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
            AND (
                (NOT dry_run                         -- committed: results exist
                    AND result_revision_no IS NOT NULL
                    AND result_resource_version IS NOT NULL)
                OR
                (dry_run                             -- nothing was written
                    AND result_revision_no IS NULL
                    AND result_resource_version IS NULL)
            ))
        OR
        (status = 5                                  -- failed
            AND request_payload IS NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NOT NULL
            AND completed_at IS NOT NULL)
    )
);

-- No index by status. Polling reads every item of one operation ordered by
-- item_no, which uq_tr_operation_item_no serves, and a batch is bounded, so
-- scanning its items costs less than maintaining a second index.


-- The logical registry entity: one row per admitted managed GTS Identifier,
-- of either kind. It is also the tombstone of a deleted identifier, which is
-- what keeps a previously issued Registry Reference reverse-resolvable.
--
-- Two columns look derivable and are stored deliberately, both to serve an
-- index rather than to record a fact:
--
--   * `gts_uuid` is UUIDv5 over `gts_id` under the GTS namespace of spec 5.1,
--     but the hash is not invertible, so reverse resolution needs an index over
--     the stored value. Its uniqueness is also the ADR-0001 collision detector:
--     a derivation that did collide is rejected at admission rather than
--     silently rebinding a stored domain reference. An expression index is not
--     an option - UUIDv5 over a namespace is not portable across the three
--     backends.
--
--     This column is what PRD and the ADRs call a Registry Reference. The SDK
--     and REST contracts expose it under this same name, so nothing is
--     translated at the boundary. ADR-0001 still forbids a gear to derive the
--     value locally; that prohibition now rests on SDK documentation and review
--     rather than on a name that concealed the value's shape.
--   * `entity_kind` follows from the trailing `~`, but a suffix predicate is
--     not portably indexable, and the column also carries the kind-conditional
--     constraints.
--
-- `lifecycle_status` has two values and no third. Under ADR-0008 no managed
-- entity ever carries `deprecated` in P1, and externally managed entities are
-- never stored here, so adding a value to this enumeration would be changing a
-- decision rather than extending a vocabulary.
--
-- Ownership is copied from version_family so that SecureORM can scope on this
-- row and every visibility filter avoids a join. The invariant that the copy
-- equals the family's owner is enforced by admission under the family row lock,
-- not by a constraint: a composite foreign key would be skipped entirely for
-- global entities, where owner_tenant_id is NULL and MATCH SIMPLE does not
-- check, so it would cover half the cases while looking complete.
--
-- `owning_gear` answers "who do I ask about this contract", which a global
-- entity otherwise cannot answer at all - ck_tr_entity_owner leaves its whole
-- owner side null. It is the gear name from `#[toolkit::gear(name = ...)]`,
-- mandatory for a global entity and optional for a tenant-owned one, whose
-- owner is already a tenant. See DESIGN §3.3, *Where the desired definitions
-- come from*, for why it is attribution rather than authority and why it is
-- rewritten on every admission instead of being write-once like the two
-- columns above.
--
-- Nothing may authorize on it. It is declared by the caller and cannot be
-- verified: in a single-process deployment every gear shares the process
-- workload identity, so the platform cannot tell which gear inside it is
-- registering.
--
-- No index. No P1 flow selects by owning gear, and adding one to serve a
-- future operator report is cheaper then than carrying it now.
CREATE TABLE types_registry__entity (
    id                       bigint        GENERATED BY DEFAULT AS IDENTITY,
    gts_uuid                 uuid          NOT NULL,
    gts_id                   varchar(1024) COLLATE "C" NOT NULL,
    -- 1 type_schema, 2 instance
    entity_kind              smallint      NOT NULL,
    family_id                bigint        NOT NULL,
    ownership_scope          smallint      NOT NULL, -- 1 global, 2 tenant
    owner_tenant_id          uuid          NULL,
    owning_gear              varchar(64)   NULL,
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

-- Exact family membership for discovery, which a GTS wildcard cannot express on
-- its own because it is greedy across the chain separator and so also captures
-- types derived from a member. `gts_id` is included to make the enumeration
-- covering, since what the caller wants back is the identifiers.
CREATE INDEX idx_tr_entity_family
    ON types_registry__entity (family_id, lifecycle_status, gts_id);

-- The workhorse of every read. A tenant-scoped query is `ownership_scope = 1`
-- (global) `OR (ownership_scope = 2 AND owner_tenant_id IN <ancestor chain>)`,
-- so it resolves to one index range per ancestor plus one for global, each
-- carrying its own `gts_id` range for a pattern scan. Tenant hierarchies are
-- shallow, so the fan-out is small.
--
-- There is deliberately no second index leading with `entity_kind`: every read
-- must filter visibility anyway, so a kind-led scan cannot avoid this index.
-- If filtering by kind ever becomes hot, `entity_kind` belongs inside this
-- index rather than in a competing one.
CREATE INDEX idx_tr_entity_visibility
    ON types_registry__entity (
        ownership_scope,
        owner_tenant_id,
        lifecycle_status,
        gts_id
    );


-- Immutable admission snapshot: the authored document exactly as admitted, its
-- hash, and the provenance needed to scope a later repair.
--
-- Neither the effective artifacts nor the dependency revision vector are kept,
-- for the same reason: nothing reads the admission-time resolution.
-- Compatibility compares a candidate against the current revision, and the one
-- backward-looking operation - ADR-0003's repair after the compatibility
-- relation changes meaning - resolves both sides against the dependencies
-- current at repair time. `gts_spec_version` and `gts_impl_version` are what
-- scope that repair to the chains admitted under superseded rules.
--
-- The dependency vector does exist during validation, as the concurrency
-- control the commit re-checks, but it lives in the worker for one attempt: a
-- redelivered outbox message revalidates from scratch.
--
-- The admitting principal is not duplicated: `operation_item_id` is NOT NULL and
-- its foreign key is RESTRICT, so the operation and its principal are always
-- reachable. That also settles a retention question by construction - revisions
-- are retained until purge, so the operations that produced them are too. This
-- is affordable because an operation row is narrow and `request_payload` is
-- already dropped when an item reaches a terminal state.
--
-- The primary key is the natural one, `(entity_id, revision_no)`, and there is no
-- surrogate beside it. Every foreign key into this table already targets that
-- pair rather than an opaque handle, because each referencing row needs both
-- components as facts of its own: `type_schema` must carry which revision is
-- current, `instance_revision` which revision validated a value, `instance`
-- which one last revalidated it. A surrogate would leave those rows holding a
-- number they cannot read and a join to recover it. Making the pair the key also
-- clusters the revisions of one entity together on a clustering engine, which is
-- both access patterns this table has - one revision of one entity, and its
-- history in order.
CREATE TABLE types_registry__type_schema_revision (
    entity_id                  bigint       NOT NULL,
    revision_no                integer      NOT NULL,
    raw_schema                 text         NOT NULL,
    content_hash               bytea        NOT NULL,
    gts_spec_version           varchar(32)  NOT NULL,
    gts_impl_version           varchar(32)  NOT NULL,
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

-- There is no index on (entity_id, content_hash). The no-op equality proof
-- compares a candidate against the current revision, which is one row reached
-- through type_schema, and re-submitting content equal to an older non-current
-- revision is admitted as a new revision under ADR-0005 rather than looked up.


-- Immutable admission snapshot of one registered Instance value, with the exact
-- Type Schema revision that validated it (ADR-0006). It keeps no dependency
-- revision vector and no admitting principal, for the same reasons as
-- type_schema_revision: nothing reads the admission-time resolution, and the
-- principal is reachable through operation_item_id.
--
-- The primary key is the natural `(entity_id, revision_no)`, for the reasons
-- given on type_schema_revision, and the validating Type Schema revision is
-- referenced the same way.
--
-- `type_schema_entity_id` is also derivable from the Instance identifier -
-- the chain up to and including the last `~`, normative per GTS spec 11.1 - and
-- is materialized here to carry the composite foreign key.
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

-- No index by content hash, for the same reason as on type_schema_revision, and
-- none by Type Schema revision: revalidation when a schema advances runs
-- over current Instances, which idx_tr_instance_schema serves.


-- Current state of a Type Schema, as opposed to type_schema_revision, which is
-- its history. It holds only what actually differs from the revision it points
-- at, so nothing is duplicated between the two: the authored document, its hash,
-- and the checker versions are reached by joining on (entity_id, revision_no),
-- the foreign key already declared below.
--
-- What differs is the resolution: these artifacts are resolved against the
-- dependencies current NOW, and are recomputed when a floating dependency
-- advances without producing a new authored revision here. That divergence is
-- why this is a distinct fact rather than a cache of the revision.
--
-- Per-level content-model classification is not stored. It is a pure function of
-- `resolved_schema` in this same row, it is wanted only off the hot path by an
-- owner or a CI check asking whether a level can still gain an optional
-- property, and a compatibility check returns it as a by-product anyway.
--
-- `resolution_fingerprint` is a digest over the canonical bytes of the three
-- artifact columns, rewritten whenever they are recomputed. It is a content
-- digest and deliberately NOT a counter: a dependency-driven recompute very
-- often reproduces byte-identical artifacts, and a counter would invalidate
-- every consumer's cache for a change that did not reach them. Equality is the
-- only operation defined on it - it carries no order and must never be read as
-- newer or older.
--
-- It exists because read freshness and write concurrency are different axes:
-- `entity.resource_version` guards writes and must not move when only a base
-- advanced, yet a consumer holding a resolved schema still has to learn that the
-- registry would now answer differently - and a conditional read must answer
-- that without fetching and hashing a large document. It participates in the
-- resolution validator alongside `entity.resource_version` and the tenant
-- ancestor-chain version, and never in optimistic concurrency.
--
-- The digest input must be canonical and independent of the serializer's map
-- iteration order, or the value flaps without the artifacts changing.
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


-- Current state of a registered Instance, as opposed to instance_revision,
-- which is its history. Everything the revision already holds - the value, its
-- hash, the schema revision it was admitted against, the checker versions - is
-- reached by joining on (entity_id, revision_no).
--
-- Only one thing is genuinely current state: `validated_type_schema_revision_no`,
-- which advances when a newer schema revision revalidates this unchanged value.
-- There is no counterpart to type_schema's resolution fingerprint, because an
-- Instance has no derived form to drift. Its value is authored and immutable per
-- revision, so a read result cannot go stale without the entity itself changing,
-- and `entity.resource_version` with the tenant ancestor-chain version is a
-- complete validator for it.
CREATE TABLE types_registry__instance (
    entity_id                         bigint      NOT NULL,
    revision_no                       integer     NOT NULL,
    type_schema_entity_id             bigint      NOT NULL,
    validated_type_schema_revision_no integer     NOT NULL,
    created_at                        timestamptz NOT NULL,
    updated_at                        timestamptz NOT NULL,

    CONSTRAINT pk_tr_instance PRIMARY KEY (entity_id),
    CONSTRAINT fk_tr_instance_revision_ptr
        FOREIGN KEY (entity_id, revision_no)
        REFERENCES types_registry__instance_revision (
            entity_id,
            revision_no
        )
        ON DELETE CASCADE,
    CONSTRAINT fk_tr_instance_validated_schema
        FOREIGN KEY (
            type_schema_entity_id,
            validated_type_schema_revision_no
        )
        REFERENCES types_registry__type_schema_revision (
            entity_id,
            revision_no
        )
        ON DELETE RESTRICT
);

CREATE INDEX idx_tr_instance_schema
    ON types_registry__instance (
        type_schema_entity_id,
        validated_type_schema_revision_no
    );


-- The single dependency relation: `from_entity_id` depends on `to_entity_id`.
--
-- Every row is a DIRECT dependency, whatever its origin. Nothing transitive is
-- stored; a transitive question is answered by a recursive CTE over this table
-- rather than by a second, materialized one. Deletion safety reads the direct
-- rows and only those, since a transitive-only dependent must not block - it
-- would disappear the moment the intermediate entity did.
--
-- Derivation and Instance conformance are stored even though both follow from
-- the identifier, and the reason is the query rather than the fact. A recursive
-- CTE may reference itself exactly once on all three backends, so a second
-- recursive branch joining `entity.gts_id` by prefix range is not expressible;
-- folding it into one branch with `OR` would abandon the indexes. Uniformity of
-- the relation is what makes one query possible. The cost is small and the drift
-- risk near zero: one derivation edge points at the immediate base, not at the
-- whole chain, it is written once at admission from `chain_ids()`, and it never
-- changes because an identifier never changes.
--
-- Traversal MUST use `UNION`, never `UNION ALL`. The graph can contain cycles -
-- ADR-0012 admits cyclic dependency components, and mutually recursive `$ref` is
-- ordinary JSON Schema - and `UNION ALL` would not terminate on one. The failure
-- would also diverge across backends, which is what the multi-backend constraint
-- forbids: MySQL stops at cte_max_recursion_depth, while PostgreSQL and SQLite
-- have no limit and would run until memory is gone.
--
-- The recursive term MUST NOT carry a depth or any other per-row accumulator.
-- `UNION` deduplicates whole rows, so a depth column makes every revisit distinct
-- and reinstates the non-termination that `UNION` was there to prevent. Depth
-- would not give a usable order anyway - distance from the source is not a
-- topological order once two paths reach one node. Ordering comes from a second
-- query for the edges among the affected set, then strongly connected component
-- condensation and a topological sort in the worker, which ADR-0012 already
-- requires for the candidate graph inside a batch.
--
-- Maintenance is entirely local: admission replaces the rows for one entity and
-- touches nothing else. No rule reaches sideways, and in particular admitting an
-- entity never adds an edge to some other entity's row set.
--
-- That follows from how an `x-gts-ref` becomes an edge. The keyword constrains
-- what an instance value may name; it is not itself a dependency, which is why
-- the strict reference extractor in gts-rust excludes it from schema resolution.
-- An edge therefore points at the entity the value or the constraint **names**:
--
--   * an exact identifier names that entity;
--   * a wildcard pattern names the longest prefix of itself that is a valid GTS
--     identifier - `...topic.v1~*` and `...topic.v1~x.core.*` both name
--     `gts.cf.core.events.topic.v1~`;
--   * a pattern naming nothing valid, such as `gts.*`, produces no edge, and so
--     does the `/$id` self-reference of GTS spec 9.6.
--
-- Nothing here depends on the open set of entities a pattern matches, so
-- registering a new entity under an existing pattern requires no re-expansion,
-- and a `dependency_pattern` table with its reverse-lookup index is unnecessary.
-- Storing one and expanding matches into edges is the alternative, and the
-- difficulty of keeping that expansion current is the signal that it models the
-- wrong fact.
--
-- The edge protects the named target, not the satisfiability of the constraint.
-- Deleting `topic.v1~` is refused because the pattern names it and the reference
-- would dangle. Deleting every type under `...~x.core.*` while the base survives
-- is permitted even though it empties the set of admissible values: the subject
-- depends on no particular member, and protecting satisfiability would mean
-- depending on an open set.
--
-- One asymmetry follows and is accepted. A named base is a dependency for
-- deletion but not for revalidation: when it admits a revision the subject's
-- effective form does not change, since the subject holds a pattern string
-- rather than the base's content. The traversal reaches the subject anyway,
-- recomputes it, finds an identical digest, and stops there.
--
-- A materialized transitive closure was considered and rejected: it answers the
-- same question in the same number of round trips, since a CTE loops inside the
-- engine, and its failure mode is a silent under-report that skips revalidation
-- and admits an incompatible change. If measurement shows the CTE is too slow on
-- MySQL, whose recursive-CTE implementation is the weakest of the three, a
-- closure may return as a cache over these rows - never as a replacement.
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

-- Serves both the reverse hop of the recursive CTE and deletion safety, which
-- now reads all three categories of ADR-0011 in one query instead of a reverse
-- lookup plus a prefix range scan.
CREATE INDEX idx_tr_dependency_to
    ON types_registry__dependency (to_entity_id, from_entity_id);


-- Routing configuration as a whole. One row, and it exists for two jobs that
-- turn out to be the same job.
--
-- It serializes claim mutation. Source Claim overlap cannot be a constraint:
-- uq_tr_source_claim_gts_id_pattern catches an exact duplicate, but
-- `gts.acme.*` and `gts.acme.foo.*` are different strings that overlap, and no
-- unique index expresses that. Validation runs outside the transaction on the asynchronous
-- write path, so two activations could each see no overlap and both commit.
-- Locking this row for the duration of any claim mutation closes that window.
-- There is nothing else to lock: the invariant is about the absence of an
-- overlapping row, and a row that does not exist cannot be locked.
--
-- It carries `generation`, bumped in the same transaction. Federated pagination
-- cursors bind to it so they go stale when routing changes, and the in-memory
-- claim set - a few rows, so it is held in memory rather than queried - uses it
-- to know when to reload. A counter rather than a digest, unlike
-- type_schema.resolution_fingerprint: there a recomputation often reproduces
-- identical artifacts and a counter would invalidate consumers for nothing,
-- whereas here every mutation of a claim is by definition a change.
CREATE TABLE types_registry__routing_config (
    id          smallint    NOT NULL,
    generation  bigint      NOT NULL,
    updated_at  timestamptz NOT NULL,

    CONSTRAINT pk_tr_routing_config PRIMARY KEY (id),
    CONSTRAINT ck_tr_routing_config_singleton CHECK (id = 1),
    CONSTRAINT ck_tr_routing_config_generation CHECK (generation >= 1)
);


-- Active claims and permanent retired reservations share one identity, because a
-- retired claim is a reservation over the same identifier space and must keep
-- blocking managed registration there (ADR-0011).
--
-- The row is a projection of a registered Instance of the Types Registry source
-- plugin type, itself derived from `gts.cf.toolkit.plugins.plugin.v1~`, so
-- `priority` and `plugin_entity_gts_id` mirror that base type's `priority` and
-- `id` rather than being invented here. A projection is needed even though the
-- claim set is small enough to hold in memory: a retired reservation outlives the
-- plugin Instance that created it, and the overlap invariant should be
-- checkable relationally instead of by parsing every plugin document.
--
-- `plugin_entity_gts_id` and `retired_at` survive retirement while the rest of
-- the plugin columns are cleared, so a reservation still names the plugin it
-- belonged to and the purge that removes it can find its rows.
--
-- There is no claim takeover operation (ADR-0011): activating a claim over a
-- retired reservation is rejected with no exception, because the continuity a
-- successor would assert cannot be checked against anything - Types Registry
-- persists nothing about what the predecessor served. Retargeting a reservation
-- is consequently a hand-written migration, and two things it must not omit are
-- noted in DESIGN §3.2: it bumps routing_config.generation under that row's
-- lock, and it leaves the successor plugin Instance document and this projection
-- in agreement, since the next ordinary revision of that Instance is validated
-- against these rows.
--
-- The claim's lifecycle mirrors the lifecycle of the plugin Instance it projects,
-- because it is that Instance seen from the routing side:
--
--   Instance active   -> claim active
--   Instance deleted  -> claim retired, a reservation over the same space
--   Instance purged   -> claim row removed, the space released
--
-- So retirement needs no operation of its own: deleting the Instance is the
-- governance act, and it clears the plugin columns here, which also releases the
-- foreign key before a later purge can be blocked by it. A purge then removes the
-- claim rows, found by plugin_entity_gts_id, which is why that column survives
-- retirement. Both transitions change routing and so bump
-- routing_config.generation under its lock.
--
-- Retirement is never an observation of liveness: an unreachable plugin keeps
-- its claims, and the request fails closed instead (ADR-0011). There is
-- therefore no health or last-seen column here, and nothing writes `retired_at`
-- except the deletion of the plugin Instance.
--
-- `priority` is a property of the plugin, not of the claim: PluginV1 carries one
-- value, and ADR-0007 orders plugins rather than claims. A plugin declaring
-- several claims therefore repeats it across its rows, and the invariant that
-- they agree is maintained by the projection rather than by a constraint.
--
-- There is no stored upper bound and no index. Claim counts are single digits by
-- design - ADR-0011 rejected pinning the wildcard to the version precisely
-- because it would force one claim per type family - so the authoritative GTS
-- matcher runs over the whole set, and a string range would only have been a
-- pre-filter it had to confirm anyway.
CREATE TABLE types_registry__source_claim (
    id                         bigint        GENERATED BY DEFAULT AS IDENTITY,
    gts_id_pattern             varchar(1024) COLLATE "C" NOT NULL,
    status                     smallint      NOT NULL, -- 1 active, 2 retired
    plugin_entity_gts_id       varchar(1024) COLLATE "C" NOT NULL,
    plugin_entity_id           bigint        NULL,
    plugin_entity_revision_no  integer       NULL,
    -- Bitmask over entity_kind: bit `1 << (entity_kind - 1)`, so 1 type_schema,
    -- 2 instance, 3 both. A set rather than a single value, kept numeric so it
    -- agrees with the entity_kind enumeration instead of restating its names.
    entity_kinds               smallint      NULL,
    priority                   smallint      NULL,
    created_at                 timestamptz   NOT NULL,
    updated_at                 timestamptz   NOT NULL,
    retired_at                 timestamptz   NULL,

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
        ON DELETE RESTRICT,
    -- Upper bound is 3 while there are two entity kinds; a third kind in P2
    -- widens it to 7.
    CONSTRAINT ck_tr_source_claim_kinds
        CHECK (entity_kinds IS NULL OR entity_kinds BETWEEN 1 AND 3),
    CONSTRAINT ck_tr_source_claim_state CHECK (
        (status = 1                                  -- active
            AND plugin_entity_id IS NOT NULL
            AND plugin_entity_revision_no IS NOT NULL
            AND entity_kinds IS NOT NULL
            AND priority IS NOT NULL
            AND retired_at IS NULL)
        OR
        (status = 2                                  -- retired
            AND plugin_entity_id IS NULL
            AND plugin_entity_revision_no IS NULL
            AND entity_kinds IS NULL
            AND priority IS NULL
            AND retired_at IS NOT NULL)
    )
);
