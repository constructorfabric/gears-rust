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
Fixed fields: `theme` and `language` up to `max_field_length` (default 100).
Named settings: each value up to `named_value_max_bytes` as serialized JSON
(default 4096, at most 65535 — the smallest backend column, MySQL `TEXT`), and
up to `named_settings_per_user` keys per user and tenant (default 256, at most
4096). The product of the two is capped at 16 MiB of values, because the list
returns every key in one unpaged response: either maximum is reachable, but not
both at once. Keys and JSON syntax add at most about 0.6 MiB on top (128 bytes
of key and some punctuation per entry, at 4096 keys). The gear refuses to start with a limit of 0, a bound above its maximum,
or a product above 16 MiB.
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
Fixed fields (`theme`, `language`):
- `GET /simple-user-settings/v1/settings` - Retrieve the fixed fields (defaults when unset)
- `POST /simple-user-settings/v1/settings` - Replace both fields
- `PATCH /simple-user-settings/v1/settings` - Update the fields given

Named settings (any key, JSON value):
- `GET /simple-user-settings/v1/named-settings` - Retrieve all named settings, ordered by key
- `GET /simple-user-settings/v1/named-settings/{key}` - Retrieve one; 404 if unset
- `PUT /simple-user-settings/v1/named-settings/{key}` - Create or replace one
- `DELETE /simple-user-settings/v1/named-settings/{key}` - Delete one; 204 whether or not it was set
- `DELETE /simple-user-settings/v1/named-settings` - Delete all of the caller's named settings in one call (offboarding, erasure); 204
<!-- fdd-id-content -->

### Settings Service

**ID**: [ ] `p1` `fdd-user-settings-component-service-v1`

<!-- fdd-id-content -->
Handles CRUD operations. Enforces tenant scoping. Validates request payloads.
<!-- fdd-id-content -->

### Database Repository

**ID**: [ ] `p1` `fdd-user-settings-component-repository-v1`

<!-- fdd-id-content -->
Persists settings to database. Uses toolkit-db for database access. Implements tenant isolation via security context.
<!-- fdd-id-content -->

## 5. Data Model

**`settings`** (fixed fields, one row per user and tenant):
- `tenant_id`, `user_id`: primary key `(tenant_id, user_id)`
- `theme`, `language`: nullable text

**`named_settings`** (one row per user, tenant and key):
- `tenant_id`, `user_id`, `key`: primary key `(tenant_id, user_id, key)`
- `value`: the JSON value, serialized to text on every backend

Both tables are scoped the same way (tenant column `tenant_id`, resource column
`user_id`) and authorized as the same PDP resource, `simple_user_settings.settings`:
reads need `get`, writes and deletes need `update`.

**Bounds on named settings** (configurable):
- key: 1–128 characters from `A–Z a–z 0–9 . _ - :`
- value: `named_value_max_bytes` as serialized JSON (default 4096)
- count: `named_settings_per_user` per user and tenant (default 256, at most
  4096, because the list returns every key in one unpaged response); applies to
  new keys only, so replacing a value at the bound still works
- request body: 4 × `named_value_max_bytes` + 1 KiB on the named routes, so a
  value at its bound fits even pretty-printed, and a larger body is refused
  before it is parsed

## 6. Sequences

### Settings Operation Flow

**ID**: [ ] `p1` `fdd-user-settings-seq-operation-v1`

<!-- fdd-id-content -->
1. Client sends authenticated request with tenant context
2. API layer validates authentication and authorization
3. Settings service applies tenant scoping
4. Repository queries/updates database with security context
5. Response returned to client

**Components**: `fdd-user-settings-component-rest-v1`, `fdd-user-settings-component-service-v1`, `fdd-user-settings-component-repository-v1`
<!-- fdd-id-content -->

### Named settings lifecycle

- **Delete is permanent.** `DELETE` removes the row; there is no tombstone,
  history or undo. Deleting a key that is not set is a no-op that also answers
  `204`.
- **`PUT` answers `200` either way.** Creating a key and replacing it both return
  `200` with the stored setting. The operation is an idempotent upsert, and a
  retried create should see the same answer as the first attempt, not `201` and
  then `200`; nor should a client need a second success code to handle. A client
  that needs to know whether the key existed can `GET` it first.
- **Retries are safe.** `PUT` is an upsert on `(tenant_id, user_id, key)` and
  `DELETE` is idempotent, so a client that got no answer (a timeout, a dropped
  connection) can repeat the same request. The gear itself retries only a
  transaction that lost a serialization conflict (below). The pool's
  `acquire_timeout` bounds only the wait for a connection. Running a statement
  or a transaction is bounded only if toolkit-db's `params` set
  `statement_timeout` / `transaction_timeout`, which have no default, so a
  deployment that wants a deadline on a stalled query must set them. A failure
  surfaces as `500`.
- **Count bound under concurrency.** Whether a key is new, the caller's key count
  and the write run in one `SERIALIZABLE` transaction, retried when it loses a
  conflict (toolkit-db's `transaction_with_retry`). A new key over the bound is
  refused before anything is written, so there is no compensating delete that
  could remove another request's write, and a concurrent delete cannot turn a
  replacement into an unchecked insert. Racing writes at the bound are ordered:
  one is kept and the other is refused, not both kept. If contention outlasts
  the retries, the request fails with `500` and is safe to repeat.
- **A corrupt row.** A row whose stored value no longer parses (only possible
  through an out-of-band edit or a bug) is left out of the list and logged at
  error level with its key, while a direct read of that key reports it as an
  internal error. It still occupies one slot of `named_settings_per_user`. That
  is deliberate: the row is still stored, and the bound caps what is stored.
  Deleting the key (by name, as the log gives it) frees the slot, since `DELETE`
  does not read the value.

### Migration and rollback

- `002_named_settings` only adds the `named_settings` table; `settings` is
  untouched, so the fixed-field endpoints keep working on either side of it.
- The gear's migrations run in the database phase at startup, before its REST
  routes are registered, so a node never serves `/named-settings` without the
  table. During a rolling upgrade, nodes still on the previous version do not
  have the routes at all; clients should treat `404` on `/named-settings` from an
  old node as "not available yet".
- Rolling back with `down()` drops `named_settings` and **deletes every stored
  named setting**. To downgrade the gear while keeping the data, leave the table
  in place: the previous version ignores it.

## 7. Error Handling

- Unauthenticated request → 401 Unauthorized
- Out of the caller's scope → 404 Not Found (masked, so existence is not disclosed)
- Named setting not set → 404 Not Found
- Malformed key or oversized value → 400 Bad Request with a field violation on
  `key` or `value`
- A request body past 4 × `named_value_max_bytes` + 1 KiB → 413 Payload Too
  Large, before the body is parsed
- A new named key past `named_settings_per_user` → 429 Too Many Requests
  (`resource_exhausted`, quota code `NAMED_SETTINGS_PER_USER`): the request is
  valid once the caller deletes a key
- JSON nested deeper than 128 levels → 400 Bad Request (refused while the body is
  parsed)

## 8. Dependencies

- toolkit-db for database access
- toolkit-auth for authentication/authorization
- toolkit-security for tenant context

## Appendix

### Change Log

| Date | Version | Author | Changes |
|------|---------|--------|---------|
| 2026-02-09 | 0.1.0 | System | Initial DESIGN for cypilot validation |
| 2026-09-24 | 0.2.0 | Andrej Kuchma | Named settings; data model and error handling brought in line with the code |
