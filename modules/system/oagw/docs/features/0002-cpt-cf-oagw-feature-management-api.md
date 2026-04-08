# Feature: Upstream & Route Management


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create Upstream Flow](#create-upstream-flow)
  - [Update Upstream Flow](#update-upstream-flow)
  - [Delete Upstream Flow](#delete-upstream-flow)
  - [List and Get Upstreams Flow](#list-and-get-upstreams-flow)
  - [Create Route Flow](#create-route-flow)
  - [Route Update, Delete, List, and Get Flow](#route-update-delete-list-and-get-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Upstream Validation Algorithm](#upstream-validation-algorithm)
  - [Alias Enforcement Algorithm](#alias-enforcement-algorithm)
  - [Route Validation Algorithm](#route-validation-algorithm)
  - [Enable/Disable Propagation Algorithm](#enabledisable-propagation-algorithm)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Upstream CRUD Handlers](#implement-upstream-crud-handlers)
  - [Implement Route CRUD Handlers](#implement-route-crud-handlers)
  - [Implement Alias Enforcement and Uniqueness](#implement-alias-enforcement-and-uniqueness)
  - [Implement Enable/Disable Semantics](#implement-enabledisable-semantics)
  - [Implement OData Query Support for List Endpoints](#implement-odata-query-support-for-list-endpoints)
  - [Implement RFC 9457 Error Responses](#implement-rfc-9457-error-responses)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context](#7-additional-context)
  - [Performance Considerations](#performance-considerations)
  - [Security Considerations](#security-considerations)
  - [Observability Considerations](#observability-considerations)
  - [Compliance Considerations](#compliance-considerations)
  - [Accessibility / UX Considerations](#accessibility--ux-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-oagw-featstatus-management-api-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-oagw-feature-management-api`

## 1. Feature Context

### 1.1 Overview

Implement Control Plane CRUD operations for upstreams and routes with REST API handlers, validation, alias enforcement, enable/disable semantics, OData query support, and RFC 9457 error responses.

### 1.2 Purpose

Provides the management API for configuring upstreams and routes — the core configuration objects that define how OAGW routes and processes outbound API requests. Covers `cpt-cf-oagw-fr-upstream-mgmt`, `cpt-cf-oagw-fr-route-mgmt`, `cpt-cf-oagw-fr-enable-disable`, `cpt-cf-oagw-fr-error-codes`.

Adheres to `cpt-cf-oagw-principle-tenant-scope` (all operations tenant-scoped via secure ORM) and `cpt-cf-oagw-principle-rfc9457` (all error responses use Problem Details format with GTS type identifiers).

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Creates and manages global upstream/route configurations |
| `cpt-cf-oagw-actor-tenant-admin` | Manages tenant-scoped upstream/route configurations |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADRs**: [0009 Storage Schema](../ADR/0009-storage-schema.md), [0010 Resource Identification](../ADR/0010-resource-identification.md)
- **Dependencies**: `cpt-cf-oagw-feature-domain-foundation`

**Out of scope**:

- Plugin CRUD (`cpt-cf-oagw-feature-plugin-system`)
- Proxy endpoint (`cpt-cf-oagw-feature-proxy-engine`)
- Hierarchical configuration merge and sharing modes (`cpt-cf-oagw-feature-tenant-hierarchy`)

## 2. Actor Flows (CDSL)

### Create Upstream Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-mgmt-create-upstream`

**Actor**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Upstream is created with enforced alias (auto-derived or explicit) and persisted to database
- Response contains the created upstream with GTS anonymous identifier

**Error Scenarios**:
- Validation fails (invalid endpoint format, missing required fields)
- Alias conflict: `(tenant_id, alias)` uniqueness violation
- Credential reference invalid (malformed `secret_ref` format)

**Steps**:
1. [ ] - `p1` - Actor sends POST /api/oagw/v1/upstreams with server endpoints, protocol, auth config, headers, rate limit config, tags - `inst-create-us-1`
2. [ ] - `p1` - API: Extract SecurityContext (tenant_id, principal_id, permissions) - `inst-create-us-2`
3. [ ] - `p1` - API: Validate actor has `gts.x.core.oagw.upstream.v1~:create` permission - `inst-create-us-3`
4. [ ] - `p1` - API: Deserialize and validate DTO structure - `inst-create-us-4`
5. [ ] - `p1` - Domain: Execute upstream validation algorithm (`cpt-cf-oagw-algo-mgmt-validate-upstream`) - `inst-create-us-5`
6. [ ] - `p1` - **IF** validation fails - `inst-create-us-6`
   1. [ ] - `p1` - **RETURN** 400 ValidationError (RFC 9457 Problem Details) - `inst-create-us-6a`
7. [x] - `p1` - Domain: Execute alias enforcement algorithm (`cpt-cf-oagw-algo-mgmt-enforce-alias`) - `inst-create-us-7`
8. [ ] - `p1` - DB: BEGIN transaction - `inst-create-us-8`
9. [ ] - `p1` - DB: INSERT oagw_upstream (id, tenant_id, alias, protocol, enabled, server_config, auth_config, headers_config, rate_limit_config, cors_config, plugins_config) - `inst-create-us-9`
10. [ ] - `p1` - DB: INSERT oagw_upstream_tag for each tag in request - `inst-create-us-10`
11. [ ] - `p1` - **IF** `(tenant_id, alias)` uniqueness violation - `inst-create-us-11`
    1. [ ] - `p1` - DB: ROLLBACK - `inst-create-us-11a`
    2. [ ] - `p1` - **RETURN** 409 Conflict - `inst-create-us-11b`
12. [ ] - `p1` - DB: COMMIT - `inst-create-us-12`
13. [ ] - `p1` - **RETURN** 201 Created with upstream resource (GTS ID: `gts.x.core.oagw.upstream.v1~{uuid}`) - `inst-create-us-13`

### Update Upstream Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-mgmt-update-upstream`

**Actor**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Upstream configuration is updated; alias is re-enforced if endpoints change
- Alias-only update (no endpoint change) is accepted for IP-based endpoints; normalized and validated
- Response contains the updated upstream

**Error Scenarios**:
- Upstream not found (wrong ID or tenant)
- Validation fails (same as create)
- Alias conflict after re-enforcement
- Alias override rejected for hostname-based endpoints (400 Validation) — applies to both endpoint-change and alias-only updates
- Hostname→IP endpoint transition without explicit alias (400 Validation)

**Steps**:
1. [ ] - `p1` - Actor sends PUT /api/oagw/v1/upstreams/{id} with updated configuration - `inst-update-us-1`
2. [ ] - `p1` - API: Extract SecurityContext and validate `gts.x.core.oagw.upstream.v1~:override` permission - `inst-update-us-2`
3. [ ] - `p1` - API: Parse GTS anonymous identifier from path to extract UUID - `inst-update-us-3`
4. [ ] - `p1` - DB: SELECT oagw_upstream WHERE id = :uuid AND tenant_id = :tenant_id - `inst-update-us-4`
5. [ ] - `p1` - **IF** upstream not found - `inst-update-us-5`
   1. [ ] - `p1` - **RETURN** 404 Not Found - `inst-update-us-5a`
6. [ ] - `p1` - Domain: Execute upstream validation algorithm (`cpt-cf-oagw-algo-mgmt-validate-upstream`) - `inst-update-us-6`
7. [ ] - `p1` - **IF** server endpoints changed - `inst-update-us-7`
   1. [x] - `p1` - Domain: Re-execute alias enforcement algorithm (`cpt-cf-oagw-algo-mgmt-enforce-alias`) with old/new endpoint transition rules - `inst-update-us-7a`
   2. [ ] - `p1` - **ELSE IF** alias field provided without endpoint change - `inst-update-us-7b`
      1. [x] - `p1` - **IF** endpoints are hostname-based AND normalized alias differs from derived value → **RETURN** 400 Validation: "alias cannot be overridden for hostname-based endpoints" - `inst-update-us-7b1`
      2. [x] - `p1` - **ELSE** (IP-based endpoints): normalize and validate alias; accept update - `inst-update-us-7b2`
8. [ ] - `p1` - DB: BEGIN transaction - `inst-update-us-8`
9. [ ] - `p1` - DB: UPDATE oagw_upstream SET (updated fields) WHERE id = :uuid - `inst-update-us-9`
10. [ ] - `p1` - DB: DELETE + re-INSERT oagw_upstream_tag for updated tags - `inst-update-us-10`
11. [ ] - `p1` - **IF** `(tenant_id, alias)` uniqueness violation - `inst-update-us-11`
    1. [ ] - `p1` - DB: ROLLBACK - `inst-update-us-11a`
    2. [ ] - `p1` - **RETURN** 409 Conflict - `inst-update-us-11b`
12. [ ] - `p1` - DB: COMMIT - `inst-update-us-12`
13. [ ] - `p1` - **RETURN** 200 OK with updated upstream resource - `inst-update-us-13`

### Delete Upstream Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-mgmt-delete-upstream`

**Actor**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Upstream and all associated routes are deleted (cascade)

**Error Scenarios**:
- Upstream not found

**Steps**:
1. [ ] - `p1` - Actor sends DELETE /api/oagw/v1/upstreams/{id} - `inst-delete-us-1`
2. [ ] - `p1` - API: Extract SecurityContext and validate `gts.x.core.oagw.upstream.v1~:delete` permission - `inst-delete-us-2`
3. [ ] - `p1` - API: Parse GTS anonymous identifier from path to extract UUID - `inst-delete-us-3`
4. [ ] - `p1` - DB: SELECT oagw_upstream WHERE id = :uuid AND tenant_id = :tenant_id - `inst-delete-us-4`
5. [ ] - `p1` - **IF** upstream not found - `inst-delete-us-5`
   1. [ ] - `p1` - **RETURN** 404 Not Found - `inst-delete-us-5a`
6. [ ] - `p1` - DB: DELETE oagw_upstream WHERE id = :uuid (cascades to oagw_route, oagw_upstream_tag, oagw_upstream_plugin) - `inst-delete-us-6`
7. [ ] - `p1` - **RETURN** 204 No Content - `inst-delete-us-7`

### List and Get Upstreams Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-mgmt-list-upstreams`

**Actor**: `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- List returns paginated upstreams for the tenant with OData query support
- Get returns a single upstream by GTS anonymous identifier

**Error Scenarios**:
- Invalid OData query syntax
- Upstream not found (get by ID)

**Steps**:
1. [ ] - `p1` - Actor sends GET /api/oagw/v1/upstreams[?$filter=...&$select=...&$orderby=...&$top=...&$skip=...] or GET /api/oagw/v1/upstreams/{id} - `inst-list-us-1`
2. [ ] - `p1` - API: Extract SecurityContext and validate `gts.x.core.oagw.upstream.v1~:read` permission - `inst-list-us-2`
3. [ ] - `p1` - **IF** list request - `inst-list-us-3`
   1. [ ] - `p1` - API: Parse OData query parameters ($filter, $select, $orderby, $top with default 50 / max 100, $skip) - `inst-list-us-3a`
   2. [ ] - `p1` - **IF** OData parse error - `inst-list-us-3b`
      1. [ ] - `p1` - **RETURN** 400 ValidationError with parse error details - `inst-list-us-3b1`
   3. [ ] - `p1` - DB: SELECT oagw_upstream WHERE tenant_id = :tenant_id with OData filters applied - `inst-list-us-3c`
   4. [ ] - `p1` - **RETURN** 200 OK with paginated upstream list - `inst-list-us-3d`
4. [ ] - `p1` - **IF** get-by-ID request - `inst-list-us-4`
   1. [ ] - `p1` - API: Parse GTS anonymous identifier from path - `inst-list-us-4a`
   2. [ ] - `p1` - DB: SELECT oagw_upstream WHERE id = :uuid AND tenant_id = :tenant_id - `inst-list-us-4b`
   3. [ ] - `p1` - **IF** not found - `inst-list-us-4c`
      1. [ ] - `p1` - **RETURN** 404 Not Found - `inst-list-us-4c1`
   4. [ ] - `p1` - **RETURN** 200 OK with upstream resource - `inst-list-us-4d`

### Create Route Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-mgmt-create-route`

**Actor**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Route is created and linked to its upstream
- Response contains the created route with GTS anonymous identifier

**Error Scenarios**:
- Referenced upstream not found or not owned by tenant
- Validation fails (invalid match rules, duplicate priority/path combination)

**Steps**:
1. [ ] - `p1` - Actor sends POST /api/oagw/v1/routes with upstream_id, match rules (type, path, methods, query allowlist), priority, enabled, rate limit, cors, plugins, tags - `inst-create-rt-1`
2. [ ] - `p1` - API: Extract SecurityContext and validate `gts.x.core.oagw.route.v1~:create` permission - `inst-create-rt-2`
3. [ ] - `p1` - API: Deserialize and validate DTO structure - `inst-create-rt-3`
4. [ ] - `p1` - Domain: Execute route validation algorithm (`cpt-cf-oagw-algo-mgmt-validate-route`) - `inst-create-rt-4`
5. [ ] - `p1` - **IF** validation fails - `inst-create-rt-5`
   1. [ ] - `p1` - **RETURN** 400 ValidationError (RFC 9457 Problem Details) - `inst-create-rt-5a`
6. [ ] - `p1` - DB: BEGIN transaction - `inst-create-rt-6`
7. [ ] - `p1` - DB: INSERT oagw_route (id, tenant_id, upstream_id, match_type, priority, enabled, rate_limit_config, cors_config, plugins_config) - `inst-create-rt-7`
8. [ ] - `p1` - DB: INSERT oagw_route_http_match or oagw_route_grpc_match based on match_type - `inst-create-rt-8`
9. [ ] - `p1` - DB: INSERT oagw_route_method for each allowed method - `inst-create-rt-9`
10. [ ] - `p1` - DB: INSERT oagw_route_tag for each tag - `inst-create-rt-10`
11. [ ] - `p1` - DB: COMMIT - `inst-create-rt-11`
12. [ ] - `p1` - **RETURN** 201 Created with route resource (GTS ID: `gts.x.core.oagw.route.v1~{uuid}`) - `inst-create-rt-12`

### Route Update, Delete, List, and Get Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-mgmt-route-crud`

**Actor**: `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Update: Route configuration updated; match rules re-validated
- Delete: Route and associated match/method/tag rows deleted (cascade)
- List: Paginated routes with OData query support; filterable by upstream_id
- Get: Single route by GTS anonymous identifier

**Error Scenarios**:
- Route not found (wrong ID or tenant)
- Update validation fails
- Invalid OData query syntax

**Steps**:
1. [ ] - `p1` - **IF** PUT /api/oagw/v1/routes/{id} - `inst-route-crud-1`
   1. [ ] - `p1` - API: Extract SecurityContext and validate `gts.x.core.oagw.route.v1~:override` permission - `inst-route-crud-1a`
   2. [ ] - `p1` - DB: SELECT oagw_route WHERE id = :uuid AND tenant_id = :tenant_id - `inst-route-crud-1b`
   3. [ ] - `p1` - **IF** not found, **RETURN** 404 Not Found - `inst-route-crud-1c`
   4. [ ] - `p1` - Domain: Execute route validation algorithm (`cpt-cf-oagw-algo-mgmt-validate-route`) - `inst-route-crud-1d`
   5. [ ] - `p1` - DB: BEGIN transaction; UPDATE oagw_route; DELETE + re-INSERT match/method/tag rows; COMMIT - `inst-route-crud-1e`
   6. [ ] - `p1` - **RETURN** 200 OK with updated route - `inst-route-crud-1f`
2. [ ] - `p1` - **IF** DELETE /api/oagw/v1/routes/{id} - `inst-route-crud-2`
   1. [ ] - `p1` - API: Validate `gts.x.core.oagw.route.v1~:delete` permission - `inst-route-crud-2a`
   2. [ ] - `p1` - DB: SELECT oagw_route WHERE id = :uuid AND tenant_id = :tenant_id - `inst-route-crud-2b`
   3. [ ] - `p1` - **IF** not found, **RETURN** 404 Not Found - `inst-route-crud-2c`
   4. [ ] - `p1` - DB: DELETE oagw_route WHERE id = :uuid (cascades to match/method/tag/plugin rows) - `inst-route-crud-2d`
   5. [ ] - `p1` - **RETURN** 204 No Content - `inst-route-crud-2e`
3. [ ] - `p1` - **IF** GET /api/oagw/v1/routes or GET /api/oagw/v1/routes/{id} - `inst-route-crud-3`
   1. [ ] - `p1` - API: Validate `gts.x.core.oagw.route.v1~:read` permission - `inst-route-crud-3a`
   2. [ ] - `p1` - **IF** list: Parse OData params; DB: SELECT with filters; **RETURN** 200 paginated list - `inst-route-crud-3b`
   3. [ ] - `p1` - **IF** get-by-ID: Parse GTS identifier; DB: SELECT by id+tenant; **RETURN** 200 or 404 - `inst-route-crud-3c`

## 3. Processes / Business Logic (CDSL)

### Upstream Validation Algorithm

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-mgmt-validate-upstream`

**Input**: Upstream creation/update payload, tenant_id from SecurityContext

**Output**: Validation result with errors array

**Steps**:
1. [ ] - `p1` - Parse and normalize input fields - `inst-val-us-1`
2. [ ] - `p1` - **IF** server.endpoints is empty - `inst-val-us-2`
   1. [ ] - `p1` - Add error: "At least one server endpoint is required" - `inst-val-us-2a`
3. [ ] - `p1` - **FOR EACH** endpoint in server.endpoints - `inst-val-us-3`
   1. [ ] - `p1` - **IF** scheme not in [https, wss, webtransport, grpc] - `inst-val-us-3a`
      1. [ ] - `p1` - Add error: "Unsupported scheme: {scheme}" - `inst-val-us-3a1`
   2. [ ] - `p1` - **IF** host is empty or not a valid hostname/IP - `inst-val-us-3b`
      1. [ ] - `p1` - Add error: "Invalid host: {host}" - `inst-val-us-3b1`
   3. [ ] - `p1` - **IF** port is out of range (1-65535) - `inst-val-us-3c`
      1. [ ] - `p1` - Add error: "Invalid port: {port}" - `inst-val-us-3c1`
4. [ ] - `p1` - **IF** multiple endpoints exist - `inst-val-us-4`
   1. [ ] - `p1` - **IF** endpoints have mixed protocols, schemes, or ports - `inst-val-us-4a`
      1. [ ] - `p1` - Add error: "All endpoints in a pool must share the same protocol, scheme, and port" - `inst-val-us-4a1`
5. [ ] - `p1` - **IF** auth config contains secret_ref - `inst-val-us-5`
   1. [ ] - `p1` - **IF** secret_ref does not match `cred://` URI format - `inst-val-us-5a`
      1. [ ] - `p1` - Add error: "Invalid secret_ref format; expected cred:// URI" - `inst-val-us-5a1`
6. [ ] - `p1` - **IF** protocol not in [http, grpc] - `inst-val-us-6`
   1. [ ] - `p1` - Add error: "Unsupported protocol: {protocol}" - `inst-val-us-6a`
7. [ ] - `p1` - **RETURN** { valid: errors.length == 0, errors } - `inst-val-us-7`

### Alias Enforcement Algorithm

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-mgmt-enforce-alias`

**Input**: Server endpoints list, optional explicit alias from request, existing upstream (for update path)

**Output**: Resolved alias string or validation error

Alias behavior is determined entirely by endpoint type. Hostname-based endpoints always auto-derive; IP-based endpoints require explicit alias.

**Create path** (`enforce_alias_create`):
1. [x] - `p1` - Attempt to derive alias from endpoints (`compute_derived_alias`) - `inst-alias-1`
2. [x] - `p1` - **IF** derivable (hostname-based) - `inst-alias-2`
   1. [x] - `p1` - **IF** user provided explicit alias AND it differs from derived value - `inst-alias-2a`
      1. [x] - `p1` - **RETURN** 400 Validation: "alias is auto-derived for hostname-based endpoints" - `inst-alias-2a1`
   2. [x] - `p1` - **RETURN** derived alias (normalized: lowercase, trailing dots stripped) - `inst-alias-2b`
3. [x] - `p1` - **IF** not derivable (IP-based or no common suffix) - `inst-alias-3`
   1. [x] - `p1` - **IF** no explicit alias provided - `inst-alias-3a`
      1. [x] - `p1` - **RETURN** 400 Validation: "explicit alias is required for IP-based or heterogeneous-host endpoints" - `inst-alias-3a1`
   2. [x] - `p1` - Normalize and validate explicit alias (charset, length) - `inst-alias-3b`
   3. [x] - `p1` - **RETURN** normalized alias - `inst-alias-3c`

**Update path** (`enforce_alias_update` — when endpoints change):
1. [x] - `p1` - Determine old and new endpoint derivability - `inst-alias-upd-1`
2. [x] - `p1` - **IF** new endpoints are hostname-based (derivable) - `inst-alias-upd-2`
   1. [x] - `p1` - Recompute alias from new endpoints; reject user-provided alias if different - `inst-alias-upd-2a`
3. [x] - `p1` - **IF** old derivable → new non-derivable (derivable→non-derivable transition) - `inst-alias-upd-3`
   1. [x] - `p1` - **IF** no explicit alias provided - `inst-alias-upd-3a`
      1. [x] - `p1` - **RETURN** 400 Validation: "explicit alias is required for IP-based or heterogeneous-host endpoints" - `inst-alias-upd-3a1`
   2. [x] - `p1` - **RETURN** normalized user alias - `inst-alias-upd-3b`
4. [x] - `p1` - **IF** IP → IP (no transition) - `inst-alias-upd-4`
   1. [x] - `p1` - Retain existing alias unless user provides a new one - `inst-alias-upd-4a`

**Alias-only update path** (endpoints unchanged, alias field provided):
1. [x] - `p1` - **IF** endpoints are hostname-based (derivable) AND normalized alias differs from derived value - `inst-alias-only-1`
   1. [x] - `p1` - **RETURN** 400 Validation: "alias cannot be overridden for hostname-based endpoints" - `inst-alias-only-1a`
2. [x] - `p1` - **ELSE** (IP-based or exact-match with derived): normalize, validate, and accept - `inst-alias-only-2`

**Derivation rules** (`compute_derived_alias`):
- Single hostname, standard port → hostname (e.g., `api.openai.com`)
- Single hostname, non-standard port → hostname:port (e.g., `api.openai.com:8443`)
- Multiple hostnames, all identical → treated as single-host
- Multiple hostnames, common domain suffix (≥2 labels) that is a registrable domain (i.e., has at least one label beyond the public suffix per the PSL) → common suffix (e.g., `vendor.com`); non-standard port appended (e.g., `vendor.com:8443`)
- Multiple hostnames, common suffix is a public suffix only (e.g., `co.uk`, `com.au`) or only TLD in common → `None` (explicit required); the PSL check prevents shared public suffixes from being returned as derived aliases
- IP addresses → `None` (explicit required)

**Standard ports** (omitted from derived alias): HTTP: 80, HTTPS/WSS/WebTransport/gRPC: 443.

**Hostname validation** (RFC 1123): max 253 chars total, each label 1–63 chars, labels ASCII alphanumeric + hyphen only, labels cannot start/end with hyphen. Trailing dot (FQDN) tolerated.

### Route Validation Algorithm

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-mgmt-validate-route`

**Input**: Route creation/update payload, tenant_id from SecurityContext

**Output**: Validation result with errors array

**Steps**:
1. [ ] - `p1` - Parse and normalize input fields - `inst-val-rt-1`
2. [ ] - `p1` - DB: SELECT oagw_upstream WHERE id = :upstream_id AND tenant_id = :tenant_id - `inst-val-rt-2`
3. [ ] - `p1` - **IF** upstream not found - `inst-val-rt-3`
   1. [ ] - `p1` - Add error: "Referenced upstream does not exist or is not accessible" - `inst-val-rt-3a`
4. [ ] - `p1` - **IF** match_type == "http" - `inst-val-rt-4`
   1. [ ] - `p1` - **IF** match.http.path is empty - `inst-val-rt-4a`
      1. [ ] - `p1` - Add error: "HTTP match path is required" - `inst-val-rt-4a1`
   2. [ ] - `p1` - **IF** match.http.methods contains invalid HTTP method - `inst-val-rt-4b`
      1. [ ] - `p1` - Add error: "Invalid HTTP method: {method}" - `inst-val-rt-4b1`
5. [ ] - `p1` - **IF** match_type == "grpc" - `inst-val-rt-5`
   1. [ ] - `p1` - **IF** match.grpc.service is empty - `inst-val-rt-5a`
      1. [ ] - `p1` - Add error: "gRPC service name is required" - `inst-val-rt-5a1`
6. [ ] - `p1` - **IF** priority is not a positive integer - `inst-val-rt-6`
   1. [ ] - `p1` - Add error: "Priority must be a positive integer" - `inst-val-rt-6a`
7. [ ] - `p1` - DB: SELECT oagw_route WHERE upstream_id = :upstream_id AND priority = :priority AND enabled = true - `inst-val-rt-7`
8. [ ] - `p1` - **IF** existing enabled route shares same path_prefix and priority (HTTP) or same service and method (gRPC), excluding self on update - `inst-val-rt-8`
   1. [ ] - `p1` - Add error: "Route match conflict: another enabled route with same priority and path prefix exists" - `inst-val-rt-8a`
9. [ ] - `p1` - **RETURN** { valid: errors.length == 0, errors } - `inst-val-rt-9`

### Enable/Disable Propagation Algorithm

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-mgmt-enable-disable`

**Input**: Resource (upstream or route), new enabled value, tenant hierarchy context

**Output**: Updated enabled state or rejection error

**Steps**:
1. [ ] - `p1` - **IF** setting enabled = true (re-enabling) - `inst-endis-1`
   1. [ ] - `p1` - **IF** resource is an upstream - `inst-endis-1a`
      1. [ ] - `p1` - Walk tenant hierarchy from current tenant to root - `inst-endis-1a1`
      2. [ ] - `p1` - **IF** any ancestor has the same alias with enabled = false - `inst-endis-1a2`
         1. [ ] - `p1` - **RETURN** error: "Cannot re-enable: ancestor upstream is disabled" - `inst-endis-1a2a`
   2. [ ] - `p1` - **IF** resource is a route - `inst-endis-1b`
      1. [ ] - `p1` - **IF** parent upstream is disabled - `inst-endis-1b1`
         1. [ ] - `p1` - **RETURN** error: "Cannot enable route: parent upstream is disabled" - `inst-endis-1b1a`
2. [ ] - `p1` - DB: UPDATE resource SET enabled = :new_value - `inst-endis-2`
3. [ ] - `p1` - **RETURN** success - `inst-endis-3`

## 4. States (CDSL)

Not applicable. Upstreams and routes use a boolean `enabled` flag (true/false) rather than a multi-state lifecycle. Enable/disable semantics are handled by the enable/disable propagation algorithm (`cpt-cf-oagw-algo-mgmt-enable-disable`), not by state machine transitions.

## 5. Definitions of Done

### Implement Upstream CRUD Handlers

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-mgmt-upstream-crud`

The system **MUST** provide REST handlers for POST, GET (list + by-ID), PUT, and DELETE operations on `/api/oagw/v1/upstreams` with tenant-scoped data access via secure ORM. DTOs **MUST** use serde and utoipa annotations. Path parameters **MUST** accept GTS anonymous identifiers (`gts.x.core.oagw.upstream.v1~{uuid}`).

**Implements**:
- `cpt-cf-oagw-flow-mgmt-create-upstream`
- `cpt-cf-oagw-flow-mgmt-update-upstream`
- `cpt-cf-oagw-flow-mgmt-delete-upstream`
- `cpt-cf-oagw-flow-mgmt-list-upstreams`
- `cpt-cf-oagw-algo-mgmt-validate-upstream`

**Touches**:
- API: `POST /api/oagw/v1/upstreams`, `GET /api/oagw/v1/upstreams`, `GET /api/oagw/v1/upstreams/{id}`, `PUT /api/oagw/v1/upstreams/{id}`, `DELETE /api/oagw/v1/upstreams/{id}`
- DB: `oagw_upstream`, `oagw_upstream_tag`, `oagw_upstream_plugin`
- Entities: Upstream, ServerConfig, Endpoint

### Implement Route CRUD Handlers

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-mgmt-route-crud`

The system **MUST** provide REST handlers for POST, GET (list + by-ID), PUT, and DELETE operations on `/api/oagw/v1/routes` with tenant-scoped data access via secure ORM. Route creation **MUST** validate upstream_id existence. DTOs **MUST** use serde and utoipa annotations. Path parameters **MUST** accept GTS anonymous identifiers (`gts.x.core.oagw.route.v1~{uuid}`).

**Implements**:
- `cpt-cf-oagw-flow-mgmt-create-route`
- `cpt-cf-oagw-flow-mgmt-route-crud`
- `cpt-cf-oagw-algo-mgmt-validate-route`

**Touches**:
- API: `POST /api/oagw/v1/routes`, `GET /api/oagw/v1/routes`, `GET /api/oagw/v1/routes/{id}`, `PUT /api/oagw/v1/routes/{id}`, `DELETE /api/oagw/v1/routes/{id}`
- DB: `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`, `oagw_route_tag`, `oagw_route_plugin`
- Entities: Route

### Implement Alias Enforcement and Uniqueness

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-mgmt-alias-enforcement`

The system **MUST** enforce alias rules based on endpoint type following `cpt-cf-oagw-algo-mgmt-enforce-alias`. Hostname-based endpoints auto-derive alias (user-provided alias is rejected). IP-based or non-derivable endpoints require explicit alias. Aliases **MUST** be normalized (ASCII lowercase, trailing dot stripped) and unique per `(tenant_id, alias)` with a database uniqueness constraint. On update, alias is re-enforced when endpoints change per the transition rules (hostname→hostname recomputes, hostname→IP requires explicit, IP→IP retains, IP→hostname recomputes). Alias-only updates (no endpoint change) **MUST** follow the same hostname/IP branching: hostname-based endpoints reject user-provided alias (unless it exactly matches the derived value); IP-based or non-derivable endpoints accept and normalize the new alias. Alias normalization and the `(tenant_id, alias)` uniqueness constraint apply to alias-only updates. Tests **MUST** cover the alias-only branches for both hostname-based (rejection) and IP-based (acceptance) endpoints. Endpoint hostnames **MUST** be validated per RFC 1123.

**Implements**:
- `cpt-cf-oagw-algo-mgmt-enforce-alias`

**Touches**:
- DB: `oagw_upstream` (alias column, UNIQUE constraint on `(tenant_id, alias)`)
- Entities: Upstream, ServerConfig, Endpoint

### Implement Enable/Disable Semantics

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-mgmt-enable-disable`

The system **MUST** support an `enabled` boolean field (default: `true`) on upstreams and routes. The system **MUST** prevent descendants from re-enabling an ancestor-disabled upstream. The system **MUST** prevent enabling a route whose parent upstream is disabled.

**Implements**:
- `cpt-cf-oagw-algo-mgmt-enable-disable`

**Touches**:
- DB: `oagw_upstream` (enabled column), `oagw_route` (enabled column)
- Entities: Upstream, Route

### Implement OData Query Support for List Endpoints

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-mgmt-odata-query`

The system **MUST** support OData query parameters on upstream and route list endpoints: `$filter`, `$select`, `$orderby`, `$top` (default: 50, max: 100), and `$skip`. Invalid OData syntax **MUST** return 400 ValidationError.

**Implements**:
- `cpt-cf-oagw-flow-mgmt-list-upstreams`
- `cpt-cf-oagw-flow-mgmt-route-crud`

**Touches**:
- API: `GET /api/oagw/v1/upstreams`, `GET /api/oagw/v1/routes`
- DB: `oagw_upstream`, `oagw_route`

### Implement RFC 9457 Error Responses

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-mgmt-error-responses`

The system **MUST** return all management API errors in RFC 9457 Problem Details format (`application/problem+json`) with GTS `type` identifiers. Error responses **MUST** include `type`, `title`, `status`, and `detail` fields. Management-specific error types: ValidationError (400), Not Found (404), Conflict (409).

**Implements**:
- All flows (error scenarios)

**Touches**:
- API: All management endpoints
- Entities: Error response DTOs

## 6. Acceptance Criteria

- [ ] Upstream CRUD: create, read (single + list), update, and delete operations work with tenant scoping via secure ORM
- [ ] Route CRUD: create, read (single + list), update, and delete operations work with upstream reference validation and tenant scoping
- [x] Alias enforcement: hostname-based endpoints auto-derive alias (user-provided rejected); IP-based/non-derivable require explicit alias; aliases normalized (lowercase, trailing dots stripped); update re-enforces on endpoint change per transition rules
- [x] Hostname validation: RFC 1123 (max 253 chars, labels 1–63 chars, ASCII alphanumeric + hyphen, no leading/trailing hyphen)
- [ ] `(tenant_id, alias)` uniqueness enforced at database level; 409 Conflict returned on violation
- [ ] OData $filter, $select, $orderby, $top (default 50, max 100), $skip supported on list endpoints; invalid syntax returns 400
- [ ] Enable/disable: disabled upstream causes proxy requests to be rejected (503); disabled route excluded from matching; ancestor-disabled upstream cannot be re-enabled by descendant
- [ ] All error responses use RFC 9457 Problem Details with GTS type identifiers
- [ ] All operations require appropriate GTS permissions and return 403 on unauthorized access
- [ ] Path parameters accept GTS anonymous identifiers (`gts.x.core.oagw.{type}.v1~{uuid}`)
- [ ] DTOs annotated with serde (serialization) and utoipa (OpenAPI schema generation)

## 7. Additional Context

### Performance Considerations

Not applicable. Management API operations are CRUD against the database and are not on the hot path (proxy execution). No special latency requirements beyond standard ModKit API response times.

### Security Considerations

All management operations enforce bearer token authentication via `modkit-auth` and tenant scoping via secure ORM. No credentials are stored directly — auth config references use `cred://` URIs resolved by `cred_store` at proxy time, not at management time. Management operations validate credential reference format but do not resolve or test credential accessibility.

### Observability Considerations

Not applicable for this feature. Structured audit logging for configuration changes (create/update/delete) is covered by `cpt-cf-oagw-feature-observability`.

### Compliance Considerations

Not applicable. No PII handling, no regulatory data — upstream/route configurations are technical metadata.

### Accessibility / UX Considerations

Not applicable. This feature is a backend REST API with no user-facing UI.
