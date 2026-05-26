# GTS Definitions Review & Authoring Guide

> Reference: [GTS Specification](https://github.com/GlobalTypeSystem/gts-spec)

## Table of Contents

- [GTS Definitions Review \& Authoring Guide](#gts-definitions-review--authoring-guide)
  - [Table of Contents](#table-of-contents)
  - [Goal](#goal)
  - [Review Entry Points](#review-entry-points)
  - [1. When to Introduce GTS Types](#1-when-to-introduce-gts-types)
  - [2. GTS Identifier Format](#2-gts-identifier-format)
    - [2.1 Base Type Identifier](#21-base-type-identifier)
      - [Cyber Ware examples](#cyber-ware-examples)
    - [2.2 Derived Type Identifier](#22-derived-type-identifier)
    - [2.3 Well-Known Instance Identifier](#23-well-known-instance-identifier)
    - [2.4 Anonymous Instance](#24-anonymous-instance)
    - [Key rules](#key-rules)
  - [3. Identifier Components](#3-identifier-components)
    - [3.1 Vendor](#31-vendor)
    - [3.2 Package](#32-package)
    - [3.3 Namespace](#33-namespace)
    - [3.4 Type Name](#34-type-name)
    - [3.5 Version](#35-version)
  - [4. Type (Schema) vs Instance — When to Use Each](#4-type-schema-vs-instance--when-to-use-each)
  - [5. Hybrid Storage Pattern — Base Fields + Extension Fields](#5-hybrid-storage-pattern--base-fields--extension-fields)
    - [5.1 GTS Type Storage — UUID, Not Strings](#51-gts-type-storage--uuid-not-strings)
    - [5.2 Database Schema Example (PostgreSQL)](#52-database-schema-example-postgresql)
    - [5.3 Events Table — Hybrid Storage](#53-events-table--hybrid-storage)
    - [5.4 Model Registry — GTS for Provider Types and Lifecycle](#54-model-registry--gts-for-provider-types-and-lifecycle)
    - [5.5 JSON Schema Definition (GTS Type Schema)](#55-json-schema-definition-gts-type-schema)
  - [6. GTS in Rust Code](#6-gts-in-rust-code)
    - [6.1 Defining a Base Type](#61-defining-a-base-type)
    - [6.2 Defining a Derived Type (Plugin Spec)](#62-defining-a-derived-type-plugin-spec)
    - [6.3 Registering a Plugin Instance](#63-registering-a-plugin-instance)
    - [6.4 Defining GTS Constants](#64-defining-gts-constants)
    - [6.5 Using GTS Types in Error Mapping (RFC 9457)](#65-using-gts-types-in-error-mapping-rfc-9457)
    - [6.6 GTS Well-Known Instances for Constants and Discriminator Values](#66-gts-well-known-instances-for-constants-and-discriminator-values)
      - [Why GTS instances instead of string constants](#why-gts-instances-instead-of-string-constants)
      - [Example: before and after](#example-before-and-after)
      - [When string constants are still appropriate](#when-string-constants-are-still-appropriate)
  - [6.7 Heterogeneous Dispatch — `GtsSchema for Value` and `try_narrow`](#67-heterogeneous-dispatch--gtsschema-for-value-and-try_narrow)
  - [7. Base Types — Design Guidelines](#7-base-types--design-guidelines)
  - [8. GTS-Based Security and Wildcard Access Control](#8-gts-based-security-and-wildcard-access-control)
    - [8.1 Why Naming Structure Matters for Security](#81-why-naming-structure-matters-for-security)
    - [8.2 Wildcard Patterns](#82-wildcard-patterns)
    - [8.3 Naming Rules That Enable Security](#83-naming-rules-that-enable-security)
    - [8.4 GTS-Based Access Control in Practice](#84-gts-based-access-control-in-practice)
  - [9. Derived Types — Design Guidelines](#9-derived-types--design-guidelines)
  - [10. GTS Traits](#10-gts-traits)
  - [11. Abstract and Final Types](#11-abstract-and-final-types)
    - [Abstract Types (`x-gts-abstract: true`)](#abstract-types-x-gts-abstract-true)
    - [Final Types (`x-gts-final: true`)](#final-types-x-gts-final-true)
  - [12. Cyber Ware GTS Conventions](#12-cyber-ware-gts-conventions)
  - [13. Reviewing GTS in PRD](#13-reviewing-gts-in-prd)
  - [14. Reviewing GTS in DESIGN](#14-reviewing-gts-in-design)
  - [15. Reviewing GTS in Rust Code PRs](#15-reviewing-gts-in-rust-code-prs)
  - [Quick Review Checklist](#quick-review-checklist)

## Goal

Use **GTS (Global Type System)** to define extensible, globally unique, and evolvable data structures — especially where runtime extension is required without database migrations, for example for plugins, API integrations, vendor-specific metadata, and cross-module contracts.

## Review Entry Points

Use this guide differently depending on what you are reviewing:

- **Reviewing a PRD**: start with [section 13](#13-reviewing-gts-in-prd). Check whether the requirement is describing an extension point, a closed enum, a registry-backed category, or a behavior-driving discriminator that should be modeled with GTS.
- **Reviewing a DESIGN**: start with [section 14](#14-reviewing-gts-in-design). Check whether the architecture clearly defines the base type, extension field, validation flow, storage model, registry usage, and authorization model.
- **Reviewing Rust code**: start with [section 15](#15-reviewing-gts-in-rust-code-prs). Check macro usage, generated schema shape, `type_id` naming, validation at boundaries, and whether behavior is schema-driven instead of hardcoded.
- **Doing a fast final pass**: use the [Quick Review Checklist](#quick-review-checklist) at the bottom.


GTS brings the following capabilities to Cyber Ware:

1. **Versioned schemas** — built-in major/minor versioning with automated compatibility checking, safe schema evolution, and version casting between compatible editions
2. **Human-readable origin and category** — vendor, package, namespace, and type encoded directly in the identifier; instantly comprehensible in logs, traces, and debugging
3. **Wildcard-based access control (ABAC)** — grant or restrict permissions using patterns like `gts.cf.core.events.type.v1~acme.*` instead of maintaining explicit resource lists, can be useful for cross-vendor data isolation
4. **Extensible (derived) types** — third-party vendors safely extend platform base types via inheritance chains while maintaining compatibility guarantees
5. **Hybrid storage pattern** — store base-type fields in indexed DB columns, vendor-specific extensions in JSONB — no schema migrations needed for new type variants
6. **Code-generated JSON Schema contracts** — GTS Type Schemas generated from Rust structs via `#[struct_to_gts_schema]`, keeping type definitions in code rather than hand-maintained JSON
7. **Globally unique identifiers across vendors** — no naming collisions; deterministic UUID v5 derivation from any GTS identifier for compact, fixed-size DB storage and external system interop
8. **Semantic trait metadata attached to type schemas** — cross-cutting properties (e.g., event routing topic, retention policy, audit requirements) declared as structured values in `x-gts-traits`, with their shape/defaults defined by `x-gts-traits-schema`, inherited and merged across the derivation chain
9. **GTS Registry** — GTS Type Schemas and well-known Instances indexed by GTS Identifier for discovery, validation, compatibility checking, and plugin resolution
10. **Types and well-known instances** — GTS distinguishes between type definitions (schemas that describe structure) and well-known instances (canonical, named objects of a given type); types end with `~`, instances do not — enabling a single naming system for both contracts and their predefined values. This is especially valuable for discriminator fields and former "const enum" style values: use well-known instances instead of raw strings when values need discoverability, descriptions, authorization semantics, or vendor extensibility.

Cyber Ware modules that offer extension points define **base GTS types** with a stable core schema. Derived types specialize the base by adding context-specific fields — typically metadata, payload, params, or properties. At the database level, the module stores base-type fields in dedicated columns (indexed, queryable) and the extension field as a `JSONB` column. This pattern allows new data-type variants to appear at runtime or compile time **without changing the database schema or existing APIs**.

Examples of extendable types: event schemas, resource types, plugin specifications, user settings categories, permission definitions, licenseable features, model lifecycle statuses, function/workflow contracts, and AI model provider types. All of these can be extended to carry meaningful additional metadata or payload.

**Proper GTS naming is critical.** Because GTS identifiers are used in wildcard-based access-control policies, audit logs, and cross-module references, a well-structured naming hierarchy enables security rules like `gts.cf.core.events.type.v1~acme.*` — restricting vendor Acme to reading only their own events — while a broader `gts.cf.core.events.type.v1~*` policy grants platform clients and integrations access to all vendors' events. Poor naming collapses this hierarchy and forces explicit per-vendor lists, which are fragile and risk leaking one vendor's sensitive data to another.

---

## 1. When to Introduce GTS Types

Use GTS when you identify:

- Data that **must evolve without DB schema changes** — new variants appear at runtime via plugins, integrations, or configuration
- Objects that share a common base but carry **different metadata per plugin, vendor, or integration**
- Structures where fields are **dynamic, plugin-driven, or vendor-specific** (e.g., provider settings, adapter configs)
- "Future unknowns" — metadata, settings, attributes, or payloads that will grow as the platform evolves
- Extension points where **third-party vendors** need to add data without modifying Cyber Ware modules
- Entities that need **type-based access control** — wildcard policies over families of identifiers (see [section 8](#8-gts-based-security-and-wildcard-access-control))

**Use GTS for:**
- Plugin specifications (auth plugins, runtime adapters, guard plugins)
- Event schemas and event payloads
- Resource-group type definitions
- Model provider types, model lifecycle statuses
- Function/workflow definitions and error types
- Configurable entity metadata
- Credential store backend types
- Any domain object that needs cross-module identity with schema validation

**Avoid GTS for:**
- Fully stable, closed schemas that will never be extended
- Internal-only, tightly coupled models with no extension surface
- Truly closed, small-cardinality enums that will never grow, need no description, carry no authorization semantics, and require no vendor extensibility (e.g., `enabled`/`disabled`, `asc`/`desc`). See [section 6.6](#66-gts-well-known-instances-for-constants-and-discriminator-values) for cases where values that *look* like simple enums actually warrant GTS.

---

## 2. GTS Identifier Format

A GTS identifier is a dot-separated string that uniquely names a type or instance. The full specification is at [gts-spec](https://github.com/GlobalTypeSystem/gts-spec); this section covers the practical formats used in Cyber Ware.

### 2.1 Base Type Identifier

A base type defines a core schema. Base type identifiers always **start with `gts.`** and **end with `~`**:

```text
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~
```

#### Cyber Ware examples

Cyber Ware uses `cf` as the vendor name, as it's a part of the Cyber Fabric Foundation.

Cyber Ware has several predifined packages - 'core', 'genai', 'example', etc

```text
gts.cf.core.modkit.plugin.v1~    -- base plugin schema
gts.cf.core.oagw.upstream.v1~    -- OAGW upstream resource type
gts.cf.core.oagw.auth_plugin.v1~ -- OAGW auth plugin type
gts.cf.genai.model.provider.v1~  -- AI model provider base type
gts.cf.genai.model.lifecycle.v1~ -- model lifecycle status base type
gts.cf.core.sless.function.v1~   -- serverless function base type
gts.cf.core.events.type.v1~      -- platform event base type
gts.cf.core.rg.group_type.v1~    -- resource group type
```

### 2.2 Derived Type Identifier

A derived type extends a base type. The chain is separated by `~` and follows **left-to-right inheritance**. All segments end with `~`:

```text
gts.<base_vendor>.<base_package>.<base_ns>.<base_type>.v<MAJOR>[.<MINOR>]~<derived_vendor>.<derived_package>.<derived_ns>.<derived_type>.v<MAJOR>[.<MINOR>]~
```

Cyber Ware examples:

```text
-- Tenant resolver plugin (derives from base plugin)
gts.cf.core.modkit.plugin.v1~cf.core.tenant_resolver.plugin.v1~

-- CredStore plugin
gts.cf.core.modkit.plugin.v1~cf.core.credstore.plugin.v1~

-- Azure AI Studio provider (third-party vendor extending Cyber Ware base)
gts.cf.genai.model.provider.v1~msft.azure._.ai_studio.v1~

-- Vendor audit event extending platform base event and audit event
gts.cf.core.events.type.v1~cf.core.audit.event.v1~vendor.app.store.purchase_audit_event.v1~

-- Starlark runtime adapter plugin
gts.cf.core.sless.adapter_plugin.v1~cf.core.sless.runtime_starlark.v1~

-- Serverless error: not-found (derived from base error type)
gts.cf.core.sless.err.v1~cf.core.sless.not_found.v1~
```

The `gts.` prefix appears **only once**, at the beginning. Subsequent segments in the chain omit it.

### 2.3 Well-Known Instance Identifier

A well-known instance is a canonical, named object of a given type. Instance identifiers **do not end with `~`**:

```text
gts.<type_chain>~<instance_vendor>.<instance_package>.<instance_ns>.<instance_name>.v<V>
```

Cyber Ware well-known instances examples:

```text
-- HTTP protocol instance (well-known OAGW protocol)
gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1

-- API key auth plugin instance
gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1

-- OAuth2 client credentials auth plugin instance
gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1

-- Timeout guard plugin instance
gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1

-- Admin role instance
gts.cf.iam.identity.role.v1~cf.core._.admin.v1
```

### 2.4 Anonymous Instance

Anonymous instances are runtime-created objects (DB rows, events, messages) where a globally meaningful name is not needed. Use a UUID as `id` and store the GTS Type Identifier in a separate `type` column:

```json
{
  "id": "7a1d2f34-5678-49ab-9012-abcdef123456",
  "type": "gts.cf.core.events.type.v1~cf.core.audit.event.v1~acme.app.store.purchase_audit_event.v1~",
  "occurred_at": "2025-09-20T18:35:00Z",
  "payload": { ... }
}
```

Modules may support combined notation for anonymous instances by appending the UUID after the final `~`:

```text
gts.cf.core.oagw.upstream.v1~7a1d2f34-5678-49ab-9012-abcdef123456
```

### Key rules

- **Types always end with `~`. Instances never end with `~`.** Every well-known instance must include a left-hand type segment in the chain. There are no single-segment instances.
- every GTS segment has exactly 4 components: vendor, package, namespace, and name.
- only the first segment starts with `gts.`
- JSON schemas requiring GTS identifiers as URLs may use `gts://` prefix (e.g. `gts.cf.a.b.c.v1`)

---

## 3. Identifier Components

### 3.1 Vendor

The **owner or authority** of the type definition.

- **`cf`** — reserved for CyberFabric vendor, used for all defined base and derived types and well-known instances
- Third-party vendors use their own prefix: `msft`, `google`, `stripe`, `abc`
- Must be globally unique, lowercase, stable

**Good:** `cf`, `msft`, `google`, `stripe`, `acme`
**Wrong:** `common`, `shared`, `test`, `my`, `default`

### 3.2 Package

A **domain-level grouping** — a bounded context, service, or application area.

| Package | Meaning |
|---------|---------|
| `core` | Core platform services (modkit, OAGW, events, resource groups) |
| `genai` | Generative AI subsystem (model registry, LLM gateway) |
| `bss` | Business support (billing, licensing, usage) |

**Rules:**
- Reflects business domain, not technical layer
- Stable over time — changing a package name breaks all downstream references
- Provides logical grouping for inner entities

**Avoid:** `utils`, `misc`, `temp`, `helpers`, `common`

### 3.3 Namespace

Additional grouping inside a package. Use `_` (underscore) when namespace is not needed.

**Use namespace when:**
- The package has many types that benefit from sub-grouping
- You need logical separation to avoid name collisions
- The domain is complex enough to warrant categorization

```text
gts.cf.core.oagw.upstream.v1~         -- namespace: oagw (OAGW domain)
gts.cf.core.oagw.auth_plugin.v1~      -- namespace: oagw
gts.cf.genai.model.provider.v1~       -- namespace: model
gts.cf.genai.model.lifecycle.v1~      -- namespace: model
gts.cf.core.sless.function.v1~        -- namespace: sless (serverless)
gts.cf.bss.billing.invoice.v1~        -- namespace: billing
```

When namespace is irrelevant, use `_`:

```text
gts.cf.core.modkit.plugin.v1~cf.core._.admin.v1
gts.cf.em.event.type.v1~cf.idp._.tenant_created.v1~
```

### 3.4 Type Name

The specific schema or entity name. Use a **singular noun**, not plural.

**Good:** `plugin`, `upstream`, `route`, `provider`, `lifecycle`, `callable`, `event`, `role`
**Wrong:** `plugins`, `create_user`, `is_active`, `handle_request`

Rules:
- Singular noun — not plural, not a verb, not an adjective
- Clear and self-descriptive
- Stable — renaming a type is a breaking change

### 3.5 Version

Versioning uses `v<MAJOR>` or `v<MAJOR>.<MINOR>`:

```text
gts.cf.core.events.type.v1~    -- major only
gts.cf.core.events.type.v1.2~  -- major + minor
```

- **Major increment** (`v1` → `v2`): breaking changes — field removals, renames, semantic changes
- **Minor increment** (`v1.0` → `v1.1`): backward-compatible additions — new optional fields

Prefer **major-only versioning** for simplicity. Use **minor versions** when you need immutable, cacheable type definitions that evolve without breaking consumers.

---

## 4. Type (Schema) vs Instance — When to Use Each

| Concept | Ends with `~` | Purpose | Example |
|---------|:---:|---------|---------|
| **Type (Schema)** | Yes | Defines structure, validation rules, reusable contracts | `gts.cf.core.oagw.auth_plugin.v1~` |
| **Derived Type** | Yes | Specializes a base type for a vendor or context | `gts.cf.core.modkit.plugin.v1~cf.core.credstore.plugin.v1~` |
| **Well-Known Instance** | No | Canonical named value shared across systems | `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1` |
| **Anonymous Instance** | N/A | Runtime object with UUID `id` and separate `type` | `id: UUID`, `type: "gts.cf.core.events.type.v1~..."` |

Use a **type** when you are defining a schema that objects conform to. Use a **well-known instance** when you are naming a specific canonical entity (a built-in auth plugin, a protocol, a predefined role).

---

## 5. Hybrid Storage Pattern — Base Fields + Extension Fields

This is the core database pattern that GTS enables. A module stores base-type fields in dedicated, indexed columns and the extension field (driven by derived types) as a `JSONB` column. New derived types appear without DDL changes.

### 5.1 GTS Type Storage — UUID, Not Strings

**GTS types are stored in the database as UUIDs, not as raw text strings.** The `gts-rust` library deterministically converts any GTS identifier to a UUID v5 using a fixed namespace (`ns:URL` + `"gts"`). This conversion is stable and repeatable — the same GTS string always produces the same UUID.

```rust
// gts-rust deterministic UUID derivation
let type_uuid = gts::GtsID::new("gts.cf.core.events.type.v1~")?.to_uuid();
// Always produces the same UUID for the same GTS string
```

**Why UUIDs instead of strings:**
- **Renaming/aliasing safety** — if a GTS identifier is aliased or renamed, the canonical UUID remains the foreign key; modules that store type references by UUID are unaffected
- **Predictable, fixed storage** — UUID is always 16 bytes; GTS strings can be up to 1024 characters and vary in length, making index sizing unpredictable
- **Efficient joins and indexes** — UUID comparisons are faster than variable-length text comparisons
- **No LIKE-based queries** — type hierarchy queries use the GTS Registry (which resolves GTS relationships), not text pattern matching

> **Anti-pattern:** storing GTS identifiers as `TEXT` / `VARCHAR` columns for type lookups or foreign keys. String storage is acceptable only in log messages, audit trails, and human-readable API responses where the readable form is the primary value.

### 5.2 Database Schema Example (PostgreSQL)

Resource groups use GTS types for extensible group definitions:

```sql
-- GTS type definitions table — stores registered type schemas
CREATE TABLE gts_type (
    id          SMALLINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    type_uuid   UUID NOT NULL UNIQUE,        -- deterministic UUID derived from GTS string
    type_id     TEXT NOT NULL UNIQUE,         -- human-readable GTS type identifier (for display/debug)
    metadata_schema JSONB,                    -- JSON Schema for instance metadata
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Resource groups: base fields in columns, extension in metadata JSONB
CREATE TABLE resource_group (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id   UUID REFERENCES resource_group(id),
    gts_type_id SMALLINT NOT NULL REFERENCES gts_type(id),  -- FK to GTS type (surrogate)
    name        TEXT NOT NULL,
    metadata    JSONB,              -- vendor/type-specific extension data
    tenant_id   UUID NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Efficient queries by type
CREATE INDEX idx_rg_gts_type_id ON resource_group (gts_type_id, id);
CREATE INDEX idx_rg_tenant_id   ON resource_group (tenant_id);
```

### 5.3 Events Table — Hybrid Storage

A platform event system stores base event fields in columns and vendor-specific payload as JSONB:

```sql
CREATE TABLE events (
    id          UUID PRIMARY KEY,
    type_uuid   UUID NOT NULL,          -- deterministic UUID of the GTS type
    occurred_at TIMESTAMPTZ NOT NULL,
    source      TEXT NOT NULL,
    payload     JSONB NOT NULL,         -- vendor-specific extension data
    tenant_id   UUID NOT NULL
);

CREATE INDEX idx_events_type     ON events (type_uuid, occurred_at);
CREATE INDEX idx_events_tenant   ON events (tenant_id);
```

The `type_uuid` column holds the deterministic UUID derived from the GTS chained identifier, e.g.:

```text
"gts.cf.core.events.type.v1~cf.core.audit.event.v1~acme.app.store.purchase_audit_event.v1~"
  → UUID: a3b1c2d4-5678-5aaa-bbbb-ccccddddeeee   (deterministic, repeatable)
```

This enables:
- **No schema migrations** when new event types are registered
- **Efficient UUID-based joins** between events and the type registry
- **Wildcard access control** resolved at the application layer via the GTS Registry (not via SQL `LIKE`)
- **Full validation** of `payload` against the registered GTS schema before insertion

### 5.4 Model Registry — GTS for Provider Types and Lifecycle

The model registry uses GTS types for two extension points:

```text
Provider type (which AI provider):
  gts.cf.genai.model.provider.v1~msft.azure._.ai_studio.v1~
  gts.cf.genai.model.provider.v1~google.vertex._.gemini.v1~
  gts.cf.genai.model.provider.v1~ollama.local._.gguf.v1~

Lifecycle status (model maturity):
  gts.cf.genai.model.lifecycle.v1~cf.genai._.production.v1
  gts.cf.genai.model.lifecycle.v1~cf.genai._.preview.v1
  gts.cf.genai.model.lifecycle.v1~cf.genai._.experimental.v1
```

The `type_uuid` column on the `providers` table allows:
- Filtering all Azure providers by resolving matching UUIDs via the GTS Registry
- GTS-based access control: grant a tenant access to `gts.cf.genai.model.provider.v1~msft.*` (all Microsoft providers)
- Adding new provider types (e.g., a new vendor) without DB migration

### 5.5 JSON Schema Definition (GTS Type Schema)

A base event type defined as a JSON Schema with GTS conventions:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.events.type.v1~",
  "type": "object",
  "properties": {
    "id":          { "type": "string" },
    "type_id":     { "type": "string" },
    "occurred_at": { "type": "string", "format": "date-time" },
    "payload":     { "type": "object", "additionalProperties": true }
  },
  "required": ["id", "type_id", "occurred_at", "payload"],
  "additionalProperties": false
}
```

A vendor-specific derived event type:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.events.type.v1~abc.commerce.orders.order_placed.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.events.type.v1~" },
    {
      "properties": {
        "payload": {
          "type": "object",
          "properties": {
            "order_id":    { "type": "string" },
            "customer_id": { "type": "string" },
            "total":       { "type": "number" },
            "currency":    { "type": "string" }
          },
          "required": ["order_id", "customer_id", "total"],
          "additionalProperties": false
        }
      }
    }
  ]
}
```

The `$id` uses the `gts://` URI prefix. The `$ref` references the base type. The derived type refines `payload` while preserving all base-type fields.

**Review note — current upstream spec rules:**

- Hand-authored GTS JSON Schemas must declare `"$schema": "http://json-schema.org/draft-07/schema#"`.
- Do not introduce post-Draft-07 keywords in GTS Type Schemas. Explicitly forbidden: `$defs`, `prefixItems`, `unevaluatedProperties`, `unevaluatedItems`, `$dynamicRef`, `$dynamicAnchor`, `dependentRequired`, `dependentSchemas`. For local reusable subschemas in handwritten schemas, use the Draft-07 canonical keyword `definitions` (not `$defs`). Local JSON Pointer references such as `"$ref": "#/definitions/Foo"` are the recommended form.
- Treat `x-gts-traits`, `x-gts-traits-schema`, `x-gts-final`, and `x-gts-abstract` as **schema-only** keywords. They must not appear in instance payloads. The library enforces this at the boundary — registering an instance that carries these fields is a hard validation error.
- At API and persistence boundaries, fields that carry a GTS type reference must be validated as a full **GTS Type Identifier** ending with `~`. A non-GTS `$schema` URL (e.g., `"http://json-schema.org/draft-07/schema#"`) is **not** a valid `type_id` — only strings that parse as valid GTS Type Identifiers are accepted.

---

## 6. GTS in Rust Code

**Upstream naming has shifted to `type_id` terminology.** In new Rust code and in PR reviews, prefer `type_id`, `TYPE_ID`, `BASE_TYPE_ID`, `gts_type_id()`, `gts_base_type_id()`, `innermost_type_id()`, and `GtsTypeId`. Treat `schema_id`, `SCHEMA_ID`, `BASE_SCHEMA_ID`, `gts_schema_id()`, and `GtsSchemaId` as deprecated compatibility aliases unless you are touching legacy call sites. `GtsSchemaId` is now a deprecated type alias (`pub type GtsSchemaId = GtsTypeId`). The `schema_id = "..."` macro attribute emits a compile-time deprecation warning — new code must use `type_id = "..."`. Specifying both `type_id` and `schema_id` on the same struct is a hard compile error.

In gts-rust result types, note the distinction between two predicates: `is_type_schema` (entity results — is this entity a GTS Type Schema, not an instance?) vs `is_type` (parse/validate ID results — does this GTS Identifier end with `~`, making it a type rather than an instance ID?).

Cyber Ware generates GTS type schemas from Rust structs using the `#[struct_to_gts_schema]` macro. This keeps type definitions in code, not in hand-maintained JSON files.

### 6.1 Defining a Base Type

```rust
// libs/modkit/src/gts/plugin.rs
use gts::GtsInstanceId;
use gts_macros::struct_to_gts_schema;

#[derive(Debug)]
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    type_id = "gts.cf.core.modkit.plugin.v1~",
    description = "Base modkit plugin schema",
    properties = "id,vendor,priority,properties"
)]
pub struct BaseModkitPluginV1<P: gts::GtsSchema> {
    pub id: GtsInstanceId,
    pub vendor: String,
    pub priority: i16,
    pub properties: P,  // extension point — typed by derived type
}
```

The `base = true` flag marks this as a base GTS type. The `properties` field is the extension point: each derived plugin type fills it with its own specific data.

**Review note — GTS identifier field types:** Rust struct fields that carry GTS identifiers must use typed wrappers, not `String`:
- `GtsInstanceId` — for fields holding an instance identifier (does **not** end with `~`)
- `GtsTypeId` — for fields holding a type identifier (ends with `~`)

Using `String` bypasses structural validation and makes the origin of the value unverifiable in a PR review. The `id: GtsInstanceId` in the example above is the canonical pattern.

### 6.2 Defining a Derived Type (Plugin Spec)

```rust
// modules/credstore/credstore-sdk/src/gts.rs
use gts_macros::struct_to_gts_schema;
use modkit::gts::BaseModkitPluginV1;

#[struct_to_gts_schema(
    dir_path = "schemas",
    base = BaseModkitPluginV1,
    type_id = "gts.cf.core.modkit.plugin.v1~cf.core.credstore.plugin.v1~",
    description = "CredStore plugin specification",
    properties = ""
)]
pub struct CredStorePluginSpecV1;
```

The `base = BaseModkitPluginV1` links this derived type to its base. The `type_id` is a chained GTS identifier showing the inheritance: `base~derived~`.

**Review note — generated schema shape matters:** when PRs add or modify generic or nested derived GTS structs, review the generated schema artifact or tests, not just the Rust type. Derived overlays must be wrapped at the full nesting path under the intended extension field; otherwise fields can leak to the top level and accidentally violate `additionalProperties: false` contracts. The `description` attribute is now emitted into the generated JSON schema root — a PR that adds or changes `description` on a `#[struct_to_gts_schema]` struct must also update the checked-in schema artifact.

**Review note — type schema definition checklist:**
- `type_id = "..."` attribute ends with `~`; use `type_id` not the deprecated `schema_id`
- For base types: `base = true`; for derived types: `base = ParentStruct` (not `base = true`) — the macro validates at compile time that the `type_id` prefix matches `<ParentStruct as GtsSchema>::TYPE_ID`
- `description` attribute is present — it is now emitted into the schema JSON root
- Struct fields that reference other GTS types use `GtsTypeId` / `GtsInstanceId`, not `String`
- At runtime, reference the type via `SomeStruct::gts_type_id()` (returns `&'static GtsTypeId`) rather than a raw `&str` constant

### 6.3 Registering a Plugin Instance

```rust
// In a plugin module's init() method
async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
    // Generate the well-known instance ID
    let instance_id = CredStorePluginSpecV1::gts_make_instance_id(
        "cf.builtin.default_credstore.plugin.v1"
    );

    // Create the plugin instance payload
    let instance = BaseModkitPluginV1::<CredStorePluginSpecV1> {
        id: instance_id.clone(),
        vendor: "builtin".into(),
        priority: 100,
        properties: CredStorePluginSpecV1,
    };

    // Register with the types-registry
    let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
    registry.register(vec![serde_json::to_value(&instance)?]).await?;

    // Register as a scoped client in ClientHub
    ctx.client_hub().register_scoped::<dyn CredStoreClient>(
        ClientScope::gts_id(&instance_id),
        Arc::new(DefaultCredStorePlugin::new()),
    );

    Ok(())
}
```

**Review note — instance definition checklist:**
- Instance ID created via `SomeType::gts_make_instance_id(segment)` — **never** via string concatenation or `format!("{SCHEMA_ID}{segment}")`
- The `segment` argument does **not** end with `~` — instance identifiers are not type identifiers
- The struct field holding the instance ID uses `GtsInstanceId`, not `String`
- The instance payload body must **not** contain schema-only keywords (`x-gts-traits`, `x-gts-traits-schema`, `x-gts-final`, `x-gts-abstract`) — the library rejects such payloads at registration with a structured error
- The type used as the instance's schema must itself be defined via `#[struct_to_gts_schema]` before any instances are registered

### 6.4 Defining GTS Constants

For well-known types and instances, define constants in a dedicated `gts_helpers` module:

```rust
// modules/system/oagw/oagw/src/domain/gts_helpers.rs

// Schema GTS identifiers (types)
pub const UPSTREAM_SCHEMA: &str  = "gts.cf.core.oagw.upstream.v1~";
pub const ROUTE_SCHEMA: &str     = "gts.cf.core.oagw.route.v1~";
pub const AUTH_PLUGIN_SCHEMA: &str = "gts.cf.core.oagw.auth_plugin.v1~";

// Builtin instances (well-known — no trailing ~)
pub const HTTP_PROTOCOL_ID: &str = "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1";
pub const APIKEY_AUTH_ID: &str   = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1";
pub const BEARER_AUTH_ID: &str   = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.bearer.v1";

// Format an anonymous instance (UUID-based)
pub fn format_upstream_gts(id: Uuid) -> String {
    format!("{UPSTREAM_SCHEMA}{}", id.hyphenated())
}

// Parse a GTS identifier back into schema + UUID
pub fn parse_resource_gts(s: &str) -> Result<(String, Uuid), DomainError> {
    let gts_id = gts::GtsID::new(s)?;
    let tilde_pos = s.rfind('~').ok_or(/* ... */)?;
    let uuid = Uuid::parse_str(&s[tilde_pos + 1..])?;
    Ok((s[..tilde_pos].to_string(), uuid))
}
```

**Review note — GTS constant declarations:** `pub const FOO: &str = "gts..."` is acceptable for compile-time use where `const` evaluation is required. At runtime, prefer the typed accessor methods generated by the macro:
- Type IDs: `SomeStruct::gts_type_id()` → `&'static GtsTypeId` (validated, typed)
- Instance IDs: `SomeStruct::gts_make_instance_id(segment)` → `GtsInstanceId` (validated, typed)

Avoid `format!` to build GTS identifiers at runtime — typed constructors validate structure and prevent silent truncation or extra `~` mistakes.

### 6.5 Using GTS Types in Error Mapping (RFC 9457)

GTS identifiers are used as the `type` URI in RFC 9457 Problem responses:

```rust
impl From<SlessOrchestratorError> for Problem {
    fn from(e: SlessOrchestratorError) -> Self {
        match e {
            SlessOrchestratorError::NotFound { id } =>
                Problem::not_found()
                    .with_type_uri("gts://gts.cf.core.sless.err.v1~cf.core.sless.err.not_found.v1~")
                    .with_detail(format!("Function not found: {id}")),
            SlessOrchestratorError::RateLimited { retry_after_seconds, .. } =>
                Problem::too_many_requests()
                    .with_type_uri("gts://gts.cf.core.sless.err.v1~cf.core.sless.err.rate_limited.v1~")
                    .with_header("Retry-After", retry_after_seconds),
            // ...
        }
    }
}
```

### 6.6 GTS Well-Known Instances for Constants and Discriminator Values

When an API field acts as a **discriminator** — selecting behavior, routing logic, or authorization rules — prefer GTS instance URIs over raw string constants or Rust/OpenAPI enums, even when the initial set of values looks small and stable.

The typical pattern is:

- A **base GTS type** represents the category, for example `gts.cf.qe.quota.type.v1~`.
- Each valid value is modeled as a **well-known child instance**, for example `gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1`.

This replaces ad-hoc string enums with structured, registry-backed identifiers that bring several advantages described below.

#### Why GTS instances instead of string constants

**1. Descriptions travel with the value.**

A GTS schema entry carries `description`, `display_name`, `tags`, and arbitrary `properties`. A bare string constant like `"consumption"` carries nothing — its meaning lives in scattered comments, separate documentation and PRD prose that drifts from code over time. Developers can resolve a GTS URI against the registry and get a human-readable description, the owning team, version history, and all custom properties attached to the schema.

**2. Discoverability via API.**

With string constants the only way to know valid values is to read source code or a PRD. With GTS the registry is queryable at runtime:

```text
GET /gts/type-schemas?parent=gts://gts.cf.qe.quota.type.v1~
→ ["~cf.qe.quota.consumption.v1", "~cf.qe.quota.allocation.v1", "~cf.qe.quota.rate.v1"]
```

Client SDKs, admin UIs, and integration tests can all enumerate valid values without hard-coding an enum or maintaining a separate "valid values" document.

**3. Misprint detection at the boundary.**

A string constant is validated only if someone wrote a hand-rolled allowlist check. A GTS URI is validated on build time or by resolving it against the registry in runtime — the registry either has it or returns a structured `404 Not Found` with the offending URI. A typo like `"consumptoin"` is caught at the build time or at the API boundary, not deep inside an evaluation engine.

**4. No API versioning churn when the set grows.**

Adding a value to a Rust `enum` or an OpenAPI `enum` constraint is a **breaking change**: generated client SDKs that pattern-match exhaustively fail to compile; OpenAPI validators reject the new value until the schema is republished; consumers with cached specs get `422`. With GTS the API field stays `string (GTS id)` — adding a new value means registering a new schema in the registry. The API contract does not change. Existing clients are unaffected. No version bump.

**5. Self-identifying values in logs and error responses.**

A string constant like `"hard"` or `"monthly"` is opaque in a log line. Without surrounding context it is impossible to know which field it came from or which domain concept it represents. GTS URIs are self-describing and unambiguous:

```text
# Opaque — searching for "hard" matches anything
quota_check failed: enforcement=hard

# Self-describing — greppable, unambiguous, cross-service
quota_check failed: enforcement=gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1
```

Benefits: grep is unambiguous; RFC 9457 Problem bodies point directly to the registry entry; cross-service correlation uses the same URI without a field-name mapping table; two modules logging a field named `type` have no collision because the URI prefix carries the domain.

**6. Vendor and plugin extensions without forking.**

Hard-coded value lists cannot be extended by plugin vendors without modifying the core enum and redeploying the platform. With GTS a vendor registers their own child schema under the platform's parent:

```text
gts.cf.qe.subject.type.v1~acme.billing.subject.cost_center.v1
```

The engine resolves the URI and delegates to the plugin that declared it. The platform never needs to know about `cost_center` — the registry and the plugin contract are sufficient.

**7. Schema properties drive engine behavior — no match arms.**

In Cyber Ware, behavior-driving semantics are typically attached to the resolved GTS schema as JSON Schema metadata — most often via `x-gts-traits` and `x-gts-traits-schema` — instead of being hardcoded in `match` arms:

```json
{
  "$id": "gts://gts.cf.core.rg.group_type.v1~cf.core.rg.department.v1~",
  "x-gts-traits-schema": {
    "type": "object",
    "properties": {
      "allowed_parent_types": { "type": "array", "default": [] },
      "idp_provisioning": { "type": "boolean", "default": false }
    }
  },
  "x-gts-traits": {
    "allowed_parent_types": ["gts.cf.core.rg.group_type.v1~cf.core.rg.company.v1~"],
    "idp_provisioning": true
  }
}
```

The engine resolves the schema, reads these semantic properties, and adjusts behavior accordingly. A new value or derived type can declare different trait values without requiring engine code changes. When those values themselves are GTS identifiers, the same pattern enables multi-level schema-driven dispatch.

**8. GTS-driven authorization without hardcoded role checks.**

When the required permission depends on a discriminator value (e.g., different authorization for `source=licensing` vs `source=tenant_admin`), the permission URI can live in the GTS schema's properties instead of in a `match` arm:

```rust
// Handler has zero role knowledge — authorization is fully schema-driven
let source_schema = gts.resolve(&request.source).await?;
let required_permission = source_schema.properties["required_permission"];
enforcer.access_scope(&ctx, &RESOURCE, required_permission, None).await?;
```

Adding a new source value requires registering the schema with its `required_permission` property and configuring the PDP — zero handler code changes.

#### Example: before and after

**Before — string constants:**

```json
{
  "subject_type": "tenant",
  "metric": "llm_tokens",
  "quota_type": "consumption",
  "enforcement_mode": "hard"
}
```

**After — GTS URIs:**

```json
{
  "subject_type": "gts.cf.qe.subject.type.v1~cf.qe.subject.tenant.v1",
  "metric": "gts.cf.uc.metric.type.v1~cf.uc.metric.llm_token.v1",
  "quota_type": "gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1",
  "enforcement_mode": "gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1"
}
```

The validation path at the API boundary:
1. Receive GTS string
2. Resolve against the GTS registry — `404` if misprint, `200` + schema if valid
3. Assert the resolved schema's parent matches the expected base URI (guards against cross-category confusion)
4. Read `properties` block from schema for engine configuration — no match arms

#### When string constants are still appropriate

Use plain strings or Rust enums when **all** of the following hold:

- The set is truly closed and will never grow (e.g., `enabled`/`disabled`, `asc`/`desc`)
- No description, discoverability, or schema properties are needed
- No authorization decision depends on the value
- No vendor or plugin will ever need to add values
- The value carries no domain semantics worth logging or correlating across services

If even one of these conditions is false, use GTS.

---

## 6.7 Heterogeneous Dispatch — `GtsSchema for Value` and `try_narrow`

When a Rust carrier must transport **multiple concrete GTS leaf types** at runtime through a common channel (e.g., a heterogeneous event batch, a multi-provider model catalog, an RPC boundary), use `serde_json::Value` as the generic parameter and `gts::try_narrow` for typed dispatch:

```rust
use gts::{GtsSchema, NarrowError, try_narrow};

// Carrier arrives with opaque payload — leaf type unknown at compile time
let EnvelopeV1 { gts_type, payload } = receive::<EnvelopeV1<serde_json::Value>>(raw)?;

match gts_type.as_ref() {
    id if id == <AuditEventPayloadV1 as GtsSchema>::innermost_type_id() => {
        let typed: AuditEventPayloadV1 = try_narrow(id, payload)?;
        handle_audit(&typed);
    }
    id if id == <BillingEventPayloadV1 as GtsSchema>::innermost_type_id() => {
        let typed: BillingEventPayloadV1 = try_narrow(id, payload)?;
        handle_billing(&typed);
    }
    unknown => tracing::warn!(gts_id = unknown, "unknown event type — skipping"),
}
```

**Key points:**
- **Always match on `<Q>::innermost_type_id()`**, not raw string literals — for multi-level chains (e.g., `Intermediate<Leaf>`), `innermost_type_id()` walks to the leaf id automatically.
- **`try_narrow` returns two error variants**: `NarrowError::SchemaId` (discriminator mismatch — wrong leaf type) and `NarrowError::Deserialize` (shape mismatch — malformed data).

**Avoid:**
- Using `Base<Value>` as a permanent storage or processing type — narrow to a concrete type before business logic
- Deserializing macro-generated nested GTS types directly with `serde::from_value` — use `try_narrow`, which correctly routes through `GtsDeserializeWrapper`
- Comparing runtime discriminator strings to raw string literals instead of `<Q>::innermost_type_id()`

---

## 7. Base Types — Design Guidelines

Base types define **foundational, stable concepts** that derived types specialize. They are the core contract that all extensions must preserve.

**Good practices:**
- Use a **singular noun** as the type name
- Keep the schema **minimal and stable** — only fields that all instances share
- Mark extension fields as `"type": "object", "additionalProperties": true` to allow derived types to refine them
- Mark the type as `"x-gts-abstract": true` if direct instantiation makes no sense
- Focus on **core invariants** — fields that define the concept, not optional features

**Cyber Ware base type examples:**

| Base Type | Purpose |
|-----------|---------|
| `gts.cf.core.modkit.plugin.v1~` | Base plugin schema — all Cyber Ware plugins derive from this |
| `gts.cf.core.events.type.v1~` | Base event schema — id, type_id, occurred_at, payload |
| `gts.cf.core.oagw.auth_plugin.v1~` | OAGW auth plugin type |
| `gts.cf.genai.model.provider.v1~` | AI model provider type |
| `gts.cf.core.sless.callable.v1~` | Serverless callable (function/workflow) base |
| `gts.cf.core.rg.group_type.v1~` | Resource group type definition |

**Avoid:**
- Verbs or adjectives as type names (`create_event`, `is_active`)
- Rapidly changing fields in the base schema
- Overloading a single base type with unrelated responsibilities

---

## 8. GTS-Based Security and Wildcard Access Control

**This is a critical architectural capability.** GTS naming structure directly enables wildcard-based access-control policies. If identifiers are poorly structured, security rules become impossible to express generically.

### 8.1 Why Naming Structure Matters for Security

Cyber Ware's authorization pipeline uses GTS identifiers as resource types in access-control decisions. The `AccessScope` returned by the PDP can include type-based constraints using GTS wildcard patterns. This means:

```text
Policy: role "ai_admin" → allow read on gts.cf.genai.model.provider.v1~msft.*
Effect: Grants access to ALL Microsoft AI providers (Azure, etc.) without listing each one
```

```text
Policy: role "auditor" → allow read on gts.cf.core.events.type.v1~cf.core.audit.*
Effect: Grants access to ALL audit event types and their vendor extensions
```

```text
Policy: role "vendor_abc_admin" → allow write on gts.cf.core.events.type.v1~acme.*
Effect: Grants access to ALL event types registered by vendor ACME
```

If you name types poorly (e.g., flatten the hierarchy, use inconsistent packages), these wildcard rules cannot be expressed, and you fall back to maintaining explicit lists — which is fragile, error-prone, and does not scale.

### 8.2 Wildcard Patterns

| Pattern | Matches |
|---------|---------|
| `gts.cf.core.oagw.*` | All OAGW types (upstreams, routes, auth plugins, etc.) |
| `gts.cf.genai.model.provider.v1~msft.*` | All Microsoft-derived provider types |
| `gts.cf.core.modkit.plugin.v1~*` | All derived plugin types and instances |
| `gts.cf.core.events.type.v1~cf.core.audit.event.v1~abc.*` | All of vendor ABC's audit events |

### 8.3 Naming Rules That Enable Security

1. **Keep vendor segments consistent** — a vendor must always use the same prefix so `vendor.*` patterns work
2. **Keep package segments stable** — changing packages breaks existing wildcard policies
3. **Use namespace for sub-grouping** — enables mid-hierarchy wildcards (`gts.cf.genai.model.*` vs `gts.cf.genai.inference.*`)
4. **Use the type chain to express ownership** — the rightmost segments identify the vendor extension, enabling vendor-scoped wildcard isolation
5. **Never embed authorization-relevant data in the type name itself** — use the structured segments (vendor, package, namespace) for policy matching

### 8.4 GTS-Based Access Control in Practice

The model registry demonstrates GTS-based authorization:

| Access Type | GTS Claim Required | Example |
|------------|-------------------|---------|
| Provider access | Provider GTS type | `gts.cf.genai.model.provider.v1~msft.azure.*` — access to all Azure models |
| Lifecycle access | Lifecycle GTS type | `gts.cf.genai.model.lifecycle.v1~cf.genai._.experimental.*` — access to experimental models |

These are "cheap generic rules" — no custom development needed. The platform's existing GTS claim infrastructure handles them. Access can be granted or revoked by provider type or model category at the token level.

---

## 9. Derived Types — Design Guidelines

Derived types extend base types for specialization. In Cyber Ware, plugins, vendor-specific integrations, and runtime extensions are all expressed as derived types.

**Good practices:**
- Add only **context-specific fields** — do not duplicate base fields
- Preserve base semantics — a derived type must be a valid instance of all its base types
- Use clear specialization naming that reflects the vendor and purpose
- Keep the derivation chain **shallow** — ideally one level deep (base → derived), two at most (base → intermediate → vendor)
- Use `allOf` with `$ref` in JSON Schema to express inheritance

**Cyber Ware derived type examples:**

```text
Base:    gts.cf.core.modkit.plugin.v1~
Derived: gts.cf.core.modkit.plugin.v1~cf.core.tenant_resolver.plugin.v1~
         gts.cf.core.modkit.plugin.v1~cf.core.credstore.plugin.v1~
         gts.cf.core.modkit.plugin.v1~cf.core.mini_chat_model_policy.plugin.v1~

Base:    gts.cf.core.events.type.v1~
Derived: gts.cf.core.events.type.v1~cf.core.audit.event.v1~
         gts.cf.core.events.type.v1~cf.core.audit.event.v1~abc.app.store.purchase_audit_event.v1~
```

**Avoid:**
- Deeply nested derivation chains (>2 levels) — they are hard to reason about and validate
- Breaking base-type assumptions (e.g., removing required fields, narrowing open content models)
- Creating shallow variations without meaningful specialization
- Duplicating base fields in the derived schema

---

## 10. GTS Traits

GTS traits are **structured cross-cutting semantic properties** attached to a type-schema. They are used to define meaning and system behavior that cuts across many otherwise unrelated types — not just to describe data shape. In Cyber Ware:

- `x-gts-traits-schema` defines the JSON Schema for allowed trait keys, value types, and defaults
- `x-gts-traits` carries the actual trait values for that type-schema
- traits are inherited along the derivation chain and merged right-to-left, so descendant values override ancestor values for the same key
- `x-gts-traits-schema` and `x-gts-traits` belong only in **type schemas**, never in instance documents

Traits do not replace the main schema shape and they do not create a separate inheritance system. They are a compact way to attach reusable semantic properties that can influence routing, validation, retention, provisioning, policy, or other platform behavior across many otherwise unrelated types.

**Use traits when:**
- Multiple unrelated types share the same semantic property set (e.g., retention, routing topic, provisioning flags)
- You want to annotate a type with metadata that doesn't materially change its core schema structure
- You need to query types by capability rather than by inheritance chain

**Example — resource group traits:**

```json
{
  "$id": "gts://gts.cf.core.rg.group_type.v1~cf.core.rg.department.v1~",
  "x-gts-traits-schema": {
    "type": "object",
    "properties": {
      "allowed_parent_types": { "type": "array", "default": [] },
      "idp_provisioning": { "type": "boolean", "default": false }
    }
  },
  "x-gts-traits": {
    "allowed_parent_types": ["gts.cf.core.rg.group_type.v1~cf.core.rg.company.v1~"],
    "idp_provisioning": true
  },
  "type": "object",
  "allOf": [{ "$ref": "gts://gts.cf.core.rg.group_type.v1~" }]
}
```

The GTS Registry resolves both the effective trait values and the effective trait schema across the inheritance chain. This means a base type can declare defaults in `x-gts-traits-schema`, while a derived type overrides only the values it needs in `x-gts-traits`.

**Avoid:**
- Using traits as a replacement for proper type inheritance
- Using traits for core domain fields that belong in the main schema
- Treating `x-gts-traits` as an untyped list of markers when the value really has structure and defaults
- Proliferating traits without clear semantics

---

## 11. Abstract and Final Types

### Abstract Types (`x-gts-abstract: true`)

Types that **cannot be instantiated directly**. Instances must use a concrete derived type.

**Use when:**
- Defining a base contract that only makes sense when specialized
- Enforcing that every instance carries vendor/context-specific data
- Multiple derived types are expected

**Example:**

```json
{
  "$id": "gts://gts.cf.core.events.type.v1~",
  "x-gts-abstract": true,
  "type": "object",
  "properties": {
    "id": { "type": "string" },
    "type_id": { "type": "string" },
    "occurred_at": { "type": "string", "format": "date-time" },
    "payload": { "type": "object", "additionalProperties": true }
  },
  "required": ["id", "type_id", "occurred_at", "payload"]
}
```

Direct instantiation of this type is invalid — events must be a concrete derived type (e.g., `~cf.core.audit.event.v1~`).

**Avoid:** making every type abstract, using abstract types with no extensions.

### Final Types (`x-gts-final: true`)

Types that **must not be extended**. No derived types are allowed.

**Use when:**
- The structure must remain fixed (e.g., signed/verified structures)
- External contracts depend on the exact shape
- Security or compliance constraints prevent extension

**Example:**

```json
{
  "$id": "gts://gts.cf.core.sless.err.v1~cf.core.sless.err.rate_limited.v1~",
  "x-gts-final": true,
  "type": "object",
  "properties": {
    "retry_after_seconds": { "type": "integer" }
  }
}
```

**Avoid:** marking types as final prematurely — this blocks valid future extension use cases.

---

## 12. Cyber Ware GTS Conventions

1. **Vendor prefix**: all CyberFabric-defined base types use `cf` as vendor (include the Cyber Ware types)
2. **SDK placement**: GTS type definitions live in `<module>-sdk/src/gts.rs`
3. **Schema generation**: use `#[struct_to_gts_schema]` macro — do not maintain schemas by hand
4. **Constants**: well-known GTS identifiers are defined as `const` strings in `domain/gts_helpers.rs` or `gts.rs`
5. **Registration**: plugins register GTS instances in the GTS Registry during `init()`
6. **DB storage**: base fields in columns, extension data in `JSONB` or `TEXT`
7. **Error types**: use GTS identifiers as RFC 9457 `type` URIs for machine-readable error classification
8. **Access control**: structure identifiers so that wildcard policies can grant/revoke access at the vendor, package, or namespace level
9. **Dylint enforcement**: GTS-specific lints validate identifier correctness and prevent unsupported patterns at compile time
10. **Constants as GTS instances**: discriminator fields and string constants that select behavior, routing, or authorization should be GTS well-known instances — not raw strings or Rust enums (see [section 6.6](#66-gts-well-known-instances-for-constants-and-discriminator-values))
11. **Rust naming**: in new code prefer `type_id`/`TYPE_ID`/`GtsTypeId` naming; treat `schema_id` names as deprecated compatibility aliases. The `schema_id = "..."` macro attribute produces a compile-time deprecation warning — use `type_id = "..."` instead
12. **Schema dialect**: handwritten GTS JSON Schemas and fixtures must target Draft-07 (`"$schema": "http://json-schema.org/draft-07/schema#"`) and avoid post-Draft-07 keywords; use `definitions` (not `$defs`) for local reusable subschemas
13. **Heterogeneous carriers**: when a type must hold multiple concrete GTS leaf types at runtime, use `serde_json::Value` as the generic parameter with `try_narrow` for typed dispatch (see [section 6.7](#67-heterogeneous-dispatch--gtsschema-for-value-and-try_narrow))
14. **Typed GTS ID fields**: Rust struct fields that carry GTS identifiers use `GtsTypeId` (for type refs ending with `~`) or `GtsInstanceId` (for instance refs) — never plain `String` or `&str`
15. **Instance ID construction**: use `SomeType::gts_make_instance_id(segment)` — never string concatenation; the segment must not end with `~`

---

## 13. Reviewing GTS in PRD

When reviewing a PRD, focus on whether the product requirements create an extension point that should be modeled with GTS rather than with ad-hoc JSON, enums, or vendor-specific tables.

**Review PRD requirements for:**
- Whether the domain concept is expected to grow through plugins, vendors, integrations, or configuration over time
- Whether the PRD distinguishes clearly between a **type definition** and a **runtime instance**
- Whether new discriminator values, provider types, statuses, or policies are expected to appear without API or DB breaking changes
- Whether authorization, routing, or business behavior depends on a value that is currently described as a raw string enum
- Whether the requirement expects cross-module discoverability, auditability, or registry lookup

**Strong PRD signals that GTS is needed:**
- "Third parties can add new kinds"
- "Admins can register new providers / plugin types / policies"
- "The set of values will expand over time"
- "Behavior depends on type/category/source"
- "We need tenant/vendor scoped access control"
- "We need machine-readable error categories"

**PRD review questions:**
- Is the PRD accidentally specifying a closed enum where the business actually needs an extensible category?
- Does the PRD require new variants without coordinated client releases? If yes, raw OpenAPI enums are usually the wrong fit.
- Does the PRD identify which fields are stable base fields versus extension fields?
- Does the PRD call for authorization or routing by category, source, provider, or status? If yes, should those values be GTS instances?
- Does the PRD need a registry/discovery story for valid values and their metadata?

**Common PRD anti-patterns:**
- Requiring extensibility while also hardcoding a closed enum in the contract
- Saying "custom metadata" without defining the stable base contract and extension surface
- Requiring vendor-specific behavior but omitting vendor ownership in identifiers
- Requiring future additions while assuming DB migrations for each new type

## 14. Reviewing GTS in DESIGN

When reviewing a design document, verify that the architecture actually realizes the GTS model promised by the PRD. The design should make type registration, validation, storage, and runtime consumption explicit.

**Design must make clear:**
- What the **base type** is, and which fields are invariant across all derived types
- What the **extension field** is (`payload`, `metadata`, `properties`, etc.)
- Whether the type should be **abstract** or **final**
- How derived types are registered, resolved, and validated
- Where the registry lives and how consumers look up schemas / well-known instances
- How access control uses the identifier hierarchy or schema properties
- How versioning works for breaking vs compatible change

**Design review questions:**
- Does the design separate **schema evolution** from **runtime instance storage**?
- Does it use hybrid storage correctly: base fields in indexed columns, extension data in JSONB/TEXT?
- Does it validate incoming instances against the registered GTS Type Schema before persistence or processing?
- Does it rely on wildcard/registry resolution rather than SQL `LIKE` over stored strings for hierarchy semantics?
- Does it define whether the behavior is driven by schema properties / traits versus hardcoded branching?
- Does it explain how new derived types or instance values can be introduced without changing APIs, tables, or handler code?
- Does it define the ownership of identifier segments: vendor, package, namespace, name, version?

**What a strong design should explicitly contain:**
- Canonical `type_id` examples for base types, derived types, and well-known instances
- Validation flow at boundaries: parse -> resolve -> validate -> authorize -> persist/process
- Rules for when to use `x-gts-traits`, `x-gts-abstract`, and `x-gts-final`
- Error mapping strategy when GTS types are exposed in RFC 9457 Problem responses
- Compatibility expectations for minor vs major versions

**Common DESIGN anti-patterns:**
- Manual if/else or `match` branching on raw string discriminator values where schema properties should drive behavior
- Storing only strings and depending on text pattern matching instead of registry-aware type handling
- Treating traits as arbitrary metadata blobs instead of structured schema-level semantics
- Allowing instance payloads to carry schema-only `x-gts-*` keywords
- Designing deep derivation chains that are hard to validate and reason about

## 15. Reviewing GTS in Rust Code PRs

When reviewing Rust code, verify not just naming correctness but also that the code preserves the intended schema shape, validation rules, and runtime semantics.

**Rust review checks:**
- Prefer canonical naming: `type_id`, `TYPE_ID`, `BASE_TYPE_ID`, `gts_type_id()`, `GtsTypeId`
- Use `#[struct_to_gts_schema]` for GTS schema generation instead of hand-maintained JSON where possible
- Ensure generated schemas use Draft-07 semantics and GTS URI forms for `$id` / `$ref`
- Validate boundary fields as full GTS Type Identifiers ending with `~`
- Keep schema-only keywords out of instance documents
- Keep well-known GTS constants centralized rather than scattering magic strings across handlers and services

**Code review questions:**
- Does the PR introduce a new extensible concept but model it as a Rust enum or free-form string instead of GTS?
- Does the PR use deprecated `schema_id` names in new code when `type_id` is available?
- If generics or nested derived structs changed, do tests or generated artifacts prove the overlay lands at the correct full nesting path?
- Is the code resolving schemas/instances from the registry rather than duplicating business meaning in handler `match` arms?
- Are discriminator-driven permissions, routing, or processing rules read from schema metadata/traits instead of being hardcoded?
- Are GTS errors surfaced at the API boundary with actionable messages?
- If the PR introduces a `Base<serde_json::Value>` heterogeneous carrier, is `try_narrow` used for dispatch, and are discriminators compared via `<Q>::innermost_type_id()` rather than raw string literals?

**High-risk Rust changes to inspect carefully:**
- Macro attribute changes on `#[struct_to_gts_schema]`
- Renames touching `type_id` fields, constants, or generated methods
- Changes to JSON Schema emission, `allOf` structure, nested generic paths, or `$ref` generation
- Changes to validation/parsing at API boundaries
- Changes that allow instances to carry schema-only keywords
- Changes that replace GTS-backed values with raw strings or enums
- Changes to `description` attribute values on `#[struct_to_gts_schema]` structs — verify the checked-in schema artifact reflects the new value in the JSON root
- New `Base<serde_json::Value>` carrier patterns without a corresponding `try_narrow` dispatch site

**Common Rust anti-patterns:**
- Hand-parsing identifiers with string slicing when `gts-rust` types already exist
- Accepting any string starting with `gts.` where a full type ID is required
- Adding new behavioral branches in code for values that should instead be registered as well-known instances with schema metadata
- Updating Rust structs without checking the emitted schema or compatibility consequences
- Deserializing `Base<Value>` payloads directly with `serde::from_value` instead of `try_narrow` when macro-generated nested GTS types are involved
- Matching runtime discriminator strings as raw literals instead of `<Q>::innermost_type_id()`

## Quick Review Checklist

- [ ] Is this an extension point that should use GTS? (dynamic metadata, plugin, vendor-specific data)
- [ ] Is the identifier globally unique and well-structured? (`gts.<vendor>.<package>.<namespace>.<type>.v<N>~`)
- [ ] Does the vendor segment use `cf` for Cyber Ware types or the correct third-party vendor prefix?
- [ ] Is the package stable and domain-oriented (not `utils` or `misc`)?
- [ ] Is namespace used appropriately? (`_` when unnecessary, meaningful sub-grouping when the domain is complex)
- [ ] Is the type name a singular noun?
- [ ] Is this a base type or derived type, and is that distinction clear in the `type_id`?
- [ ] Does the base type keep the extension field open (`additionalProperties: true` on `payload`/`metadata`/`properties`)?
- [ ] Can wildcard access-control policies be expressed over this naming hierarchy?
- [ ] Is versioning correct? (major for breaking changes, minor for compatible additions)
- [ ] Should this type be marked `x-gts-abstract` (no direct instances) or `x-gts-final` (no extensions)?
- [ ] If JSON Schema is handwritten, does it use Draft-07 (`http://json-schema.org/draft-07/schema#`) and avoid post-Draft-07 keywords such as `$defs`?
- [ ] Do type-reference fields at boundaries validate a full GTS Type Identifier ending with `~`, not just a `gts.` prefix?
- [ ] Do Rust struct fields that carry GTS identifiers use `GtsTypeId` (type refs) or `GtsInstanceId` (instance refs), not `String`?
- [ ] For a new GTS type: does `type_id` end with `~`, and for derived types does `base = ParentStruct` correctly reflect the parent chain?
- [ ] Are instance IDs constructed via `gts_make_instance_id()`, not via string concatenation or `format!`?
- [ ] Does the instance payload omit all schema-only keywords (`x-gts-traits`, `x-gts-traits-schema`, `x-gts-final`, `x-gts-abstract`)?
- [ ] Are `x-gts-traits`, `x-gts-traits-schema`, `x-gts-final`, and `x-gts-abstract` present only in schemas, never in instances?
- [ ] Are GTS constants defined in Rust code, not magic strings scattered across handlers?
- [ ] In new Rust code, are canonical names used (`type_id`, `TYPE_ID`, `GtsTypeId`, `gts_type_id()`), with deprecated `schema_id` aliases avoided unless needed for compatibility?
- [ ] If this PR changes generic or nested derived structs, do generated schema artifacts/tests prove the overlay lands at the correct full nesting path?
- [ ] If `description` was added or changed on a `#[struct_to_gts_schema]` struct, does the checked-in schema artifact reflect the update in the JSON root?
- [ ] If the PR introduces a `Base<serde_json::Value>` heterogeneous carrier, is `try_narrow` used for dispatch and are `NarrowError` variants handled?
- [ ] Should any string-constant discriminator fields use GTS instances instead? (see [section 6.6](#66-gts-well-known-instances-for-constants-and-discriminator-values))
- [ ] Do discriminator schemas carry `properties` that the engine can read instead of `match` arms?
- [ ] Can new values be added to discriminator fields without an API-breaking change?
- [ ] Is the JSON Schema using `$id: "gts://..."` and `$ref: "gts://..."` URI form?
- [ ] Does the DB schema store base fields in indexed columns and extension data in JSONB/TEXT?