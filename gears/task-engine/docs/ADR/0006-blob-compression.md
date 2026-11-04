---
status: accepted
date: 2026-10-24
description: "Store task payloads as opaque binary blobs, zstd-compressed above a size threshold and marked with a magic prefix plus a codec byte, so reads decompress transparently on every backend and the codec can change later."
---

# Compress JSON Payload Blobs Transparently with zstd and a Codec Header

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
  - [Option 4: Application-level zstd with a format byte](#option-4-application-level-zstd-with-a-format-byte)
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
4. **Application-level zstd with a format byte** — compress blobs with zstd (optionally with a trained dictionary) and prefix them with a version byte that names the codec.

## Decision Outcome

Chosen option: **Option 4 — Application-level zstd with a format byte.**

All payload columns are opaque `Binary` columns (`BYTEA` / `LONGBLOB` / `BLOB`). On write, a value larger than `compression_threshold_bytes` is stored as a 5-byte header — the magic prefix `0xDEADBEEF` (little-endian) and one **codec byte** — followed by the compressed bytes. The first codec is `0x01` = zstd level 1, from the Rust `zstd` crate that the workspace already uses. Smaller values are stored raw. On read, a value that starts with the magic prefix is decoded with the codec its codec byte names; any other value is returned as-is. An unknown codec byte is a data error: the read fails closed and is logged, it is never returned as raw bytes.

A raw value can never look like a header: raw values are canonical JSON, which starts with an ASCII character, while the header starts with byte `0xEF` followed by `0xBE`.

**Why not S2**: S2 is mainly a Go library and has no maintained Rust crate, and the earlier 4-byte header had no codec byte, so any later change of codec would need a data migration.

**PostgreSQL TOAST**: PostgreSQL may try to compress large values again (TOAST). Bytes that zstd already compressed do not shrink further, so TOAST mostly gives up after a short test. The Confirmation benchmark below measures this cost; the design keeps the default column storage unless the benchmark shows a real cost.

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
* the payload size limit is enforced on the uncompressed size;
* a value whose codec byte is unknown fails the read instead of being returned as raw bytes.

Before the first release, a benchmark on real payloads MUST compare zstd level 1 with LZ4 (`lz4_flex`) for ratio and CPU, on PostgreSQL with TOAST compression on and off, and confirm the NFR (≥ 40% smaller, < 1 ms per blob each way). If LZ4 wins clearly, it becomes codec `0x02` with no change to the format.

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
* Bad, because S2 has no maintained Rust crate (it is mainly a Go library).
* Bad, because the 4-byte header has no codec byte, so changing the codec later needs a data migration.
* Bad, because the database cannot inspect or redact fields inside payloads.

**Rejected.**

### Option 4: Application-level zstd with a format byte

Same opaque binary column; the prefix carries a codec id so the codec can change later without rewriting old rows.

* Good, because zstd usually gives a better ratio on JSON than Snappy-family codecs, especially with a dictionary.
* Good, because the Rust `zstd` crate is mature and widely used.
* Good, because a codec byte makes later changes cheap.
* Bad, because it costs more CPU than S2 at higher levels (level 1 is close).
* Good, because small payloads skip compression and the threshold can change without migrating old rows.
* Neutral, because PostgreSQL TOAST may try to compress the values again; the benchmark in Confirmation measures this.
* Bad, because the database cannot inspect or redact fields inside payloads.

**Chosen.**

## More Information

```text
write: len(blob) > threshold ? [EF BE AD DE] + [01] + zstd_encode(blob, level 1) : blob
read : starts_with([EF BE AD DE]) ? decode(codec = byte[4], rest = bytes[5..]) : blob
       unknown codec byte -> error (fail closed)
```

See DESIGN §3.7 Portable JSON Storage Contract and §4.6 Blob Compression.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-nfr-compression` — transparent compression of JSON blob columns.
* `cpt-cf-task-engine-nfr-payload-size` — limits apply to the uncompressed size.
* `cpt-cf-task-engine-nfr-capacity` — smaller payload storage at 500 million tasks.
* `cpt-cf-task-engine-fr-structured-io` — input and output stored as opaque compressed blobs.
* `cpt-cf-task-engine-fr-task-entries` — entry payloads use the same format.
