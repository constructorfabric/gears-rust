---
status: accepted
date: 2026-07-28
decision-makers: Constructor Fabric steering committee
---

# Namespace Gear-Owned Database Objects with Stable Portable Prefixes


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Scope](#decision-scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [ToolKit Enforcement and Gear Author Responsibilities](#toolkit-enforcement-and-gear-author-responsibilities)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [Quality Attribute Impact](#quality-attribute-impact)
  - [Review and Supersession](#review-and-supersession)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Stable Gear Database Namespace with a Double-Underscore Separator](#stable-gear-database-namespace-with-a-double-underscore-separator)
  - [Canonical Gear Name with a Single-Underscore Separator](#canonical-gear-name-with-a-single-underscore-separator)
  - [Native Database Isolation Only](#native-database-isolation-only)
  - [Continue with Ad-Hoc Names and Detect Collisions During Migration](#continue-with-ad-hoc-names-and-detect-collisions-during-migration)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-database-adr-object-namespacing`

## Context and Problem Statement

ToolKit can run multiple gears against the same PostgreSQL schema, MySQL database, or SQLite file. Although the migration runner gives each gear an isolated migration history table, the application objects created by those migrations still share the database namespace.

Table naming is currently inconsistent. Some gears use a gear-related prefix, some rely on a PostgreSQL schema, and others use generic names such as `messages`, `settings`, `files`, or `policies`. There is already a concrete collision: both `mini-chat` and `chat-engine` define incompatible `messages` and `message_reactions` tables.

How should gear-owned database objects be named so independently developed gears can safely coexist in a shared database namespace on every supported database backend?

The conflict is present in currently registrable gears, so new database objects must stop extending the inconsistency immediately. Renaming existing objects can proceed in backend-tested phases rather than as one repository-wide cutover.

## Decision Scope

This decision applies to ToolKit gears and plugins that create persistent database objects, reusable ToolKit database facilities that create objects on their behalf, and the migration/runtime metadata needed to enforce ownership. The affected stakeholders are gear authors, ToolKit maintainers, database operators, and release engineers responsible for upgrades.

The decision governs physical object names that gear migrations or ToolKit libraries can explicitly select, and the migration of those objects. It does not define table schemas, data ownership rules, authorization policy, connection routing, backup policy, or a requirement that all deployments share a database. Deployments may still provide a separate schema, database, or file per gear.

Database backends may create internal supporting objects whose names or catalog representation cannot be selected portably, such as primary-key indexes, implicit unique indexes, or auto-increment machinery. These objects inherit ownership from the gear-controlled table or object that caused their creation, but their backend-generated names are outside this convention and are not required to match its grammar.

The decision assumes gear-owned objects are internal implementation details and that ToolKit controls their migration lifecycle. Objects in externally managed or third-party schemas are outside this ADR and require a documented exception rather than being renamed by a gear.

The per-gear migration history tables created by the migration runner, currently named `toolkit_migrations__<gear>__<hash8>`, predate this convention and do not conform to it. They are runtime bookkeeping rather than gear application objects, and renaming them cannot use the migration framework that they themselves record. Bringing them into conformance, or deciding that they stay exempt, is separate work and is not settled here.

## Decision Drivers

The drivers are listed in priority order:

* **Collision avoidance** — independently developed gears must not claim the same table, index, sequence, trigger, view, or function name.
* **Backend portability** — the convention must work in a single PostgreSQL schema, a single MySQL database, and a single SQLite file.
* **Stable storage identity** — renaming a gear must not implicitly rename its persisted data.
* **Operational clarity** — operators must be able to identify the owner of a gear-controlled database object from its name.
* **Determinism** — gear-controlled object names must not depend on gear initialization order, connection `search_path`, or database-specific implicit naming.
* **Migration safety** — existing installations need an explicit and diagnosable upgrade path.
* **Identifier portability** — names must fit the strictest supported identifier limit and avoid backend-specific case and quoting behavior.
* **Low authoring overhead** — the common naming path should be generated or validated by ToolKit rather than repeatedly reimplemented by every gear.

## Considered Options

* Stable gear database namespace with a double-underscore separator
* Canonical gear name with a single-underscore separator
* Native database isolation only
* Continue with ad-hoc names and detect collisions during migration

## Decision Outcome

Chosen option: **Stable gear database namespace with a double-underscore separator**, because it is the only option that provides deterministic, portable object isolation while preserving readable ownership and allowing multiple gears to share one database namespace.

Every gear that owns persistent database objects must explicitly declare a stable `db_namespace`. The namespace normally matches the canonical gear name converted to lowercase snake case, but it is not implicitly derived because a later gear rename must not change persisted storage identity. Explicit declaration also permits a shorter stable storage alias when a long gear name would consume too much of the 63-byte identifier budget. Changing `db_namespace` requires an explicit database migration.

For example:

| Gear name | Stable `db_namespace` |
|---|---|
| `file-storage` | `file_storage` |
| `mini-chat` | `mini_chat` |
| `chat-engine` | `chat_engine` |
| `account-management` | `account_management` |
| `bss-ledger` | `bss_ledger` |
| `timescaledb-usage-collector-plugin` | `usage_tsdb` |

Gear-owned database objects whose physical names can be explicitly selected use the following grammar:

```text
table       = <db_namespace>__<local_table_name>
index       = idx_<table>__<purpose>
unique      = uq_<table>__<purpose>
foreign_key = fk_<table>__<purpose>
check       = ck_<table>__<purpose>
trigger     = trg_<table>__<purpose>
sequence    = seq_<table>__<purpose>
view        = view_<db_namespace>__<local_name>
function    = fn_<db_namespace>__<local_name>
```

Table examples:

```text
file_storage__files
file_storage__file_versions
mini_chat__messages
chat_engine__messages
account_management__tenants
bss_ledger__journal_entry
```

The double underscore is a reserved ownership boundary in the naming patterns above. Because namespaces, local names, and purposes cannot contain `__`, tooling can parse each pattern and attribute an object to exactly one registered gear. A single underscore would require a global invariant that no namespace extends another at an `_` boundary; otherwise `mini_chat_%` also matches objects owned by `mini_chat_pro`. Such an invariant cannot be enforced for out-of-tree gears without a global registry.

The cost is that a dropped separator in hand-written SQL produces a valid but unowned identifier. Dylint rejects such identifiers when they are statically recognizable in Rust code; dynamically constructed identifiers and raw SQL that the lint cannot recognize require code review. Shared-database migration tests provide backend execution coverage and may expose collisions as DDL failures, but they are not a complete naming-conformance check and do not treat backend-generated internal objects as convention violations.

Explicitly named supporting objects carry a type marker, the full name of the table they belong to, and a purpose. For example:

```text
idx_file_storage__files__tenant_owner
uq_mini_chat__messages__chat_request_role
fk_chat_engine__messages__session
```

The first double underscore marks the ownership boundary. For explicitly named table-bound supporting objects, the second double underscore separates the full table name from the purpose, making the name unambiguous for humans and tooling. When the physical name of a supporting object can be selected portably across the supported backends, the gear author or ToolKit helper must supply a name that follows this grammar. Backend-generated names that cannot be selected portably, such as `<table_name>_pkey`, `<table_name>_<column>_key`, `<table_name>_<column>_seq`, MySQL's `PRIMARY`, or SQLite's `sqlite_autoindex_<table>_<n>`, are outside the grammar and Dylint's validation scope. Tooling rejects an overlong gear-controlled physical identifier before migration execution rather than relying on database-side truncation; collision avoidance follows from the unique `db_namespace` and the grammar above.

The following identifier rules apply:

* Names are lowercase ASCII and use only `[a-z0-9_]`.
* A `db_namespace` starts with a letter and must not contain `__`.
* Local names and purposes must not contain `__`.
* `toolkit` is reserved for database objects owned by the ToolKit runtime.
* Every complete gear-controlled identifier must be at most 63 bytes.
* Tooling must reject an overlong gear-controlled identifier; database-side implicit truncation is not permitted for names governed by this convention.
* SQL identifiers are not quoted merely to preserve case or punctuation.

Columns do not need the gear namespace because their containing table already provides it.

### ToolKit Enforcement and Gear Author Responsibilities

The convention is enforced by ToolKit and CI rather than relying only on gear-author discipline.

The intended gear declaration is explicit:

```rust
#[toolkit::gear(
    name = "file-storage",
    db_namespace = "file_storage",
    capabilities = [db, rest]
)]
pub struct FileStorageGear;
```

The corresponding SeaORM entity uses the complete physical table name:

```rust
#[sea_orm(table_name = "file_storage__files")]
```

* The gear author chooses meaningful local object names such as `files`, `messages`, or `tenant_owner`, explicitly declares the stable `db_namespace`, and uses the resulting complete names in entity metadata and migrations. A long canonical gear name may use a shorter globally unique storage alias such as `usage_tsdb`; the alias becomes compatibility-sensitive after persistence.
* The gear macro exposes `db_namespace` as stable gear metadata and validates its syntax and length. Scaffolding may suggest the normalized gear name, but compilation must not silently derive a namespace for a database-capable gear.
* The runtime rejects duplicate `db_namespace` values among registered database-capable gears before applying any gear migration.
* `toolkit-db` provides validated helpers for constructing table, index, constraint, sequence, trigger, view, and function names from a namespace and local name. Reusable database libraries accept the caller's namespace instead of claiming generic global names.
* Where SeaORM or another macro requires a string literal, the gear author writes the complete physical name and a custom Dylint rule verifies that it starts with the owning gear's `<db_namespace>__` prefix.
* Raw migration SQL remains explicit because ToolKit cannot safely rewrite arbitrary SQL containing foreign keys, indexes, triggers, functions, or backend-specific syntax. Gear authors must use the names produced by the ToolKit convention for identifiers they control; Dylint checks statically recognizable names and patterns, while code review covers identifiers the lint cannot recognize.
* CI applies compatible gear migrations to a shared database namespace for backend execution coverage, including collisions that surface as DDL failures. It is not a complete catalog-conformance check and does not require backend-generated internal objects to match the portable naming grammar.

ToolKit must not silently add prefixes to arbitrary SQL at runtime. Hidden rewriting could make migration DDL disagree with static SeaORM entity metadata and would make the resulting physical schema difficult to predict.

PostgreSQL schemas or separate databases remain valid optional deployment isolation mechanisms, but they do not replace the portable name prefix. A gear must use its prefixed object names even when deployed into a dedicated PostgreSQL schema so the same migrations and entities remain safe in SQLite and other flat namespaces.

A reusable database facility is namespaced by the owner of its data, not by the crate that supplies its implementation. If the facility stores per-consumer data, its objects belong to the consumer's `db_namespace`, and the facility must accept and validate a caller-supplied namespace instead of claiming a generic global name; `toolkit_outbox` and the shared coordination-lease tables are the current examples. If it stores genuinely cross-consumer data, it may use the reserved `toolkit` namespace as a platform singleton, but that choice and its sharing semantics must be documented at the facility.

This convention provides name isolation and ownership visibility. It is not a security boundary: permissions, credentials, and access controls remain responsible for preventing unauthorized cross-gear database access.

### Consequences

* ToolKit gear metadata must expose a stable `db_namespace` for every gear with database capability.
* The runtime must reject duplicate `db_namespace` values before applying migrations.
* SeaORM `table_name` declarations, migration DDL, raw migration SQL, explicitly named indexes, constraints, triggers, functions, views, operational queries, and documentation must agree on the prefixed names.
* New gear-controlled database object names must comply immediately after this ADR is accepted.
* Existing unprefixed objects require forward rename migrations. Previously released migrations remain immutable; they must not be edited to pretend the old names never existed.
* A rename migration must update or recreate explicitly named indexes, constraints, triggers, sequences, and functions where the backend does not rename them automatically.
* A migration from a generic legacy name must verify the expected legacy table shape before renaming it. If ownership is ambiguous, startup must fail with a diagnostic instead of adopting or overwriting the table.
* `mini-chat` and `chat-engine` can coexist after their conflicting tables and supporting objects are migrated into their respective namespaces.
* Gear-controlled database object names become compatibility-sensitive. Changing a `db_namespace` requires an explicit, reviewed migration.
* PostgreSQL-only schema isolation such as `bss` may remain as defense in depth, but names inside that schema must follow this convention.
* Cross-gear access continues through SDK contracts and ClientHub, not by querying another gear's tables directly.
* Renames can require schema locks and must be scheduled according to the affected backend and table size; the convention itself adds no query-time join, lookup, or serialization overhead.
* Operational SQL, dashboards, backup/restore selections, and support procedures that refer to old names must move with each rename migration.
* A reverse rename is technically possible only while the old name remains free and no incompatible consumer has been introduced. Each gear migration must therefore define its own tested rollback or forward-recovery policy.

### Confirmation

Compliance is confirmed through all of the following:

* The gear macro validates the `db_namespace` syntax and identifier length.
* Runtime registration rejects duplicate database namespaces before the first gear migration runs.
* A custom Dylint rule checks that SeaORM `table_name` values begin with the owning gear's `<db_namespace>__` prefix; CI executes it through `make dylint`, and `make dylint-test` runs its UI tests.
* Migration tests apply all compatible registered gears to one fresh SQLite file and, where supported, one shared PostgreSQL schema and MySQL database.
* Shared-database migration tests detect backend-specific DDL failures, including collisions that the backend reports while applying migrations; they do not enumerate every catalog object or validate backend-generated internal names.
* Code review verifies gear-controlled identifiers in raw migration SQL that Dylint cannot recognize and explicitly allows only documented framework-owned or externally managed object exceptions.

### Quality Attribute Impact

* **Performance** — not materially affected during steady-state operation; rollout may take DDL locks and must be evaluated per backend and table. Runtime benchmarks and load-test changes are not required because generated SQL resolves the same number of objects with longer identifiers.
* **Security** — authentication, authorization, credentials, sessions, attack surface, and data protection are unchanged. Prefixes improve ownership visibility but do not grant isolation or replace database privileges and ToolKit secure access. A penetration test is not required for a physical naming change.
* **Reliability** — steady-state availability, service-level objectives, and failure topology are unchanged. Rename migrations introduce a controlled deployment risk; migration failure must leave the old schema recoverable and must be visible through existing migration failure reporting.
* **Data** — logical models, classification, retention, privacy, consistency, and data volume are unchanged. Physical names and catalog metadata change; backups, legacy shape checks, and backend-specific rename tests protect data integrity during the transition.
* **Integration** — public SDK and REST contracts are unchanged. Direct cross-gear SQL is unsupported; operator integrations using physical names require coordinated updates.
* **Operations** — no steady-state infrastructure, configuration, logging, or monitoring cost is added. Inspection and incident response become clearer because ownership is encoded in every gear-controlled object name; rollout runbooks and database-object dashboards must be updated with each migration.
* **Maintainability** — centralized generation and the custom Dylint rule add ToolKit work but remove per-gear naming ambiguity and manual collision coordination. Gear-author documentation must teach the namespace rule.
* **Testing** — new-object linting and shared-database migration tests become mandatory. Each legacy rename must pass supported-backend upgrade and recovery tests before release; no feature flag or user-facing canary applies to this catalog-level change.
* **Compliance** — The naming convention does not change data content, processing purpose, classification, retention, or residency requirements. Each rollout owner must assess and update database grants, backup/restore selectors, audit tooling, retention inventories, and data catalogs affected by physical renames, and obtain privacy or legal review when required by organizational policy or the assessment outcome.
* **User experience** — no user workflow, training, or user communication changes. Gear-author and operator documentation is the only affected usability surface.
* **Business** — the convention enables more gears to be packaged together without requiring separate databases. It adds implementation and migration work for maintainers, accepted in exchange for avoiding deployment failures and per-gear database infrastructure.

### Review and Supersession

Review this decision if ToolKit drops flat-namespace backends, adopts mandatory physical database isolation, adds a backend with an identifier limit below 63 bytes, or finds that the namespace convention cannot represent a required class of database object.

An accepted ADR is not edited to change the convention retrospectively. A future incompatible decision must supersede this ADR with a new record and include migration guidance. This ADR does not supersede an earlier cross-cutting database naming decision.

## Pros and Cons of the Options

### Stable Gear Database Namespace with a Double-Underscore Separator

Each gear declares a stable storage namespace and prefixes every owned database object whose physical name it can select with an unambiguous `__` boundary.

* Good, because it prevents cross-gear name collisions in flat namespaces.
* Good, because it works consistently across PostgreSQL, MySQL, and SQLite.
* Good, because ownership is visible on gear-controlled objects during database inspection and incident response.
* Good, because storage identity is decoupled from later product or gear renames.
* Good, because `__` makes ownership attribution exact for names governed by the convention, so lints and operator queries identify the owner without a namespace registry.
* Neutral, because PostgreSQL deployments may use both a schema and a prefix.
* Bad, because gear-controlled object names become longer and sometimes require shorter local purpose names to remain within 63 bytes.
* Bad, because a dropped separator in dynamically constructed or otherwise unrecognized raw SQL can evade Dylint and requires code review; shared-database migration tests catch it only if the resulting name causes a DDL failure.
* Bad, because existing installations need coordinated rename migrations.

### Canonical Gear Name with a Single-Underscore Separator

Derive names directly as `<normalized_gear_name>_<local_name>`, for example `mini_chat_messages`.

* Good, because names are shorter, are harder to mistype, and match established framework conventions such as Django app labels and Rails engine prefixes.
* Bad, because ownership attribution stops being local: `mini_chat_%` also matches a `mini_chat_pro` namespace, and exactness can only be restored by a global no-nested-prefix invariant that out-of-tree gears cannot be held to.
* Bad, because the residual `foo_bar_users` ambiguity produces a silent collision rather than a diagnosable failure.
* Bad, because automatically following a gear rename couples product naming to physical storage compatibility.
* Bad, because it does not define ownership for indexes, sequences, triggers, views, or functions.

### Native Database Isolation Only

Give every gear its own PostgreSQL schema, MySQL database, or SQLite file and allow local object names inside it.

* Good, because it provides stronger operational and permission isolation.
* Good, because local names remain short.
* Bad, because the mechanisms are not equivalent across the supported backends.
* Bad, because it does not satisfy deployments that deliberately use one PostgreSQL schema, one MySQL database, or one SQLite file.
* Bad, because SeaORM entities and raw migrations would need backend-specific qualification behavior.
* Bad, because shared transactions and simple local development become harder.

### Continue with Ad-Hoc Names and Detect Collisions During Migration

Keep current names and rely on `CREATE ... IF NOT EXISTS`, migration ordering, or a preflight inventory to detect duplicates.

* Good, because it requires no immediate migration of existing tables.
* Good, because individual gear authors retain complete naming freedom.
* Bad, because collision detection occurs only after independently developed gears are combined.
* Bad, because `IF NOT EXISTS` can temporarily hide incompatible ownership and fail later during index creation or ORM queries.
* Bad, because generic names do not communicate ownership to operators.
* Bad, because a collision registry becomes a centralized manual process without establishing a durable naming rule.

## More Information

* ToolKit currently isolates only per-gear migration history tables: [migration runner](../../../../libs/toolkit-db/src/migration_runner.rs).
* ToolKit database execution and migration patterns: [database patterns](../../../toolkit_unified_system/11_database_patterns.md).
* Project-specific architectural rules and their validation commands: [architecture lints and validation commands](../../../../CONTRIBUTING.md).
* The reusable outbox already supports caller-provided prefixes: [outbox migrations](../../../../libs/toolkit-db/src/outbox/migrations.rs).
* Existing incompatible `messages` entities: [Chat Engine](../../../../gears/chat-engine/chat-engine/src/infra/db/entity/message.rs) and [Mini Chat](../../../../gears/mini-chat/mini-chat/src/infra/db/entity/message.rs).
* File Storage documents why its runtime migrations currently use a flat namespace: [File Storage initial migration](../../../../gears/file-storage/file-storage/src/infra/storage/migrations/m20260624_000001_p1_initial.rs).
* BSS Ledger demonstrates optional PostgreSQL schema isolation with a flat SQLite fallback: [BSS schema migration](../../../../gears/bss/ledger/ledger/src/infra/storage/migrations/m20260619_000001_create_bss_schema.rs).

## Traceability

No dedicated cross-cutting database PRD or DESIGN artifact exists at the time of this decision. The authoritative related design guidance is the [ToolKit database patterns](../../../toolkit_unified_system/11_database_patterns.md); this ADR adds the naming rationale and constraints that guidance must adopt.

This decision applies to every implementation of ToolKit `DatabaseCapability`, every gear or plugin that independently migrates its own storage, and reusable ToolKit database facilities that create persistent objects on behalf of a gear.

It extends the database migration invariant from isolated migration histories to isolated application-object names while preserving the existing secure-by-default database access model.
