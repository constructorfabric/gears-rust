-- PostgreSQL DDL created by `m20261005_000001_create_upstreams_and_routes` and
-- `m20261012_000001_create_registry_state`. `oagw_plugin` is not created yet.
--
-- Query frequency:
--  * per miss    — a proxied request that misses the proxy configuration cache
--                  (ADR-0020): 1 SELECT per section that missed, none on a hit
--  * per request — every proxied request with the cache off (3 SELECTs)
--  * per call    — Management API / SDK CRUD
--  * per check   — registry reconcile gate, every pod once per re-check interval
--  * per run     — registry reconcile, only when the registry content changed
--
-- Child tables are read by parent id only, so they need no index beyond the
-- primary key (on MySQL, InnoDB adds one per child foreign key). Child foreign keys include `tenant_id`, so a child row always
-- has its parent's tenant.


-- ── Upstreams ────────────────────────────────────────────────────────────────
CREATE TABLE oagw_upstream (
    id                  UUID          NOT NULL,
    tenant_id           UUID          NOT NULL,
    alias               VARCHAR(253)  NOT NULL,
    protocol            VARCHAR(256)  NOT NULL,
    enabled             BOOLEAN       NOT NULL,
    managed_by          VARCHAR(16)   NOT NULL,   -- 'api' | 'registry'
    schema_version      INTEGER       NOT NULL DEFAULT 1,
    server              JSONB         NOT NULL,
    auth_plugin_ref     VARCHAR(256),
    auth_plugin_uuid    UUID,
    auth_config         JSONB,
    auth_sharing        VARCHAR(16)   NOT NULL,   -- 'private' | 'inherit' | 'enforce'
    headers             JSONB,
    cors                JSONB,
    cors_sharing        VARCHAR(16)   NOT NULL,
    rate_limit          JSONB,
    rate_limit_sharing  VARCHAR(16)   NOT NULL,
    plugins_sharing     VARCHAR(16),              -- NULL = no plugins configuration
    created_at          TIMESTAMPTZ   NOT NULL,
    updated_at          TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id)                              -- get / update / delete (per call)
);

-- Alias resolution across the tenant chain (per miss; per request with the
-- cache off); ancestor and budget lookups (per call).
CREATE UNIQUE INDEX uq_oagw_upstream_tenant_alias ON oagw_upstream (tenant_id, alias);

-- Child foreign keys; upstream list (per call).
CREATE UNIQUE INDEX uq_oagw_upstream_tenant_id ON oagw_upstream (tenant_id, id);

-- Registry reconcile (per run).
CREATE INDEX idx_oagw_upstream_managed_by ON oagw_upstream (managed_by);


CREATE TABLE oagw_upstream_tag (
    upstream_id  UUID          NOT NULL,
    tenant_id    UUID          NOT NULL,
    tag          VARCHAR(128)  NOT NULL,

    PRIMARY KEY (upstream_id, tag),
    CONSTRAINT fk_oagw_upstream_tag_upstream
        FOREIGN KEY (tenant_id, upstream_id) REFERENCES oagw_upstream (tenant_id, id)
        ON DELETE CASCADE
);


CREATE TABLE oagw_upstream_plugin (
    upstream_id     UUID          NOT NULL,
    tenant_id       UUID          NOT NULL,
    "position"      INTEGER       NOT NULL,
    plugin_ref      VARCHAR(256)  NOT NULL,
    plugin_uuid     UUID,
    schema_version  INTEGER       NOT NULL DEFAULT 1,
    config          JSONB,

    PRIMARY KEY (upstream_id, "position"),
    CONSTRAINT fk_oagw_upstream_plugin_upstream
        FOREIGN KEY (tenant_id, upstream_id) REFERENCES oagw_upstream (tenant_id, id)
        ON DELETE CASCADE
);


-- ── Routes ───────────────────────────────────────────────────────────────────
CREATE TABLE oagw_route (
    id                  UUID          NOT NULL,
    tenant_id           UUID          NOT NULL,
    upstream_id         UUID          NOT NULL,
    enabled             BOOLEAN       NOT NULL,
    priority            INTEGER       NOT NULL,
    match_type          VARCHAR(16)   NOT NULL,   -- 'http' | 'grpc'
    managed_by          VARCHAR(16)   NOT NULL,   -- 'api' | 'registry'
    schema_version      INTEGER       NOT NULL DEFAULT 1,
    match_config        JSONB,
    cors                JSONB,
    rate_limit          JSONB,
    rate_limit_sharing  VARCHAR(16)   NOT NULL,
    plugins_sharing     VARCHAR(16),              -- NULL = no plugins configuration
    created_at          TIMESTAMPTZ   NOT NULL,
    updated_at          TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id),                             -- get / update / delete (per call)
    CONSTRAINT fk_oagw_route_upstream
        FOREIGN KEY (tenant_id, upstream_id) REFERENCES oagw_upstream (tenant_id, id)
        ON DELETE CASCADE
);

-- Child foreign keys; route list (per call).
CREATE UNIQUE INDEX uq_oagw_route_tenant_id ON oagw_route (tenant_id, id);

-- Route load of the cache (per miss); route candidates (per request with the
-- cache off); route list and conflict check (per call); cascade from
-- oagw_upstream.
CREATE INDEX idx_oagw_route_tenant_upstream ON oagw_route (tenant_id, upstream_id);

-- Registry reconcile (per run).
CREATE INDEX idx_oagw_route_managed_by ON oagw_route (managed_by);


CREATE TABLE oagw_route_http_match (
    route_id     UUID           NOT NULL,
    tenant_id    UUID           NOT NULL,
    path_prefix  VARCHAR(2048)  NOT NULL,

    PRIMARY KEY (route_id),
    CONSTRAINT fk_oagw_route_http_match_route
        FOREIGN KEY (tenant_id, route_id) REFERENCES oagw_route (tenant_id, id)
        ON DELETE CASCADE
);


CREATE TABLE oagw_route_method (
    route_id   UUID         NOT NULL,
    tenant_id  UUID         NOT NULL,
    method     VARCHAR(16)  NOT NULL,

    PRIMARY KEY (route_id, method),
    CONSTRAINT fk_oagw_route_method_route
        FOREIGN KEY (tenant_id, route_id) REFERENCES oagw_route (tenant_id, id)
        ON DELETE CASCADE
);


CREATE TABLE oagw_route_grpc_match (
    route_id   UUID          NOT NULL,
    tenant_id  UUID          NOT NULL,
    service    VARCHAR(256)  NOT NULL,
    method     VARCHAR(256)  NOT NULL,

    PRIMARY KEY (route_id),
    CONSTRAINT fk_oagw_route_grpc_match_route
        FOREIGN KEY (tenant_id, route_id) REFERENCES oagw_route (tenant_id, id)
        ON DELETE CASCADE
);


CREATE TABLE oagw_route_tag (
    route_id   UUID          NOT NULL,
    tenant_id  UUID          NOT NULL,
    tag        VARCHAR(128)  NOT NULL,

    PRIMARY KEY (route_id, tag),
    CONSTRAINT fk_oagw_route_tag_route
        FOREIGN KEY (tenant_id, route_id) REFERENCES oagw_route (tenant_id, id)
        ON DELETE CASCADE
);


CREATE TABLE oagw_route_plugin (
    route_id        UUID          NOT NULL,
    tenant_id       UUID          NOT NULL,
    "position"      INTEGER       NOT NULL,
    plugin_ref      VARCHAR(256)  NOT NULL,
    plugin_uuid     UUID,
    schema_version  INTEGER       NOT NULL DEFAULT 1,
    config          JSONB,

    PRIMARY KEY (route_id, "position"),
    CONSTRAINT fk_oagw_route_plugin_route
        FOREIGN KEY (tenant_id, route_id) REFERENCES oagw_route (tenant_id, id)
        ON DELETE CASCADE
);


-- ── Registry reconcile state ──────────────────────────────────────────────────
-- One installation-wide row (no tenant_id), seeded by the migration. Read
-- per check (generation, content_hash); updated per run (lease, claim, hash).
-- The lease times are written by the database clock only.
CREATE TABLE oagw_registry_state (
    id                SMALLINT      NOT NULL,   -- always 1
    generation        BIGINT,                   -- highest deployment generation that claimed it
    content_hash      VARCHAR(64),              -- hex SHA-256 of the last clean reconcile
    applied_at        TIMESTAMPTZ,              -- when content_hash was recorded
    lease_owner       UUID,                     -- process holding the reconcile lease
    lease_expires_at  TIMESTAMPTZ,              -- when the lease lapses

    PRIMARY KEY (id)
);

INSERT INTO oagw_registry_state (id)
SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM oagw_registry_state WHERE id = 1);


-- ── Deferred ─────────────────────────────────────────────────────────────────
-- With `oagw_plugin`: indexes on oagw_upstream (auth_plugin_uuid),
-- oagw_upstream_plugin (plugin_uuid) and oagw_route_plugin (plugin_uuid).
