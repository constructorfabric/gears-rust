---
status: accepted
date: 2026-10-24
description: "Store task payloads as opaque binary blobs, S2-compressed above a size threshold and marked with a 4-byte magic prefix so reads decompress transparently on every backend."
---

# Compress JSON Payload Blobs Transparently with S2 and a Magic Header

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Native JSON columns](#option-1-native-json-columns)
  - [Option 2: Database-level compression](#option-2-database-level-compression)
  - [Option 3: Application-level S2 compression with a magic header](#option-3-application-level-s2-compression-with-a-magic-header)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-blob-compression`

## Context and Problem Statement

Task input, context, output, errors, warnings, entry payloads, event payloads, and resolved wait payloads are JSON documents of up to 64 KB each. At the target capacity (500 million stored tasks) they dominate storage. The engine never queries inside these payloads; it only stores and returns them.

We must decide how to store payloads so that storage stays small, behavior is identical on PostgreSQL, MariaDB/MySQL, and SQLite, and reads stay fast.

## Decision Drivers

* Reduce storage for large payload volumes.
* Identical behavior on every supported database.
* Low CPU cost on the write and read paths.
* Small payloads must not pay a compression overhead.
* Mixed compressed and uncompressed rows must read correctly.

## Considered Options

1. **Native JSON columns** — store payloads as JSONB / JSON columns.
2. **Database-level compression** — rely on TOAST, InnoDB page compression, or similar.
3. **Application-level S2 compression with a magic header** — compress blobs above `compression_threshold_bytes` with S2 (Snappy-compatible) and prefix them with a 4-byte marker.

## Decision Outcome

Chosen option: **Option 3 — Application-level S2 compression with a magic header.**

All payload columns are opaque `Binary` columns (`BYTEA` / `LONGBLOB` / `BLOB`). On write, a value larger than `compression_threshold_bytes` is stored as the magic prefix followed by its S2 encoding; otherwise it is stored raw. On read, a value that starts with the prefix is decompressed; any other value is returned as-is.

The PRD payload size limit (64 KB) is measured on the uncompressed canonical JSON, before compression, so the limit does not depend on how well a payload compresses.

### Consequences

* Storage for payload-heavy tables shrinks, which supports the capacity target.
* Every backend stores and returns payloads the same way.
* Old rows stay readable after a threshold change, because each row carries its own marker.
* Payloads cannot be queried or field-level redacted in SQL; right-to-erasure of one field means rewriting the whole blob or deleting the task.
* Search filters work on indexed columns, tags, and resource references, never on payload content.

### Confirmation

Tests MUST check that:

* values above the threshold are compressed and carry the prefix, and smaller values are stored raw;
* reads round-trip both forms exactly;
* rows written under one threshold read correctly after the threshold changes;
* the payload size limit is enforced on the uncompressed size.

## Pros and Cons of the Options

### Option 1: Native JSON columns

Use the database's own JSON type.

* Good, because the database could query inside payloads.
* Bad, because JSON support and storage differ between PostgreSQL, MySQL, and SQLite.
* Bad, because there is no compression control and storage stays large.
* Bad, because the engine never needs server-side JSON queries.

**Rejected.**

### Option 2: Database-level compression

Leave compression to each backend's storage engine.

* Good, because there is no application code.
* Bad, because it is not available or not equivalent on every backend (SQLite has none).
* Bad, because ratios and behavior vary per deployment.

**Rejected.**

### Option 3: Application-level S2 compression with a magic header

Blobs are stored in `Binary` columns. Values above the threshold (default 1 KiB) are written as the little-endian magic `0xDEADBEEF` followed by S2-encoded bytes; smaller values are written raw. The read path checks the prefix to decide whether to decompress.

* Good, because behavior is identical on every backend.
* Good, because S2 is very fast and gives good ratios on JSON.
* Good, because small payloads skip compression entirely.
* Good, because the threshold can change without migrating old rows.
* Bad, because the database cannot inspect or redact fields inside payloads.

**Chosen.**

## More Information

```text
write: len(blob) > threshold ? [EF BE AD DE] + s2_encode(blob) : blob
read : starts_with([EF BE AD DE]) ? s2_decode(rest) : blob
```

See DESIGN §3.7 Portable JSON Storage Contract and §4.6 S2 Blob Compression.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-nfr-compression` — transparent compression of JSON blob columns.
* `cpt-cf-task-engine-nfr-payload-size` — limits apply to the uncompressed size.
* `cpt-cf-task-engine-nfr-capacity` — smaller payload storage at 500 million tasks.
* `cpt-cf-task-engine-fr-structured-io` — input and output stored as opaque compressed blobs.
* `cpt-cf-task-engine-fr-task-entries` — entry payloads use the same format.
