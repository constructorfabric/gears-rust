-- Created:  2026-03-06 by Constructor Tech
-- Updated:  2026-03-16 by Constructor Tech

-- ── GTS type path domain ─────────────────────────────────────────────────────
-- GTS type identifier: single or chained, always ends with ~ (schema, not instance).
-- Format: gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>][~<segment>]*~
-- Spec:   https://github.com/GlobalTypeSystem/gts-spec
CREATE DOMAIN gts_type_path AS TEXT
    CHECK (
        LENGTH(VALUE) <= 1024
        AND VALUE ~ '^gts\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?(?:~[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?)*~$'
    );

-- ── GTS types ────────────────────────────────────────────────────────────────

CREATE TABLE gts_type (
    id SMALLINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    schema_id gts_type_path NOT NULL UNIQUE,
    metadata_schema JSONB,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NULL
);

COMMENT ON TABLE gts_type
    IS 'GTS type definitions. Stores raw GTS JSON Schema documents. schema_id = GTS type path ($id without gts:// prefix). can_be_root is resolved at runtime from x-gts-traits in the registered GTS schema (not stored as a column). metadata_schema stores the JSON Schema for the metadata object of instances of this type (e.g. tenant barrier, department category). Surrogate SMALLINT id used as FK from resource_group, resource_group_membership, and junction tables.';

-- ── GTS type relationships (junction tables) ─────────────────────────────────

CREATE TABLE gts_type_allowed_parent (
    type_id        SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    parent_type_id SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    PRIMARY KEY (type_id, parent_type_id)
);

COMMENT ON TABLE gts_type_allowed_parent
    IS 'Many-to-many: which GTS types are allowed as parents for a given RG type. E.g. department → tenant means departments can be children of tenants.';

CREATE TABLE gts_type_allowed_membership (
    type_id            SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    membership_type_id SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    PRIMARY KEY (type_id, membership_type_id)
);

COMMENT ON TABLE gts_type_allowed_membership
    IS 'Many-to-many: which resource types are allowed as members of groups of a given RG type. E.g. branch → user means users can be members of branches.';

-- NOTE: The placement invariant (can_be_root OR at least one allowed_parent) is enforced at the
-- application layer. can_be_root is resolved from x-gts-traits in the registered GTS schema.

-- ── Resource groups ──────────────────────────────────────────────────────────

CREATE TABLE resource_group (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id UUID,
    gts_type_id SMALLINT NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata JSONB,
    tenant_id UUID NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NULL,
    CONSTRAINT fk_rg_gts_type
        FOREIGN KEY (gts_type_id)
        REFERENCES gts_type(id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_resource_group_parent
        FOREIGN KEY (parent_id)
        REFERENCES resource_group(id)
        ON UPDATE CASCADE
        ON DELETE RESTRICT
);

-- ── resource_group indexes ─────────────────────────────────────────────────

-- parent_id: equality and IN filters
CREATE INDEX idx_rg_parent_id
    ON resource_group (parent_id);

-- name: equality and IN filters
CREATE INDEX idx_rg_name
    ON resource_group (name);

-- gts_type_id + id: composite allows seek by type and ordered scan by id (avoids PK scan + filter)
CREATE INDEX idx_rg_gts_type_id
    ON resource_group (gts_type_id, id);

-- tenant_id: SecureORM injects WHERE tenant_id IN (...) on every query via AccessScope
CREATE INDEX idx_rg_tenant_id
    ON resource_group (tenant_id);

COMMENT ON TABLE resource_group
    IS 'Hierarchical resource groups with closure table pattern for efficient ancestor/descendant queries';
COMMENT ON COLUMN resource_group.parent_id
    IS 'Direct parent group reference; NULL for root groups (e.g. top-level tenants)';
COMMENT ON COLUMN resource_group.gts_type_id
    IS 'Reference to gts_type.id defining the type of this resource group';

-- ── Closure table ────────────────────────────────────────────────────────────

CREATE TABLE resource_group_closure (
    ancestor_id UUID NOT NULL,
    descendant_id UUID NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    PRIMARY KEY (ancestor_id, descendant_id),
    CONSTRAINT fk_closure_ancestor
        FOREIGN KEY (ancestor_id)
        REFERENCES resource_group(id)
        ON UPDATE CASCADE
        ON DELETE RESTRICT,
    CONSTRAINT fk_closure_descendant
        FOREIGN KEY (descendant_id)
        REFERENCES resource_group(id)
        ON UPDATE CASCADE
        ON DELETE RESTRICT
);

COMMENT ON TABLE resource_group_closure
    IS 'Closure table for resource group hierarchy - stores all ancestor-descendant relationships with depth';
COMMENT ON COLUMN resource_group_closure.depth
    IS 'Distance between ancestor and descendant: 0 = self-reference, 1 = direct descendant, 2+ = deeper descendants';

-- Closure indexes: JOIN on descendant_id and filter by ancestor+depth
CREATE INDEX idx_rgc_descendant_id
    ON resource_group_closure (descendant_id);

CREATE INDEX idx_rgc_ancestor_depth
    ON resource_group_closure (ancestor_id, depth);

-- ── Memberships ──────────────────────────────────────────────────────────────

CREATE TABLE resource_group_membership (
    group_id UUID NOT NULL,
    gts_type_id SMALLINT NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id)
        REFERENCES resource_group(id)
        ON UPDATE CASCADE
        ON DELETE RESTRICT,
    CONSTRAINT fk_rgm_gts_type
        FOREIGN KEY (gts_type_id)
        REFERENCES gts_type(id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_resource_group_membership_unique
        UNIQUE (group_id, gts_type_id, resource_id)
);

-- ── resource_group_membership indexes ──────────────────────────────────────

-- gts_type_id + resource_id (without group_id): supports membership lookups by resource
CREATE INDEX idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
