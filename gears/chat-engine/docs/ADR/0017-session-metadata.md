Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0017: Session Metadata JSONB for Extensibility


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: JSONB metadata column](#option-1-jsonb-metadata-column)
  - [Option 2: Fixed columns](#option-2-fixed-columns)
  - [Option 3: Metadata table](#option-3-metadata-table)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**Review**: Revisit if metadata query patterns require fixed-schema columns

**ID**: `cpt-cf-chat-engine-adr-session-metadata`

## Context and Problem Statement

Sessions need additional metadata beyond core fields (session_id, client_id, session_type_id). Examples include user-defined titles, tags, custom fields, summaries, or application-specific data. How should Chat Engine store extensible metadata without frequent schema changes?

## Decision Drivers

* Extensibility without schema migrations (add new metadata fields easily)
* Support user-defined titles and tags for organization
* Store session summaries for quick previews
* Enable application-specific custom fields
* Query capabilities for common metadata (title, tags)
* JSON schema flexibility for evolving requirements
* Efficient storage for sparse data
* Index support for frequently queried fields

## Considered Options

* **Option 1: JSONB metadata column** - Single JSONB field storing arbitrary key-value pairs
* **Option 2: Fixed columns** - Add columns for title, tags, summary, etc.
* **Option 3: Metadata table** - Separate key-value table with FK to sessions

## Decision Outcome

Chosen option: "JSONB metadata column", because it enables schema-free extensibility (add metadata without migrations), supports PostgreSQL JSONB indexing (GIN index for tags), provides flexible storage for evolving needs, efficiently handles sparse data, and maintains simple session table schema.

**Validation Strategy**: Session metadata is opaque client-defined JSON; validating it against GTS schemas is a planned direction and is not implemented. This provides database-level flexibility for rapid iteration while maintaining type safety and schema evolution management at the application boundary. Until schema validation exists, clients own the shape of their metadata.

### Consequences

* Good, because add new metadata fields without schema migrations
* Good, because JSONB supports flexible structure (title, tags, summary, custom)
* Good, because PostgreSQL GIN indexes enable efficient metadata queries
* Good, because sparse data efficient (only store present fields)
* Good, because JSON operators for querying (->>, @>, ? for tag search)
* Good, because schema evolution simple (clients add new fields)
* Bad, because no schema enforcement (typos possible: "titel" vs "title")
* Bad, because metadata structure not self-documenting (need external docs)
* Bad, because complex queries less efficient than normalized columns
* Bad, because type validation at application level (not database level)

### Confirmation

Confirmed when session metadata is stored as a JSONB column with a GIN index and stored without schema validation (GTS validation is not implemented).

## Pros and Cons of the Options

### Option 1: JSONB metadata column

Single JSONB field on the sessions table storing arbitrary key-value pairs.

* Good, because new metadata fields require zero schema migrations
* Good, because PostgreSQL GIN indexes enable efficient queries on JSONB content (tags, title)
* Good, because sparse data is storage-efficient (only present fields consume space)
* Good, because JSON operators (`->>`, `@>`, `?`) provide flexible query patterns
* Bad, because no database-level schema enforcement (typos like "titel" go undetected)
* Bad, because metadata structure is not self-documenting without external schema definitions
* Bad, because complex analytical queries on JSONB are less efficient than on normalized columns

### Option 2: Fixed columns

Add dedicated columns (title, tags, summary, etc.) directly to the sessions table.

* Good, because database enforces types and constraints (NOT NULL, length limits) per column
* Good, because schema is self-documenting — column names and types visible in DDL
* Good, because queries on fixed columns are straightforward and well-optimized by the planner
* Bad, because every new metadata field requires a schema migration and deployment
* Bad, because sparse data wastes storage (NULL columns still occupy row overhead)
* Bad, because tightly couples application-specific fields to the core sessions schema
* Bad, because high migration frequency as requirements evolve across different integrations

### Option 3: Metadata table

Separate key-value table (session_id FK, key, value) for storing metadata entries.

* Good, because fully normalized — each metadata entry is a distinct row with its own constraints
* Good, because adding new metadata keys requires no schema changes
* Good, because individual key-value pairs can have row-level access control or audit trails
* Bad, because queries spanning multiple keys require multiple JOINs or pivot logic
* Bad, because retrieving full metadata for a session requires aggregation (N rows per session)
* Bad, because value column must use a generic type (TEXT), losing type safety for integers, booleans, arrays
* Bad, because higher storage overhead per entry compared to a single JSONB column

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Sets session metadata (title, tags, custom fields)
* `cpt-cf-chat-engine-component-session-management` - Manages metadata updates

**Requirements**:
* `cpt-cf-chat-engine-fr-search-sessions` - Search includes session metadata (title, tags)
* `cpt-cf-chat-engine-fr-session-summary` - Summary stored in metadata

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-session` - metadata field (JSONB)
* `cpt-cf-chat-engine-dbtable-sessions` - metadata column with GIN index
* HTTP GET /sessions/{id} returns metadata

**Related ADRs**:
* ADR-0009 (Stateless Horizontal Scaling with Database State) - PostgreSQL JSONB support
* ADR-0019 (PostgreSQL Full-Text Search with GIN Indexes) - Full-text search includes metadata fields
