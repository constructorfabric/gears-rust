<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-08-10 by Constructor Tech -->

# Feature: Category Management

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-category-management`

- [ ] `p1` - `cpt-cf-settings-service-feature-category-management`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create Category](#create-category)
  - [Update Category](#update-category)
  - [Delete Category](#delete-category)
  - [Get Category](#get-category)
  - [List Categories](#list-categories)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Category Key Validation](#category-key-validation)
  - [No-Orphan Deletion Guard](#no-orphan-deletion-guard)
  - [Category Visibility and Domain Filter](#category-visibility-and-domain-filter)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Category Entity and Schema](#category-entity-and-schema)
  - [Category CRUD Operations](#category-crud-operations)
  - [No-Orphan Deletion Rule](#no-orphan-deletion-rule)
  - [Key Format Enforcement](#key-format-enforcement)
  - [Authorization on Category Operations](#authorization-on-category-operations)
  - [Optimistic Concurrency on Mutations](#optimistic-concurrency-on-mutations)
  - [Category Mutation Audit](#category-mutation-audit)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Category Management provides the flat taxonomy every setting declaration is filed under: create, get, list, update, and delete over a `Category` entity with globally unique `key` and `name`, and a no-orphan deletion rule that refuses to remove a category while any declaration still references it.

### 1.2 Purpose

Categories are the first domain entity in the Settings Service and the only one with no upstream domain dependency, which is why they come immediately after the gear foundation. A setting declaration carries a non-null `category_id`, so categories must exist before any declaration can.

The category `key` is load-bearing well beyond grouping. It becomes the `<category>` segment of an admin-authored setting's instance id, so it is validated against the reserved path separator at create time rather than treated as free text. That coupling has a consequence worth stating up front: renaming or moving a category re-keys every setting inside it, and the stale key then resolves as not-found with no alias and no key history. The `key` is therefore immutable after creation, and only the display `name` may change.

The no-orphan rule protects the invariant that no declaration is ever left pointing at a missing category. It is enforced twice deliberately: an explicit pre-check that returns a meaningful conflict, and the declaration-to-category foreign key `ON DELETE RESTRICT`, which is the authoritative guard and holds even when the only referencing declaration is `retired`.

**Requirements**: `cpt-cf-settings-service-fr-settings-category-model`

**Principles**: `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Creates, updates, and deletes categories; category governance is platform-level, not tenant-level |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads and lists categories, subject to domain filtering and the visibility gate |
| `cpt-cf-settings-service-actor-authz-resolver` | Supplies the authorization decision and the `AccessScope` constraints applied to reads |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) — §4.1 (Entity `Category`), §4.2 (Component: Category Management), §4.3 (REST API — Categories, Error Response Format), §4.7 (Table `categories`), §4.8 (Security and Authorization)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.2
- **Dependencies**: entry 2.1 gear foundation, which supplies the persistence adapter, the RFC-9457 Problem mapping, the shared `If-Match` precondition helper returning `428` and `412`, the `PolicyEnforcer` PEP and `AccessScope` derivation, and the Audit Emitter. This feature consumes those rather than restating them.
- **Not applicable**: No PRD use case maps directly to category administration; the four defined use cases concern setting configuration, value resolution, staging, and audit review. Feature and licence entitlement gating is not part of this wave and lands with the licensing feature, so the entitlement consultation named in the DESIGN component's dependency list is wired through the foundation's authorization path here without its own fail-closed licence policy. Frontend presentation is owned by a future frontend DESIGN. Performance targets are set at the system level in the PRD NFR section.

## 2. Actor Flows (CDSL)

### Create Category

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-category-management-create`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Category created with unique `key` and `name`, returned with its assigned identifier, timestamps, and ETag

**Error Scenarios**:
- Actor not authorized for `create` on the category resource type
- Authorization decision unobtainable, denied fail-closed
- `key` contains the reserved path separator or violates its length bound
- `name` or `description` violates its length bound
- Duplicate `key` or duplicate `name`

**Steps**:
1. [ ] - `p1` - Actor sends POST /v1/categories with `key`, `name`, optional `description`, optional `domain_affinity`, `sort_order`, optional `icon` - `inst-cat-create-1`
2. [ ] - `p1` - Authorize `create` on `gts.cf.toolkit.settings.category.v1~` through the `PolicyEnforcer` PEP - `inst-cat-create-2`
3. [ ] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-cat-create-3`
4. [ ] - `p1` - Invoke category key validation on the supplied `key` - `inst-cat-create-4`
5. [ ] - `p1` - **IF** key validation fails → **RETURN** `422` with a field-level error naming `key` - `inst-cat-create-5`
6. [ ] - `p1` - Validate `name` within 1..256 and `description` within 0..4096 - `inst-cat-create-6`
7. [ ] - `p1` - **IF** field validation fails → **RETURN** `422` with field-level errors - `inst-cat-create-7`
8. [ ] - `p1` - Generate the category identifier and set `created_at` and `updated_at` to the current UTC instant - `inst-cat-create-8`
9. [ ] - `p1` - DB: INSERT INTO categories (id, key, name, description, domain_affinity, sort_order, icon, created_at, updated_at) - `inst-cat-create-9`
10. [ ] - `p1` - **IF** unique violation on `uq_category_key` or `uq_category_name` → **RETURN** `409` naming the conflicting field - `inst-cat-create-10`
11. [ ] - `p1` - Emit a category-created audit record through the Audit Emitter - `inst-cat-create-11`
12. [ ] - `p1` - **RETURN** `201` with the created Category and its ETag - `inst-cat-create-12`

### Update Category

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-category-management-update`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Category metadata partially updated and returned with a refreshed ETag

**Error Scenarios**:
- Actor not authorized for `update`
- Category does not exist
- `If-Match` absent, or present but stale
- Attempt to modify the immutable `key`
- Updated `name` collides with an existing category

**Steps**:
1. [ ] - `p1` - Actor sends PATCH /v1/categories/{id} with `If-Match` and any of `name`, `description`, `domain_affinity`, `sort_order`, `icon` - `inst-cat-update-1`
2. [ ] - `p1` - Authorize `update` on `gts.cf.toolkit.settings.category.v1~` through the `PolicyEnforcer` PEP - `inst-cat-update-2`
3. [ ] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-cat-update-3`
4. [ ] - `p1` - **IF** the request body carries `key` → **RETURN** `422`, because `key` is immutable once settings are keyed through it - `inst-cat-update-4`
5. [ ] - `p1` - DB: SELECT the category row WHERE id = {id} - `inst-cat-update-5`
6. [ ] - `p1` - **IF** category not found → **RETURN** `404` - `inst-cat-update-6`
7. [ ] - `p1` - Evaluate the `If-Match` precondition against the current representation using the shared precondition helper - `inst-cat-update-7`
8. [ ] - `p1` - **IF** `If-Match` is absent → **RETURN** `428` - `inst-cat-update-8`
9. [ ] - `p1` - **IF** `If-Match` is stale → **RETURN** `412` - `inst-cat-update-9`
10. [ ] - `p1` - Validate the supplied updatable fields against their length bounds - `inst-cat-update-10`
11. [ ] - `p1` - **IF** field validation fails → **RETURN** `422` with field-level errors - `inst-cat-update-11`
12. [ ] - `p1` - DB: UPDATE categories SET {supplied fields}, updated_at = now() WHERE id = {id} - `inst-cat-update-12`
13. [ ] - `p1` - **IF** unique violation on `uq_category_name` → **RETURN** `409` - `inst-cat-update-13`
14. [ ] - `p1` - Emit a category-updated audit record carrying the changed field set with pre-image and post-image - `inst-cat-update-14`
15. [ ] - `p1` - **RETURN** `200` with the updated Category and its refreshed ETag - `inst-cat-update-15`

### Delete Category

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-category-management-delete`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Empty category hard-deleted

**Error Scenarios**:
- Actor not authorized for `delete`
- Category does not exist
- `If-Match` absent, or present but stale
- Category still contains one or more declarations, active or retired

**Steps**:
1. [ ] - `p1` - Actor sends DELETE /v1/categories/{id} with `If-Match` - `inst-cat-delete-1`
2. [ ] - `p1` - Authorize `delete` on `gts.cf.toolkit.settings.category.v1~` through the `PolicyEnforcer` PEP - `inst-cat-delete-2`
3. [ ] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-cat-delete-3`
4. [ ] - `p1` - DB: SELECT the category row WHERE id = {id} - `inst-cat-delete-4`
5. [ ] - `p1` - **IF** category not found → **RETURN** `404` - `inst-cat-delete-5`
6. [ ] - `p1` - Evaluate the `If-Match` precondition using the shared precondition helper - `inst-cat-delete-6`
7. [ ] - `p1` - **IF** `If-Match` is absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-cat-delete-7`
8. [ ] - `p1` - Invoke the no-orphan deletion guard for this category - `inst-cat-delete-8`
9. [ ] - `p1` - **IF** the guard reports referencing declarations → **RETURN** `409 CategoryNotEmpty` - `inst-cat-delete-9`
10. [ ] - `p1` - DB: DELETE FROM categories WHERE id = {id} - `inst-cat-delete-10`
11. [ ] - `p1` - **IF** the declaration foreign key `ON DELETE RESTRICT` rejects the delete → **RETURN** `409 CategoryNotEmpty`, covering a declaration inserted between the guard and the delete - `inst-cat-delete-11`
12. [ ] - `p1` - Emit a category-deleted audit record carrying the pre-image - `inst-cat-delete-12`
13. [ ] - `p1` - **RETURN** `204` - `inst-cat-delete-13`

### Get Category

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-category-management-get`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- Category returned with its ETag when visible to the caller

**Error Scenarios**:
- Category does not exist, or exists but falls outside the caller's domain or visibility scope

**Steps**:
1. [ ] - `p1` - Actor sends GET /v1/categories/{id} - `inst-cat-get-1`
2. [ ] - `p1` - Authorize `read` on `gts.cf.toolkit.settings.category.v1~` and obtain the `AccessScope` constraints - `inst-cat-get-2`
3. [ ] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-cat-get-3`
4. [ ] - `p1` - DB: SELECT the category row WHERE id = {id} - `inst-cat-get-4`
5. [ ] - `p1` - **IF** category not found → **RETURN** `404` - `inst-cat-get-5`
6. [ ] - `p1` - Apply the domain and visibility filter to the loaded row - `inst-cat-get-6`
7. [ ] - `p1` - **IF** the category is filtered out → **RETURN** `404` rather than `403`, so a hidden category's existence is not disclosed - `inst-cat-get-7`
8. [ ] - `p1` - **RETURN** `200` with the Category and its ETag - `inst-cat-get-8`

### List Categories

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-category-management-list`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- Paginated, domain-filtered, visibility-gated page of categories returned in a stable order

**Error Scenarios**:
- Unsupported OData filter or ordering expression
- Malformed or expired pagination cursor

**Steps**:
1. [ ] - `p1` - Actor sends GET /v1/categories with optional OData `$filter`, `$orderby`, `$select`, and a pagination cursor - `inst-cat-list-1`
2. [ ] - `p1` - Authorize `read` on `gts.cf.toolkit.settings.category.v1~` and obtain the `AccessScope` constraints - `inst-cat-list-2`
3. [ ] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-cat-list-3`
4. [ ] - `p1` - Parse the OData expressions against the category field mapping - `inst-cat-list-4`
5. [ ] - `p1` - **IF** an expression references an unmapped field or an unsupported operator → **RETURN** `422` - `inst-cat-list-5`
6. [ ] - `p1` - Derive the domain and visibility predicate from the `AccessScope` constraints - `inst-cat-list-6`
7. [ ] - `p1` - DB: SELECT categories with the combined predicate applied in the query, ordered by `sort_order` then `name` so the cursor is deterministic - `inst-cat-list-7`
8. [ ] - `p1` - **IF** the supplied cursor is malformed or no longer decodable → **RETURN** `422` - `inst-cat-list-8`
9. [ ] - `p1` - **RETURN** `200` with the page and a next-page cursor when further rows remain - `inst-cat-list-9`

## 3. Processes / Business Logic (CDSL)

### Category Key Validation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-category-management-key-validation`

**Input**: Candidate category `key` string

**Output**: Accepted key, or a validation problem naming the violated rule

**Steps**:
1. [ ] - `p1` - Take the key verbatim without trimming or case-folding, so a stored key and a supplied key compare identically - `inst-cat-keyval-1`
2. [ ] - `p1` - **IF** length falls outside 1..128 → **RETURN** validation problem for the length bound - `inst-cat-keyval-2`
3. [ ] - `p1` - **IF** the key contains `/` → **RETURN** validation problem stating the separator is reserved because the key becomes the single category segment of an admin setting key - `inst-cat-keyval-3`
4. [ ] - `p1` - **RETURN** the accepted key - `inst-cat-keyval-4`

### No-Orphan Deletion Guard

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-category-management-no-orphan-guard`

**Input**: Category identifier

**Output**: Empty verdict, or a non-empty verdict carrying the referencing declaration count

**Steps**:
1. [ ] - `p1` - DB: SELECT the count of rows FROM setting_declarations WHERE category_id = {id} - `inst-cat-orphan-1`
2. [ ] - `p1` - Count declarations of every `status`, because a retired declaration still occupies its category and its values are retained - `inst-cat-orphan-2`
3. [ ] - `p1` - **IF** the count is greater than zero → **RETURN** non-empty verdict carrying the count - `inst-cat-orphan-3`
4. [ ] - `p1` - **RETURN** empty verdict, treating it as advisory only: the foreign key `ON DELETE RESTRICT` remains the authoritative guard at delete time - `inst-cat-orphan-4`

### Category Visibility and Domain Filter

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-category-management-visibility-filter`

**Input**: Caller `AccessScope` constraints, and a loaded category row or a pending query

**Output**: Filtered row, or an augmented query predicate

**Steps**:
1. [ ] - `p1` - Read the administrative-domain constraints carried on the caller's `AccessScope` - `inst-cat-visfilter-1`
2. [ ] - `p1` - **IF** the scope carries no domain restriction → **RETURN** the input unchanged - `inst-cat-visfilter-2`
3. [ ] - `p1` - Build a predicate matching categories whose `domain_affinity` is null or falls within the permitted domain set, so an undomained category stays universally visible - `inst-cat-visfilter-3`
4. [ ] - `p1` - Apply the predicate inside the query rather than as a post-filter, so pagination counts and cursors reflect only visible rows - `inst-cat-visfilter-4`
5. [ ] - `p1` - **RETURN** the filtered row or the augmented query predicate - `inst-cat-visfilter-5`

## 4. States (CDSL)

Not applicable. The `Category` entity carries no lifecycle status column and no state machine: a category exists from insert until hard delete, and deletion is permitted only from the empty condition enforced by `cpt-cf-settings-service-algo-category-management-no-orphan-guard`. Lifecycle state enters the service with `SettingDeclaration` in entry 2.3, which carries `active` and `retired`.

## 5. Definitions of Done

### Category Entity and Schema

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-entity-schema`

The system **MUST** persist categories in a `categories` table with a UUID primary key, `key` unique via `uq_category_key`, `name` unique via `uq_category_name`, nullable `description`, `domain_affinity`, and `icon`, non-null `sort_order` defaulting to `0`, and non-null `created_at` and `updated_at`, together with the `idx_categories_name_trgm` GIN trigram index on `name` that later search builds on.

**Implements**:
- `cpt-cf-settings-service-flow-category-management-create`

**Touches**:
- DB Table: `categories`
- Entities: `Category`

### Category CRUD Operations

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-crud`

The system **MUST** expose create, get, list, update, and delete over categories. Partial update **MUST** be restricted to `name`, `description`, `domain_affinity`, `sort_order`, and `icon`, and `key` **MUST** be rejected as immutable, because settings are keyed through the category slug and an in-place change would silently re-key every setting in the category.

**Implements**:
- `cpt-cf-settings-service-flow-category-management-create`
- `cpt-cf-settings-service-flow-category-management-update`
- `cpt-cf-settings-service-flow-category-management-get`
- `cpt-cf-settings-service-flow-category-management-list`
- `cpt-cf-settings-service-flow-category-management-delete`

**Touches**:
- API: `POST /v1/categories`
- API: `GET /v1/categories`
- API: `GET /v1/categories/{id}`
- API: `PATCH /v1/categories/{id}`
- API: `DELETE /v1/categories/{id}`
- Entities: `Category`

### No-Orphan Deletion Rule

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-no-orphan`

The system **MUST** refuse to delete a category while any setting declaration references it, returning `409 CategoryNotEmpty`, and **MUST** apply that refusal regardless of the referencing declaration's `status`, including `retired`. The declaration-to-category foreign key **MUST** be declared `ON DELETE RESTRICT` so the database enforces the rule independently, and the handler **MUST** translate that database rejection into the same `409` so a declaration inserted between the guard and the delete cannot produce a `500`.

**Implements**:
- `cpt-cf-settings-service-flow-category-management-delete`
- `cpt-cf-settings-service-algo-category-management-no-orphan-guard`

**Touches**:
- API: `DELETE /v1/categories/{id}`
- DB Table: `categories`, `setting_declarations`
- Entities: `Category`

### Key Format Enforcement

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-key-format`

The system **MUST** reject a category `key` that is empty, exceeds 128 characters, or contains `/`, and **MUST** store it verbatim without trimming or case-folding so a stored key and a supplied key compare identically.

**Implements**:
- `cpt-cf-settings-service-algo-category-management-key-validation`

**Touches**:
- API: `POST /v1/categories`
- Entities: `Category`

### Authorization on Category Operations

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-authorization`

The system **MUST** authorize every category operation as per-resource-type CRUD on `gts.cf.toolkit.settings.category.v1~` through the `PolicyEnforcer` PEP, **MUST** apply the caller's `AccessScope` domain constraints inside the query rather than as a post-filter, and **MUST** deny when a decision cannot be obtained. A category filtered out by the visibility gate **MUST** be reported as absent rather than as forbidden.

**Implements**:
- `cpt-cf-settings-service-algo-category-management-visibility-filter`
- `cpt-cf-settings-service-flow-category-management-get`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /v1/categories`
- API: `GET /v1/categories/{id}`
- Entities: `Category`

### Optimistic Concurrency on Mutations

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-concurrency`

The system **MUST** require `If-Match` on `PATCH` and `DELETE`, returning `428` when the header is absent and `412` when it is stale, and **MUST** return a refreshed ETag on every successful read and update.

**Implements**:
- `cpt-cf-settings-service-flow-category-management-update`
- `cpt-cf-settings-service-flow-category-management-delete`

**Constraints**: `cpt-cf-settings-service-constraint-optimistic-concurrency`

**Touches**:
- API: `PATCH /v1/categories/{id}`
- API: `DELETE /v1/categories/{id}`
- Entities: `Category`

### Category Mutation Audit

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-category-management-audit`

The system **MUST** emit an audit record through the Audit Emitter for every successful category create, update, and delete. The update record **MUST** identify the changed field set with pre-image and post-image, and the delete record **MUST** carry the pre-image.

**Implements**:
- `cpt-cf-settings-service-flow-category-management-create`
- `cpt-cf-settings-service-flow-category-management-update`
- `cpt-cf-settings-service-flow-category-management-delete`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- API: `POST /v1/categories`
- API: `PATCH /v1/categories/{id}`
- API: `DELETE /v1/categories/{id}`
- Entities: `Category`

## 6. Acceptance Criteria

- [ ] Creating a category with unique `key` and `name` returns `201` with an identifier, populated timestamps, and an ETag
- [ ] Creating a category whose `key` duplicates an existing category returns `409` naming `key` as the conflicting field
- [ ] Creating a category whose `name` duplicates an existing category returns `409` naming `name` as the conflicting field
- [ ] Creating a category whose `key` contains `/` returns `422` with a field-level error and inserts no row
- [ ] Creating a category whose `key` is empty or exceeds 128 characters returns `422`
- [ ] A `key` supplied with surrounding whitespace or mixed case is stored verbatim and matches only an identical string
- [ ] A `PATCH` carrying `key` returns `422` and the stored `key` is unchanged
- [ ] A `PATCH` without `If-Match` returns `428` and modifies no row
- [ ] A `PATCH` with a stale `If-Match` returns `412` and modifies no row
- [ ] A `PATCH` with a current `If-Match` returns `200` and an ETag different from the one the request carried
- [ ] A `DELETE` without `If-Match` returns `428`, and with a stale `If-Match` returns `412`
- [ ] Deleting an empty category returns `204` and the row is gone
- [ ] Deleting a category holding an `active` declaration returns `409 CategoryNotEmpty` and the row remains
- [ ] Deleting a category whose only declaration is `retired` returns `409 CategoryNotEmpty` and the row remains
- [ ] A declaration inserted between the guard check and the delete produces `409 CategoryNotEmpty` rather than `500`
- [ ] A database-level delete of a category holding any declaration is rejected by the foreign key `ON DELETE RESTRICT`, independently of the application pre-check
- [ ] Getting a category outside the caller's permitted domain returns `404` rather than `403`
- [ ] A category with null `domain_affinity` is visible to a caller whose `AccessScope` restricts domains
- [ ] Listing returns categories ordered by `sort_order` then `name`, and that order is reproduced across cursor pages
- [ ] Listing applies the visibility predicate inside the query, so a page is never short by the number of rows filtered out afterwards
- [ ] Listing with an OData filter on an unmapped field or an unsupported operator returns `422`
- [ ] Listing with a malformed pagination cursor returns `422`
- [ ] Every error response is `application/problem+json` carrying `type`, `title`, `status`, and `trace_id`, and every `422` carries a field-level `errors` array
- [ ] A category operation whose authorization decision cannot be obtained is denied rather than allowed
- [ ] Every successful create, update, and delete produces exactly one audit record; the update record names the changed fields with pre-image and post-image, and the delete record carries the pre-image
