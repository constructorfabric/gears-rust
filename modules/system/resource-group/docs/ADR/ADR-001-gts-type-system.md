---
status: accepted
date: 2026-03-16
decision-makers: Constructor Tech
---

# ADR-001: GTS Type System for Resource Group


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [GTS with traits pattern](#gts-with-traits-pattern)
  - [Simple string type codes](#simple-string-type-codes)
  - [Enum-based type system](#enum-based-type-system)
- [More Information](#more-information)
  - [Key Rules](#key-rules)
- [Type Map](#type-map)
- [Schemas](#schemas)
  - [RG Type Contract — `gts.x.core.rg.type.v1~`](#rg-type-contract--gtsxcorergtypev1)
  - [Tenant — `gts.y.core.tn.tenant.v1~`](#tenant--gtsycoretntenantv1)
  - [Department — `gts.w.core.org.department.v1~`](#department--gtswcoreorgdepartmentv1)
  - [Branch — `gts.x.core.rg.branch.v1~`](#branch--gtsxcorergbranchv1)
  - [User — `gts.z.core.idp.user.v1~`](#user--gtszcoreidpuserv1)
  - [Course — `gts.z.core.lms.course.v1~`](#course--gtszcorelmscoursev1)
- [Chained RG Type Schemas](#chained-rg-type-schemas)
  - [Tenant as RG Type — `gts.x.core.rg.type.v1~y.core.tn.tenant.v1~`](#tenant-as-rg-type--gtsxcorergtypev1ycoretntenantv1)
  - [Department as RG Type — `gts.x.core.rg.type.v1~w.core.org.department.v1~`](#department-as-rg-type--gtsxcorergtypev1wcoreorgdepartmentv1)
  - [Branch as RG Type — `gts.x.core.rg.type.v1~x.core.rg.branch.v1~`](#branch-as-rg-type--gtsxcorergtypev1xcorergbranchv1)
- [Instance Examples](#instance-examples)
  - [Tenant T1 (root)](#tenant-t1-root)
  - [Department D2 (under T1)](#department-d2-under-t1)
  - [Branch B3 (under D2)](#branch-b3-under-d2)
  - [Nested Tenant T7 (barrier, under T1)](#nested-tenant-t7-barrier-under-t1)
  - [Root Tenant T9 (custom domain)](#root-tenant-t9-custom-domain)
  - [Department D8 (under T7)](#department-d8-under-t7)
- [Example Hierarchy](#example-hierarchy)
- [DB Schema Summary](#db-schema-summary)
- [Implementation Findings](#implementation-findings)
  - [Finding 1: `x-gts-traits` mandatory on base contract](#finding-1-x-gts-traits-mandatory-on-base-contract)
  - [Finding 2: GTS segment format — strict 4-token rule](#finding-2-gts-segment-format--strict-4-token-rule)
  - [Finding 3: Entity-specific field validation at GTS level via metadata object](#finding-3-entity-specific-field-validation-at-gts-level-via-metadata-object)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-resource-group-adr-p1-gts-type-system`

## Context and Problem Statement

The Resource Group (RG) module needs a type identification standard for its hierarchy and membership model. The core problem: how to identify and compose RG types across vendors while maintaining DB-enforced referential integrity and clear separation of type-level metadata (topology rules) from instance-level data (group fields)?

## Decision Drivers

* Cross-vendor composability — types defined by different vendors (tenant, department, user) must compose into a single hierarchy
* DB-enforced referential integrity — type relationships (allowed parents, allowed memberships) must be enforced at the database level, not just application code
* Compact storage — the type system must scale to millions of groups without storage overhead from long string type identifiers in every row
* Separation of concerns — type-level metadata (topology rules like `can_be_root`, `allowed_parents`) must be distinct from instance-level data (group fields like `name`, `barrier`)
* Alignment with platform type conventions — RG should use the same type system as other CyberFabric modules

## Considered Options

* **Option 1: GTS (Global Type System) with traits pattern** — chained type identifiers, `x-gts-traits-schema`/`x-gts-traits` for topology rules, SMALLINT surrogate keys
* **Option 2: Simple string type codes** — free-form string codes (e.g. `"tenant"`, `"department"`) with no external type system
* **Option 3: Enum-based type system** — fixed Rust enum for type definitions compiled into the binary

## Decision Outcome

Chosen option: "GTS with traits pattern", because it is the only option that provides cross-vendor composability, DB-enforced referential integrity via junction tables with SMALLINT FKs, and clear separation of type-level metadata from instance-level data.

### Consequences

* RG types use chained GTS identifiers: `gts.x.core.rg.type.v1~<derived>~`
* SMALLINT surrogate keys and junction tables (`gts_type_allowed_parent`, `gts_type_allowed_membership`) enforce type relationships at DB level
* `x-gts-traits-schema` on the base contract defines type-level metadata shape; `x-gts-traits` on each chained type provides concrete values
* **Base contract MUST include `x-gts-traits` with defaults** alongside `x-gts-traits-schema` — the `gts` crate enforces this (see [Finding 1](#finding-1-x-gts-traits-mandatory-on-base-contract))
* `properties` defines instance-level fields; derived type fields stored in `metadata` JSONB column (nested under `metadata` object, not flattened to top level)
* **Chained types define `metadata` properties inline** (single `$ref` to base contract + inline `properties.metadata`). Entity schemas are registered for reference but NOT `$ref`'d from chained types. `metadata` sub-object uses `additionalProperties: false` for field isolation (see [Finding 3](#finding-3-entity-specific-field-validation-at-gts-level-via-metadata-object))
* Type registration order matters: base types first, then chained types that reference them
* SMALLINT IDs are DB-internal only — all external interfaces use GTS type path strings

### Confirmation

* Code review: verify all API responses use GTS type paths, never SMALLINT IDs
* Integration tests: verify junction table FK constraints reject invalid type references
* Schema validation: verify `x-gts-traits` values match `x-gts-traits-schema` definition
* **GTS type system unit tests** (33 tests, all passing): `modules/system/types-registry/types-registry/tests/rg_gts_type_system_tests.rs` — validates all schemas from this ADR against `gts` crate v0.8.4 in-memory, including base contract with metadata, chained types with inline metadata properties, valid/invalid instances, metadata field validation, traits, and deferred validation

## Pros and Cons of the Options

### GTS with traits pattern

* Good, because cross-vendor type composition is built into the identifier format
* Good, because SMALLINT surrogate keys minimize storage (2 bytes vs 50+ byte strings per FK)
* Good, because junction tables provide DB-level referential integrity for type relationships
* Good, because traits pattern cleanly separates topology rules from instance data
* Good, because aligns with platform GTS conventions used by other modules
* Bad, because adds complexity: type registration order dependency, chained identifier parsing
* Bad, because requires GTS type registry to be available before RG can create types

### Simple string type codes

* Good, because simple to implement — no external type system dependency
* Good, because no registration order dependency
* Bad, because no cross-vendor composability — type codes are local to RG
* Bad, because type relationships stored as TEXT arrays — no DB-level referential integrity
* Bad, because no separation of type-level metadata from instance-level data

### Enum-based type system

* Good, because compile-time type safety
* Good, because no runtime type resolution overhead
* Bad, because new types require code changes and redeployment
* Bad, because no cross-vendor extensibility — all types must be known at compile time
* Bad, because violates PRD requirement for dynamic type configuration via API

## More Information

### Key Rules

- Trailing `~` = type/schema. No trailing `~` = instance. ([GTS spec](https://github.com/GlobalTypeSystem/gts-spec))
- `x-gts-traits-schema` + `x-gts-traits` are schema-only keywords (never in instance documents)
- **If a schema defines `x-gts-traits-schema`, it MUST also provide `x-gts-traits`** with default/baseline values. The `gts` crate (v0.8.4) enforces this at validation time — see [Finding 1](#finding-1-x-gts-traits-mandatory-on-base-contract)
- Traits are immutable once set — descendants cannot override ancestor trait values (GTS OP#13)
- **Each GTS ID segment follows strict 4-token format**: `vendor.package.namespace.type.vMAJOR[.MINOR]` — no extra tokens — see [Finding 2](#finding-2-gts-segment-format--strict-4-token-rule)
- **Chained schema uses single `$ref` + inline `metadata` properties** — entity-specific fields nested under `metadata` object (`metadata` sub-object uses `additionalProperties: false`); base contract is open model (GTS OP#12 forbids `additionalProperties: false` on base when derived types override properties) — see [Finding 3](#finding-3-entity-specific-field-validation-at-gts-level-via-metadata-object)
- SMALLINT IDs are DB-internal only, never exposed in API

## Type Map

| GTS Type Path | Kind | Vendor |
|---|---|---|
| `gts.x.core.rg.type.v1~` | RG base contract | x (platform) |
| `gts.y.core.tn.tenant.v1~` | entity type | y (tenant service) |
| `gts.w.core.org.department.v1~` | entity type | w (org service) |
| `gts.x.core.rg.branch.v1~` | entity type | x (platform) |
| `gts.z.core.idp.user.v1~` | resource type | z (IDP) |
| `gts.z.core.lms.course.v1~` | resource type | z (LMS) |
| `gts.x.core.rg.type.v1~y.core.tn.tenant.v1~` | chained RG type | x + y |
| `gts.x.core.rg.type.v1~w.core.org.department.v1~` | chained RG type | x + w |
| `gts.x.core.rg.type.v1~x.core.rg.branch.v1~` | chained RG type | x + x |

## Schemas

### RG Type Contract — `gts.x.core.rg.type.v1~`

Base contract for all RG type definitions. Traits define topology rules; properties define instance fields.

```json
{
  "$id": "gts://gts.x.core.rg.type.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Resource Group Type",
  "type": "object",
  "x-gts-traits-schema": {
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "can_be_root": {
        "type": "boolean",
        "description": "Whether this type permits root placement (no parent_id).",
        "default": false
      },
      "allowed_parents": {
        "type": "array",
        "items": { "type": "string", "x-gts-ref": "gts.x.core.rg.type.v1~" },
        "description": "Well-known instances of allowed parent RG types.",
        "default": []
      },
      "allowed_memberships": {
        "type": "array",
        "items": { "type": "string", "x-gts-ref": "gts.*" },
        "description": "GTS type identifiers of resource types allowed as members.",
        "default": []
      }
    }
  },
  "x-gts-traits": {
    "can_be_root": false,
    "allowed_parents": [],
    "allowed_memberships": []
  },
  "required": ["id", "name"],
  "properties": {
    "id": {
      "type": "string",
      "format": "uuid",
      "description": "Group identifier."
    },
    "name": {
      "type": "string",
      "minLength": 1,
      "maxLength": 255,
      "description": "Display name."
    },
    "parent_id": {
      "type": ["string", "null"],
      "format": "uuid",
      "description": "Direct parent group (null for root groups)."
    },
    "tenant_id": {
      "type": "string",
      "format": "uuid",
      "readOnly": true,
      "description": "Computed. Derived from hierarchy — equals own id for tenant types, inherited from nearest tenant ancestor otherwise."
    },
    "depth": {
      "type": "integer",
      "readOnly": true,
      "description": "Computed. Absolute depth from root (0 = root) in list/get; relative to reference group in hierarchy queries."
    },
    "metadata": {
      "type": "object",
      "description": "Type-specific fields. Schema overridden by each chained RG type via allOf."
    }
  },
  "x-gts-constraints": {
    "placement-invariant": "can_be_root OR len(allowed_parents) >= 1"
  }
}
```

### Tenant — `gts.y.core.tn.tenant.v1~`

```json
{
  "$id": "gts://gts.y.core.tn.tenant.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Tenant",
  "type": "object",
  "required": ["id", "name"],
  "properties": {
    "id": { "type": "string", "format": "uuid" },
    "name": { "type": "string", "minLength": 1, "maxLength": 255 },
    "custom_domain": { "type": "string", "format": "hostname" },
    "barrier": { "type": "boolean", "default": false }
  }
}
```

### Department — `gts.w.core.org.department.v1~`

```json
{
  "$id": "gts://gts.w.core.org.department.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Department",
  "type": "object",
  "required": ["id"],
  "properties": {
    "id": { "type": "string", "format": "uuid" },
    "short_description": { "type": "string", "maxLength": 500 },
    "category": { "type": "string", "maxLength": 100 }
  }
}
```

### Branch — `gts.x.core.rg.branch.v1~`

```json
{
  "$id": "gts://gts.x.core.rg.branch.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Branch",
  "type": "object",
  "required": ["id", "name"],
  "properties": {
    "id": { "type": "string", "format": "uuid" },
    "name": { "type": "string", "minLength": 1, "maxLength": 255 },
    "location": { "type": "string" }
  }
}
```

### User — `gts.z.core.idp.user.v1~`

```json
{
  "$id": "gts://gts.z.core.idp.user.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "User",
  "type": "object",
  "required": ["id", "email", "display_name"],
  "properties": {
    "id": { "type": "string", "format": "uuid" },
    "email": { "type": "string", "format": "email" },
    "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
    "avatar_url": { "type": "string", "format": "uri" }
  }
}
```

### Course — `gts.z.core.lms.course.v1~`

```json
{
  "$id": "gts://gts.z.core.lms.course.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Course",
  "type": "object",
  "required": ["id", "title"],
  "properties": {
    "id": { "type": "string", "format": "uuid" },
    "title": { "type": "string", "minLength": 1, "maxLength": 255 }
  }
}
```

## Chained RG Type Schemas

When a type is registered as an RG type, it chains with the base contract via a **single `$ref`** and provides: (1) trait values via `x-gts-traits`, and (2) type-specific fields via inline `properties.metadata` override. Entity schemas (e.g. `gts.y.core.tn.tenant.v1~`) are registered in GTS for reference but are **NOT `$ref`'d** from chained types — the `metadata` properties are defined inline. The `metadata` sub-object uses `additionalProperties: false` to reject unknown fields (see [Finding 3](#finding-3-entity-specific-field-validation-at-gts-level-via-metadata-object)).

### Tenant as RG Type — `gts.x.core.rg.type.v1~y.core.tn.tenant.v1~`

```json
{
  "$id": "gts://gts.x.core.rg.type.v1~y.core.tn.tenant.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "allOf": [
    { "$ref": "gts://gts.x.core.rg.type.v1~" },
    {
      "properties": {
        "metadata": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "custom_domain": { "type": "string", "format": "hostname" },
            "barrier": { "type": "boolean", "default": false }
          }
        }
      },
      "x-gts-traits": {
        "can_be_root": true,
        "allowed_parents": ["gts.x.core.rg.type.v1~y.core.tn.tenant.v1~"],
        "allowed_memberships": ["gts.z.core.idp.user.v1~"]
      }
    }
  ]
}
```

- Root: yes
- Parents: self (tenant can nest under tenant)
- Members: users
- Instance `metadata` fields: `custom_domain` (hostname), `barrier` (boolean)

### Department as RG Type — `gts.x.core.rg.type.v1~w.core.org.department.v1~`

```json
{
  "$id": "gts://gts.x.core.rg.type.v1~w.core.org.department.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "allOf": [
    { "$ref": "gts://gts.x.core.rg.type.v1~" },
    {
      "properties": {
        "metadata": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "category": { "type": "string", "maxLength": 100 },
            "short_description": { "type": "string", "maxLength": 500 }
          }
        }
      },
      "x-gts-traits": {
        "can_be_root": false,
        "allowed_parents": ["gts.x.core.rg.type.v1~y.core.tn.tenant.v1~"],
        "allowed_memberships": ["gts.z.core.idp.user.v1~"]
      }
    }
  ]
}
```

- Root: no
- Parents: tenant only
- Members: users
- Instance `metadata` fields: `category` (maxLength: 100), `short_description` (maxLength: 500)

### Branch as RG Type — `gts.x.core.rg.type.v1~x.core.rg.branch.v1~`

```json
{
  "$id": "gts://gts.x.core.rg.type.v1~x.core.rg.branch.v1~",
  "$schema": "http://json-schema.org/draft-07/schema#",
  "allOf": [
    { "$ref": "gts://gts.x.core.rg.type.v1~" },
    {
      "properties": {
        "metadata": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "location": { "type": "string" }
          }
        }
      },
      "x-gts-traits": {
        "can_be_root": false,
        "allowed_parents": ["gts.x.core.rg.type.v1~w.core.org.department.v1~"],
        "allowed_memberships": ["gts.z.core.idp.user.v1~", "gts.z.core.lms.course.v1~"]
      }
    }
  ]
}
```

- Root: no
- Parents: department only
- Members: users and courses
- Instance `metadata` fields: `location` (string)

## Instance Examples

Instances are anonymous (UUID `id`, separate `type` field). Derived type fields are nested inside a `metadata` object; DB stores them in `metadata` JSONB.

### Tenant T1 (root)

```json
{
  "id": "11111111-1111-1111-1111-111111111111",
  "type": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~",
  "name": "T1",
  "parent_id": null,
  "tenant_id": "11111111-1111-1111-1111-111111111111",
  "depth": 0
}
```

### Department D2 (under T1)

```json
{
  "id": "22222222-2222-2222-2222-222222222222",
  "type": "gts.x.core.rg.type.v1~w.core.org.department.v1~",
  "name": "D2",
  "parent_id": "11111111-1111-1111-1111-111111111111",
  "tenant_id": "11111111-1111-1111-1111-111111111111",
  "depth": 1,
  "metadata": {
    "category": "finance",
    "short_description": "Mega Department"
  }
}
```

### Branch B3 (under D2)

```json
{
  "id": "33333333-3333-3333-3333-333333333333",
  "type": "gts.x.core.rg.type.v1~x.core.rg.branch.v1~",
  "name": "B3",
  "parent_id": "22222222-2222-2222-2222-222222222222",
  "tenant_id": "11111111-1111-1111-1111-111111111111",
  "depth": 2,
  "metadata": {
    "location": "Building A, Floor 3"
  }
}
```

### Nested Tenant T7 (barrier, under T1)

```json
{
  "id": "77777777-7777-7777-7777-777777777777",
  "type": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~",
  "name": "T7",
  "parent_id": "11111111-1111-1111-1111-111111111111",
  "tenant_id": "77777777-7777-7777-7777-777777777777",
  "depth": 1,
  "metadata": {
    "barrier": true
  }
}
```

### Root Tenant T9 (custom domain)

```json
{
  "id": "99999999-9999-9999-9999-999999999999",
  "type": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~",
  "name": "T9",
  "parent_id": null,
  "tenant_id": "99999999-9999-9999-9999-999999999999",
  "depth": 0,
  "metadata": {
    "custom_domain": "t9.example.com"
  }
}
```

### Department D8 (under T7)

```json
{
  "id": "88888888-8888-8888-8888-888888888888",
  "type": "gts.x.core.rg.type.v1~w.core.org.department.v1~",
  "name": "D8",
  "parent_id": "77777777-7777-7777-7777-777777777777",
  "tenant_id": "77777777-7777-7777-7777-777777777777",
  "depth": 2,
  "metadata": {
    "category": "hr"
  }
}
```

## Example Hierarchy

```text
tenant T1 (depth 0)
├── department D2 (depth 1) {metadata: {category: "finance", short_description: "Mega Department"}}
│   └── branch B3 (depth 2) {metadata: {location: "Building A, Floor 3"}}
│       └── [member] course R4
│   └── [member] user R5
├── [member] user R4
├── [member] user R6
└── tenant T7 (depth 1) {metadata: {barrier: true}}
    ├── department D8 (depth 2) {metadata: {category: "hr"}}
    │   └── [member] user R8
    └── [member] user R8
tenant T9 (depth 0) {metadata: {custom_domain: "t9.example.com"}}
└── [member] user R0
```

Notes:
- T7 has `metadata.barrier: true` — RG stores it in metadata JSONB. Tenant Resolver (`BarrierMode`) + AuthZ enforce it
- R4 appears twice: as **course** in B3 and as **user** in T1 (different `gts_type_id`)
- R8 appears twice: as **user** in D8 and T7 (same type, two group memberships)

## DB Schema Summary

| Table | Description |
|---|---|
| `gts_type` | GTS type definitions (SMALLINT id PK, schema_id UNIQUE, metadata_schema JSONB). `can_be_root` resolved at runtime from `x-gts-traits` in the registered GTS schema. |
| `gts_type_allowed_parent` | Junction: type_id → parent_type_id (both SMALLINT FK) |
| `gts_type_allowed_membership` | Junction: type_id → membership_type_id (both SMALLINT FK) |
| `resource_group` | Groups (UUID id, gts_type_id SMALLINT FK, name, metadata JSONB, parent_id UUID FK, tenant_id UUID) |
| `resource_group_closure` | Closure table (ancestor_id, descendant_id, depth) |
| `resource_group_membership` | Memberships (group_id UUID FK, gts_type_id SMALLINT FK, resource_id TEXT) |

Key design choices:
- Junction tables with SMALLINT FK (not TEXT arrays) — referential integrity enforced by DB
- `barrier` stored in `metadata` JSONB — RG treats it as metadata, TR + AuthZ enforce it
- `depth` computed from closure table (self-referencing row depth=0 for root)
- SMALLINT IDs never exposed in API — all external interfaces use GTS type path strings

## Implementation Findings

> Discovered during implementation validation with `gts` crate v0.8.4 via in-memory `TypesRegistryService` unit tests.
> Test file: `modules/system/types-registry/types-registry/tests/rg_gts_type_system_tests.rs` (33 tests, all passing).

### Finding 1: `x-gts-traits` mandatory on base contract

**Problem**: The original base contract schema (`gts.x.core.rg.type.v1~`) defined `x-gts-traits-schema` (the shape of traits) but did not include `x-gts-traits` (concrete trait values). The `gts` crate (v0.8.4) rejects this at `switch_to_ready()` validation:

```
Entity defines x-gts-traits-schema but no x-gts-traits values are provided
```

**Root cause**: GTS OP#13 requires that any schema declaring `x-gts-traits-schema` must also provide `x-gts-traits` — even if all values are defaults. The validator does not implicitly populate defaults from `x-gts-traits-schema`; they must be stated explicitly.

**Fix**: The base contract now includes explicit default trait values:

```json
"x-gts-traits": {
  "can_be_root": false,
  "allowed_parents": [],
  "allowed_memberships": []
}
```

These defaults are overridden by concrete values in each chained RG type (e.g. tenant sets `can_be_root: true`). The trait immutability rule (GTS OP#13) applies only to non-default values set by ancestors — chained types are free to override defaults with concrete values.

**Impact**: Base contract schema updated in this ADR. All consumers registering the RG base contract must include `x-gts-traits`.

---

### Finding 2: GTS segment format — strict 4-token rule

**Context**: Each segment of a GTS identifier (whether type or instance) must follow the canonical format:

```
vendor.package.namespace.type.vMAJOR[.MINOR]
  │       │        │       │      │      │
  │       │        │       │      │      └─ optional minor version (non-negative int)
  │       │        │       │      └──────── major version (non-negative int)
  │       │        │       └─────────────── type name [a-z_][a-z0-9_]*
  │       │        └─────────────────────── namespace (or _ placeholder)
  │       └──────────────────────────────── package name
  └──────────────────────────────────────── vendor name
```

Exactly **4 name tokens + version**. The `gts` crate (v0.8.4) rejects segments with fewer or more tokens:

```
Too many name tokens before version (got 5, expected 4).
Expected format: vendor.package.namespace.type.vMAJOR[.MINOR]
```

**How this applies to RG**: The 4-token rule is already satisfied by all GTS identifiers that RG uses. Specifically:

| Where in RG | Format | Example | Valid? |
|-------------|--------|---------|--------|
| `gts_type.schema_id` (DB) | Type path, ends with `~` | `gts.x.core.rg.type.v1~y.core.tn.tenant.v1~` | Yes — each segment is 4 tokens + version |
| `resource_group.id` (DB) | UUID | `11111111-1111-1111-1111-111111111111` | N/A — not a GTS ID |
| `resource_group.gts_type_id` (DB) | SMALLINT FK | `3` | N/A — internal surrogate |
| API response `type` field | Same as `gts_type.schema_id` | `gts.x.core.rg.type.v1~y.core.tn.tenant.v1~` | Yes |
| `resource_group_membership.resource_id` (DB) | Opaque TEXT | `user-uuid-here` | N/A — not a GTS ID |

**No contradictions with migration.sql**: The `gts_type_path` domain regex in `migration.sql` already enforces the 4-token rule per segment via:

```sql
'^gts\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)...'
--     ^^^^^^^^^^^^^^^^   ^^^^^^^^^^^^^^^^   ^^^^^^^^^^^^^^^^   ^^^^^^^^^^^^^^^^   ^^^^^^^^^^^^^^^^^^
--     vendor              package            namespace          type               version
```

This matches exactly 4 name tokens per segment — the DB constraint and the `gts` crate are consistent.

**Test artifact note**: The unit tests in `rg_gts_type_system_tests.rs` use well-known GTS instance IDs (e.g. `gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t1.v1`) to register test instances in the types-registry. The `x.core._.t1.v1` segment format (with `_` as namespace placeholder) is needed only for these test-level well-known instances. **RG itself never constructs such IDs** — resource groups are anonymous instances with UUID `id` and a `type` field pointing to a type path. The `_` placeholder format is documented here for completeness in case future code needs to construct well-known instances.

---

### Finding 3: Entity-specific field validation at GTS level via metadata object

**Problem**: Entity-specific fields (`category`, `barrier`, `custom_domain`, `location`) need to be validated, but they must be isolated from base contract fields to prevent name clashes and ensure clean separation. Two approaches were evaluated:

1. **Double `$ref`** — chained type references both base contract and entity schema. **Rejected**: works for validation, but couples chained types to pre-registered entity schemas and flattens fields to top level (clash risk).
2. **`additionalProperties: false` on base contract** — closes the model. **Rejected**: GTS OP#12 schema compatibility check rejects derived schemas that override `properties` in `allOf` when the base has `additionalProperties: false` (`"derived schema loosens additionalProperties from false in base"`).

**Solution: metadata object pattern**. Each chained type uses:
- A **single `$ref`** to the base contract (which is an open model)
- An inline `properties.metadata` override with `additionalProperties: false` on the **sub-object only**

```json
"allOf": [
  { "$ref": "gts://gts.x.core.rg.type.v1~" },
  {
    "properties": {
      "metadata": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "category": { "type": "string", "maxLength": 100 },
          "short_description": { "type": "string", "maxLength": 500 }
        }
      }
    },
    "x-gts-traits": { ... }
  }
]
```

**Why this works**:
- Base contract declares `metadata: {type: object}` as an open placeholder — no OP#12 conflict
- Chained type narrows `metadata` via `allOf` merge — `additionalProperties: false` applies only within the `metadata` sub-object
- Unknown metadata fields rejected (e.g. `metadata: {foo: "bar"}` → error)
- Entity-specific constraints enforced (e.g. `metadata.category` maxLength: 100 → validated)
- Top-level field isolation: `metadata.barrier` can never clash with a future base field `barrier`

**Test results** (33 tests, all passing):

| Test case | Expected | Result |
|-----------|----------|--------|
| Department `metadata.category` 101 chars | rejected | rejected |
| Department `metadata.short_description` 501 chars | rejected | rejected |
| Tenant `metadata.barrier: "yes"` (string) | rejected | rejected |
| Tenant `metadata: {foo: "bar"}` (unknown field) | rejected | rejected |
| Tenant `metadata.barrier: true` | accepted | accepted |
| Tenant `metadata.custom_domain: "t9.example.com"` | accepted | accepted |
| Branch `metadata.location: "..."` | accepted | accepted |
| Top-level `barrier: true` (no metadata wrapper) | accepted by GTS* | accepted* |
| Full hierarchy batch (T1, D2, B3, T7, D8, T9) | accepted | accepted |

*Top-level extra fields pass GTS validation (base is open model). The RG application layer enforces that callers use `metadata` — it strips/rejects unknown top-level fields on create/update.

**Validation strategy (two concerns)**:

| Concern | Validated by | How |
|---------|-------------|-----|
| GTS ID format | `gts` crate | Automatic on register |
| Base fields (`id`, `name`, `parent_id`, etc.) | `gts` crate via `$ref` to base contract | required, type, minLength, maxLength, format |
| Metadata fields (`category`, `barrier`, etc.) | `gts` crate via inline `metadata` schema in chained type | type, maxLength, format, additionalProperties:false |
| Topology traits | `gts` crate via `x-gts-traits` against `x-gts-traits-schema` | OP#13 immutability |
| Top-level field isolation | RG application layer | Strip/reject fields not in base contract properties on create/update |

**Impact**: DB columns renamed: `data_schema` → `metadata_schema`, `data` → `metadata`. Entity schemas are registered for reference but not `$ref`'d. The second GTS segment in chained type IDs (e.g. `y.core.tn.tenant.v1~`) serves as a naming convention and reference, not as a required dependency.

---

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-resource-group-fr-manage-types` — GTS type system defines how types are identified, composed, and validated
* `cpt-cf-resource-group-fr-validate-parent-type` — Junction tables enforce parent type compatibility at DB level
* `cpt-cf-resource-group-constraint-surrogate-ids-internal` — SMALLINT surrogate keys are DB-internal, GTS paths are external
* `cpt-cf-resource-group-constraint-db-agnostic` — GTS path validation at application layer for non-PostgreSQL backends
* `cpt-cf-resource-group-principle-dynamic-types` — GTS enables runtime-configurable type definitions
* `cpt-cf-resource-group-principle-barrier-as-data` — Barrier stored in `metadata` JSONB per GTS derived type schema

Additional references:
- Migration DDL: [migration.sql](../migration.sql)
- OpenAPI spec: [openapi.yaml](../openapi.yaml)
