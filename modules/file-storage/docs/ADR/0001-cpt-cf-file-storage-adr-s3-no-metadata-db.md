---
status: accepted
date: 2026-05-05
supersedes_revisions: [2026-04-23, 2026-04-27]
---
# ADR-0001: Module-Owned SQL Metadata Index Shared by All Backends


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A — No DB, S3 object metadata + owner-prefixed keys](#option-a--no-db-s3-object-metadata--owner-prefixed-keys)
  - [Option B — One SQL metadata schema owned by the FileStorage module, shared by every backend](#option-b--one-sql-metadata-schema-owned-by-the-filestorage-module-shared-by-every-backend)
  - [Option C — Hybrid: S3-only by default, opt-in DB index per backend](#option-c--hybrid-s3-only-by-default-opt-in-db-index-per-backend)
- [More Information](#more-information)
  - [Why "S3-only" matters so much](#why-s3-only-matters-so-much)
  - [Why we nevertheless choose the DB](#why-we-nevertheless-choose-the-db)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-s3-no-metadata-db`

## Context and Problem Statement

The P1 File Storage module ships exactly one backend kind — `s3-compatible` (AWS S3, MinIO, Ceph RGW, Wasabi, GCS S3-compat, or `s3s-fs` running side-by-side as the local-disk recipe). Future phases reserve type-space for `webdav` and similar protocol bindings, but P1 does not deliver them. Across the kinds we expect to support, FileStorage needs queryable metadata to serve `list_files`, `get_file_info`, the presign-first lifecycle (`pending_upload → uploaded` with etag pinning), and every P2/P3 capability on the roadmap (audit, quota, retention, ownership transfer, versioning, usage reporting).

The open question is whether the `s3-compatible` adapter (and the future `webdav` adapter, when it lands) should rely on the backend's native metadata facilities (S3 user-metadata + `ListObjectsV2` prefix scans) or share a SQL metadata store.

A metadata DB makes it trivial to answer queries like "give me all files owned by user X" or "all files with `mime_type=image/png` modified in the last 30 days". Without one, the `s3-compatible` adapter would have to express these queries through the S3 addressing scheme and accept that user-defined metadata (`custom_metadata`, `mime_type`, time ranges) is not natively queryable by S3.

The boundary of this decision is the FileStorage module's metadata layer. The P3 `FileShare` service, which will index shareable links, guest URL ledger entries, and view counters, is explicitly out of scope — shareable-link state is a separate concern owned by a separate module and does not influence this decision.

## Decision Drivers

* **S3 is the expected production storage surface** — across the product footprint (cloud deployments, on-prem installations, managed appliances, edge) the dominant storage dependency we expect to encounter is S3 or an S3-compatible endpoint. Keeping the `s3-compatible` adapter "S3-only" (no auxiliary services required on the deployment) is a very, very large operational advantage: any S3-shaped bucket becomes a working FileStorage backend with zero additional moving parts.
* **Architectural flexibility over time** — FileStorage is not a one-quarter feature; P2/P3 already plan audit trail, quota enforcement, retention policies, event emission, ownership transfer, versioning, and a separate FileShare service. Each of those is far cheaper to deliver with a queryable metadata row than with a bucket-scan-only posture.
* `cpt-cf-file-storage-fr-list-files` — callers list files scoped by `owner_type` (user or tenant), with optional `mime_type`, date range, and custom metadata filters. The listing path must work for S3-backed deployments.
* `cpt-cf-file-storage-principle-atomic-metadata` — readers must never see content of version N with metadata of version N+1; adding a second metadata store reintroduces a dual-write consistency problem that must be explicitly managed by the adapter's coordinator.
* `cpt-cf-file-storage-constraint-static-config-p1` and the P1 Deployment Topology (`cpt-cf-file-storage-topology-p1`) — the module is stateless apart from per-adapter storage; adding a SQL dependency to the S3 adapter turns every S3-only deployment into a S3-plus-SQL deployment.
* `cpt-cf-file-storage-nfr-durability` (RPO = 0, RTO ≤15 min) — S3 backends inherit durability from the object store; a local DB index would have its own RPO/RTO that must be reconciled with the bucket.
* `cpt-cf-file-storage-nfr-scalability` — horizontal scaling is trivially additive when the adapter holds no local state; a DB index introduces a coordination point.

## Considered Options

* Option A — No DB for `s3-compatible`, S3 object metadata + owner-prefixed keys
* Option B — One SQL metadata schema owned by the FileStorage module, shared by every backend
* Option C — Hybrid: S3-only by default, opt-in DB index per backend

## Decision Outcome

Chosen option: **Option B — One SQL metadata schema owned by the FileStorage module, shared by every backend**.

The FileStorage **module** (not any individual adapter) owns the `file_storage` SQL schema. Every backend that needs queryable metadata — `s3-compatible` in P1, future `webdav` and similar protocol bindings as they land — writes into the same schema in the same database; rows are discriminated by `backend_id` (see DESIGN §3.7). For S3-class backends, S3 remains the authoritative store for the bytes; a small subset of metadata (`Content-Type`, `Content-Disposition`, every `x-amz-meta-<k>=<v>`) lives on the object too, mirrored explicitly through the upload presign + `CopyObject self-copy` paths so the bucket alone can reconstruct what the file is in a DR scenario. **`gts_file_type` is the explicit exception** — it lives ONLY in the SQL row and is never written to S3, because it is the resource type used for authz decisions. The SQL row is the authoritative store for everything queryable (ownership lookups, `mime_type` filters, date ranges, `custom_metadata` keys, GTS file type scans). Listings and filters are served from the index; content operations go to the backend. The upload coordinator writes the index row first (`status = pending_upload`), issues the presigned PUT, and flips the row to `uploaded` only after `reconcile` HEADs the backend and confirms the object — this single contract works uniformly across all backend kinds and keeps the "readers never see N/N+1 skew" invariant intact.

This decision is taken despite the significant operational attraction of Option A ("S3-only dependency"). See [Why "S3-only" matters so much](#why-s3-only-matters-so-much) and [Why we nevertheless choose the DB](#why-we-nevertheless-choose-the-db) for the full rationale — in short: every P2/P3 capability on the roadmap either costs us 10× more to build on top of a bucket-scan substrate, or is outright infeasible without the index.

### Consequences

* Good, because all listing and filter queries — mandatory owner scoping, optional `mime_type`, date range, `custom_metadata` — become single-round-trip indexed SQL, not `ListObjectsV2` paginated scans with client-side post-filtering.
* Good, because the P2/P3 roadmap (ownership transfer, retention policies, quota enforcement, audit trail, usage reporting, event emission, versioning) becomes incremental column/row work rather than a re-platforming exercise.
* Good, because ownership transfer (`cpt-cf-file-storage-fr-ownership-transfer`, deferred P2) becomes a single `UPDATE` instead of an S3 copy-then-delete dance.
* Good, because the listing code path unifies across all adapters — one SQL query-planner, one test harness, one set of migration patterns, instead of two listing strategies (SQL for `local`, `ListObjectsV2`+client-filter for `s3-compatible`).
* Good, because opaque/uuid physical keys become possible on S3 (see ADR-0002 for the addressing-scheme decision that stacks on top of this one).
* Good, because cross-tenant analytics and operational queries ("total bytes per tenant", "files uploaded this hour") are native SQL.
* Bad, because introduces a dual-write between S3 and the SQL index on every mutation. The upload coordinator must own the reconciliation path (row-first, S3-second, row-finalize) and a background reconciliation sweep must exist to detect and repair drift.
* Bad, because "S3-only dependency" is sacrificed: an `s3-compatible` deployment now also requires operating a SQL-compatible database. This is the single largest cost of this decision and is flagged again in the next section.
* Bad, because durability and availability of the backend become the minimum of S3 and the SQL store; the SQL store's RPO/RTO must be defined, measured, and reconciled with the bucket.
* Bad, because horizontal scaling is no longer coordination-free — write serialisation on the coordinator path is bounded by the SQL store's write throughput.
* Bad, because an adversarially-populated bucket (objects present in S3 but absent from the index) silently becomes unreachable through FileStorage until the reconciliation sweep catches it — the bucket is no longer the single source of truth.

### Confirmation

* The FileStorage module declares the ModKit `DatabaseCapability` exactly once (at the module level, not per adapter) and ships a single reference migration that creates the `file_storage.files` table (see `migration.sql`). All backend adapters reuse the same connection pool through the `Files Repo` component (DESIGN §3.2).
* Integration test: `list_files(owner_type=user, owner_id=X)` is served by a single indexed SQL query with no `ListObjectsV2` calls on the hot path, regardless of which backend kind owns the rows.
* Integration test: optional filter by `mime_type` is served by the same SQL query without any post-filter fan-out.
* Integration test: killing the module between the `pending_upload` row insert and the backend ack leaves a recoverable state — the row's `upload_expires_at` ensures the P2 GC sweep reclaims it, and no reader observes N/N+1 skew because only `status = uploaded` rows are visible through the partial unique index.
* Deployment check: booting the FileStorage module without a SQL connection string fails fast with a clear error pointing at the missing `DatabaseCapability` — not a silent fallback. There is no operational mode in which an `s3-compatible` backend is deployed without the module DB.

## Pros and Cons of the Options

### Option A — No DB, S3 object metadata + owner-prefixed keys

All `FileMetadata` fields are written to S3 user-metadata (and system-metadata) atomically with the object. Physical S3 keys encode ownership as `{tenant_id}/{owner_kind}/{owner_id}/{file_path}`. Listings are `ListObjectsV2` prefix scans; optional filters are applied by the adapter after the scan.

* Good, because single source of truth — no dual-write, no reconciliation job.
* Good, **because "S3-only dependency"** — one of the largest operational advantages any adapter can have in our product context. Most storage endpoints we expect to encounter are already S3-compatible; a backend that needs nothing but an S3 bucket drops straight into any such deployment with zero infrastructure ask. This is the single most compelling reason this option exists.
* Good, because inherits S3 durability, lifecycle, replication, and encryption.
* Good, because horizontal scaling is additive — the adapter is stateless.
* Good, because owner-scoped listing — the one mandatory filter axis — is a single prefix scan.
* Bad, because optional filters (`mime_type`, date, custom metadata) are applied client-side after the scan; listing cost scales with the prefix, not with the result count.
* Bad, because ownership transfer is not addressable without rewriting the object key.
* Bad, because cross-tenant or cross-owner search (not required in P1, but repeatedly implied by P2 audit/quota/usage requirements) is infeasible without a full bucket scan.
* Bad, because every P2/P3 capability that wants "all files matching predicate P" — audit, quota, retention, versioning — has to either accept `ListObjectsV2` cost or re-introduce a DB at that point, which is exactly this ADR's debate deferred.

### Option B — One SQL metadata schema owned by the FileStorage module, shared by every backend

The FileStorage module owns one SQL schema (`file_storage`) that every backend writes into; rows are discriminated by `backend_id`. File bytes for `s3-compatible` live in S3; bytes for the future `webdav` will live on the remote server — but every metadata row goes into the same shared module DB. Listings and filters are served from the DB; content operations go to the backend. Local-disk deployments use `s3s-fs` registered as a regular `s3-compatible` backend; FileStorage has no native POSIX adapter.

* Good, because rich queries — any indexed combination of `(tenant_id, owner, mime_type, gts_file_type, created_at, custom_metadata)` is native SQL.
* Good, because ownership transfer is a single `UPDATE`.
* Good, because cross-tenant / analytical / operational queries are feasible.
* Good, because unifies the listing path across all backend kinds — one SQL query-planner instead of one strategy per protocol.
* Good, because opens the door to ADR-0002 Option B/C (opaque UUID addressing), which in turn closes several URL-leakage and URL-stability problems that Option A cannot solve.
* Good, because P2/P3 roadmap items (audit trail, usage reporting, quota, retention, versioning, ownership transfer) become incremental schema work rather than re-platforming.
* Bad, because introduces a dual-write between S3 and the SQL store; every write path needs an explicit reconciliation strategy (row-first coordinator + background sweep) to avoid drift.
* Bad, because **sacrifices "S3-only dependency"** — an `s3-compatible` deployment now also requires operating a SQL-compatible database. This is the single largest cost of Option B and is explicitly called out here because it cuts against our default preference for minimal-dependency adapters.
* Bad, because the "Atomic Metadata-Content Coupling" principle (DESIGN §2.1) must be re-derived for S3, since the two stores can be written out of order — effectively the same problem the `local`-backend coordinator solves, but now across a network boundary.
* Bad, because durability guarantees become the minimum of S3 and the SQL store; RPO/RTO for the DB index must be established separately.

### Option C — Hybrid: S3-only by default, opt-in DB index per backend

Declare an additional backend capability (e.g., `metadata_index = true`) that, when enabled in TOML, wires a SQL index for that backend. When disabled, the adapter behaves exactly like Option A.

* Good, because deployments that genuinely need rich filtering can enable it without forcing it on everyone.
* Good, because preserves the lightweight S3-only profile for deployments that don't need rich filters.
* Good, because allows Option B's capabilities to be adopted incrementally — first as a feature flag, then as the default if demand emerges.
* Bad, because doubles the number of listing code paths the module must maintain and test (prefix-scan vs SQL-backed), widening the surface area for capability-boundary bugs.
* Bad, because `fr-list-files` semantics diverge per deployment — the same API call returns different effective filter expressiveness depending on the backend flag, which leaks backend internals into caller expectations.
* Bad, because the dual-write reconciliation problem from Option B reappears whenever the flag is on, without a corresponding simplification elsewhere.
* Bad, because P2/P3 capabilities that rely on metadata queries (audit, quota, retention, versioning) must either ship only for flag-on deployments or ship two implementations — defeating much of the per-deployment flexibility gain.

## More Information

### Why "S3-only" matters so much

The product footprint skews heavily towards S3-compatible object stores: AWS S3 itself, MinIO for on-prem and dev, Ceph RGW in private-cloud installations, Wasabi for cold tiers, GCS via its S3-compatibility shim, and S3-compatible appliances in edge/air-gapped deployments. In every one of these environments, the difference between "FileStorage needs an S3 bucket" and "FileStorage needs an S3 bucket **and** a SQL database" is material — it is an additional operational dependency that must be provisioned, secured, monitored, backed up, patched, HA-configured, and disaster-recovered independently of the bucket. Option A's big win is that it collapses this to a single operational dependency: the bucket *is* the backend.

Every bullet of the "Good" column of Option A is a real, non-marginal benefit, and this ADR does not wave them away. They are the reason Option A exists as a serious contender rather than a straw man.

### Why we nevertheless choose the DB

Despite the very real strength of Option A, we choose Option B because FileStorage has a long roadmap whose capabilities are each much easier — and in several cases, only feasible — on top of a metadata index:

- **Listing with non-prefix filters** (`fr-list-files` optional filters: `mime_type`, date range, `custom_metadata`) — on Option A, every such listing is a paginated `ListObjectsV2` scan with client-side post-filtering; on Option B, it is one indexed query.
- **Ownership transfer** (`fr-ownership-transfer`, P2) — on Option A, requires an S3 copy-to-new-key + delete-old-key dance and rewrites every pre-existing URL; on Option B, a single `UPDATE`.
- **Audit trail** (`fr-audit-trail`, P2), **usage reporting** (`fr-usage-reporting`, P2), **quota enforcement** (`fr-storage-quota`, P2) — all want "all files matching predicate P"; on Option A, either full bucket scans or a DB introduced *then*; on Option B, straightforward.
- **Retention policies** (`fr-retention-policies`, P2) and **file versioning** (`fr-file-versioning`, P3) — both naturally modelled as rows with lifecycle columns; on Option A, encoded into key naming and bucket lifecycle rules, with much tighter coupling between policy and physical layout.
- **URL opacity** (ADR-0002 Option B/C) — not possible on Option A, which requires the logical path to be encoded in the S3 key and therefore visible in every URL and every presigned URL.

The combined P2/P3 cost of working around "no index" is, by a comfortable margin, larger than the one-time and ongoing operational cost of owning a SQL schema per `s3-compatible` backend. The SQL schema itself is deliberately small (three tables, application-generated UUIDs, portable SQL — see `migration.sql`) so the operational burden does not grow with the feature roadmap.

We do not adopt Option C because the Hybrid mode doubles the implementation surface and makes API behaviour diverge per deployment — two things we pay for permanently to avoid committing to one architectural choice. Under Option B we instead keep Option A available as a **future** decision triggered by explicit evidence (see revisit triggers below), rather than as a permanently parallel path.

Revisit triggers — this decision should be reopened (superseded by a new ADR) if any of the following hold:

- A class of deployments emerges where operating a SQL-compatible database alongside S3 is truly infeasible (genuine edge / air-gapped / appliance-class), **and** that deployment class is required to run the full FileStorage feature set — not a trimmed one.
- The coordinator dual-write reconciliation proves unstable in production to a degree that the operational cost of keeping DB+S3 consistent exceeds the engineering cost of the bucket-scan alternative.
- An S3 capability (e.g., native metadata search or a supported secondary index) becomes broadly available across the S3-compatible implementations we support, closing the "optional filters require client-side scan" gap at the backend level.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related ADR**: [ADR-0002](./0002-cpt-cf-file-storage-adr-opaque-file-ids.md) — external URL / S3 key addressing scheme, which is enabled by this decision.

This decision directly addresses the following requirements and design elements:

* `cpt-cf-file-storage-fr-list-files` — owner scoping and optional filters (`mime_type`, date range, `custom_metadata`) are served by indexed SQL on the adapter-owned metadata table.
* `cpt-cf-file-storage-fr-file-ownership` — owner is captured in the metadata row; S3 user-metadata mirrors it for disaster-recovery reconstruction.
* `cpt-cf-file-storage-fr-metadata-storage` — the SQL row is the authoritative metadata store for queryable fields; S3 user-metadata mirrors it.
* `cpt-cf-file-storage-principle-atomic-metadata` — upheld by the same `pending_upload → uploaded` coordinator contract used uniformly across all backend kinds, anchored on etag pinning.
* `cpt-cf-file-storage-constraint-static-config-p1` — the SQL dependency is declared statically in TOML alongside the backend, no runtime reconfiguration in P1.
* `cpt-cf-file-storage-component-s3-backend` — adapter consumes the FileStorage-module-owned schema through the `Files Repo` component; the adapter itself does not declare a `DatabaseCapability` because the module already does.
* `cpt-cf-file-storage-component-files-repo` — the typed interface to the shared `file_storage.files` table; introduced by this decision so SQL access does not leak into adapters or the SDK facade.
