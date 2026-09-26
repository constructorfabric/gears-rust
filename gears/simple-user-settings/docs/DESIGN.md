# DESIGN

## 1. Architecture Overview

Simple User Settings is implemented as a ToolKit gear with database persistence. It provides a REST API for CRUD operations on user settings.

**System Context**: Operates as a lightweight service gear within Gears, using the platform's database layer for storage.

## 2. Design Principles

### Simplicity

**ID**: [ ] `p2` `fdd-user-settings-principle-simplicity-v1`

<!-- fdd-id-content -->
Minimal API surface. No complex query language. Straightforward key-value model.
<!-- fdd-id-content -->

### Security First

**ID**: [ ] `p2` `fdd-user-settings-principle-security-v1`

<!-- fdd-id-content -->
Tenant isolation enforced at DB layer. User authentication required. No anonymous access.
<!-- fdd-id-content -->

## 3. Constraints

### Data Size

**ID**: [ ] `p2` `fdd-user-settings-constraint-size-v1`

<!-- fdd-id-content -->
Maximum 1MB per user settings document. Individual key-value pairs limited to 100KB.
<!-- fdd-id-content -->

### Schema

**ID**: [ ] `p2` `fdd-user-settings-constraint-schema-v1`

<!-- fdd-id-content -->
Free-form JSON storage. No enforced schema validation. Application responsible for data structure.
<!-- fdd-id-content -->

## 4. Components

### REST Endpoints

**ID**: [ ] `p1` `fdd-user-settings-component-rest-v1`

<!-- fdd-id-content -->
- `GET /simple-user-settings/v1/settings` - Retrieve all settings
- `GET /simple-user-settings/v1/settings/{key}` - Retrieve specific setting
- `PUT /simple-user-settings/v1/settings` - Update settings
- `DELETE /simple-user-settings/v1/settings/{key}` - Delete setting
<!-- fdd-id-content -->

### Settings Service

**ID**: [ ] `p1` `fdd-user-settings-component-service-v1`

<!-- fdd-id-content -->
Handles CRUD operations. Enforces tenant scoping. Validates request payloads.

Decides the user half of the key once per request: the token subject, unless the
deployment has registered a `SettingsOwnerResolver` on the `ClientHub`, in which
case its answer (bounded by `owner_resolver_timeout_ms`) is the key and the
resource id sent to the PDP. The tenant half is always the caller's tenant. The
contract a resolver must honour is on the trait in the SDK.

The gear does not retry a resolver call. A failure or a timeout is answered once:
unavailable-like categories and timeouts as `503 Service Unavailable`, refusals
as the usual masked denial, anything else as `500`. Retrying is the caller's
choice, and it is safe: the lookup is a read with no side effects, and nothing is
written before it succeeds. `owner_resolver_timeout_ms` must be between 1 and
30000; the gear refuses to start otherwise.
<!-- fdd-id-content -->

### Database Repository

**ID**: [ ] `p1` `fdd-user-settings-component-repository-v1`

<!-- fdd-id-content -->
Persists settings to database. Uses toolkit-db for database access. Implements tenant isolation via security context.
<!-- fdd-id-content -->

## 5. Data Model

**Settings Entity**:
- `user_id`: User identifier (scoped to tenant)
- `key`: Setting key
- `value`: JSON value
- `created_at`: Timestamp
- `updated_at`: Timestamp

**Indexes**:
- Primary key: `(tenant_id, user_id, key)`
- Ensures fast lookups and tenant isolation

## 6. Sequences

### Settings Operation Flow

**ID**: [ ] `p1` `fdd-user-settings-seq-operation-v1`

<!-- fdd-id-content -->
1. Client sends authenticated request with tenant context
2. API layer validates authentication
3. Settings service resolves the owner key (token subject, or the deployment's
   `SettingsOwnerResolver`), asks the PDP whether the caller may act on it, and
   applies tenant scoping
4. Repository queries/updates database with security context
5. Response returned to client

**Components**: `fdd-user-settings-component-rest-v1`, `fdd-user-settings-component-service-v1`, `fdd-user-settings-component-repository-v1`
<!-- fdd-id-content -->

## 7. Data Model

**Settings Entity**:
- `user_id`: User identifier (scoped to tenant)
- `key`: Setting key
- `value`: JSON value
- `created_at`: Timestamp
- `updated_at`: Timestamp

**Indexes**:
- Primary key: `(tenant_id, user_id, key)`
- Ensures fast lookups and tenant isolation

## 8. Error Handling

- Unauthenticated request → 401 Unauthorized
- Missing tenant context → 403 Forbidden
- Setting not found → 404 Not Found
- Invalid JSON → 400 Bad Request
- Data too large → 413 Payload Too Large

## 9. Dependencies

- toolkit-db for database access
- toolkit-auth for authentication/authorization
- toolkit-security for tenant context

## Appendix

### Change Log

| Date | Version | Author | Changes |
|------|---------|--------|---------|
| 2026-02-09 | 0.1.0 | System | Initial DESIGN for cypilot validation |
| 2026-09-24 | 0.2.0 | Andrej Kuchma | Deployment-supplied `SettingsOwnerResolver` decides the user key |
