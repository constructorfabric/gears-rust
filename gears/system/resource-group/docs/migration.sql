-- Created:  2026-03-06 by Constructor Tech
-- Updated:  2026-03-16 by Constructor Tech

-- Reference DDL (PostgreSQL) for the current schema. Resource group types are NOT stored here:
-- they are GTS Type Schemas in the Types Registry. Rows reference a type by its Types Registry
-- Registry Reference (UUID) with no foreign key. The tables gts_type, gts_type_allowed_parent,
-- gts_type_allowed_membership and the gts_type_path domain no longer exist.

-- ── Resource groups ──────────────────────────────────────────────────────────

CREATE TABLE resource_group (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id UUID,
    gts_type_id UUID NOT NULL,
    is_tenant BOOLEAN NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata JSONB,
    tenant_id UUID NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NULL,
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

-- At most one tenant root group in the whole deployment.
CREATE UNIQUE INDEX uq_rg_tenant_root
    ON resource_group (is_tenant) WHERE parent_id IS NULL AND is_tenant;

COMMENT ON TABLE resource_group
    IS 'Hierarchical resource groups with closure table pattern for efficient ancestor/descendant queries';
COMMENT ON COLUMN resource_group.parent_id
    IS 'Direct parent group reference; NULL for root groups (e.g. top-level tenants)';
COMMENT ON COLUMN resource_group.gts_type_id
    IS 'Types Registry Registry Reference (UUID) of the group type; no foreign key, resolved through the Types Registry';
COMMENT ON COLUMN resource_group.is_tenant
    IS 'TRUE when the group type is the platform tenant type; derived from the type id at creation, never changes';

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
    gts_type_id UUID NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id)
        REFERENCES resource_group(id)
        ON UPDATE CASCADE
        ON DELETE RESTRICT,
    CONSTRAINT uq_resource_group_membership_unique
        UNIQUE (group_id, gts_type_id, resource_id)
);

COMMENT ON COLUMN resource_group_membership.gts_type_id
    IS 'Types Registry Registry Reference (UUID) of the member resource type; no foreign key';

-- ── resource_group_membership indexes ──────────────────────────────────────

-- gts_type_id + resource_id (without group_id): supports membership lookups by resource
CREATE INDEX idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
