# FileStorage

Universal file storage and management service for the Gears middleware.

## Overview

FileStorage provides upload, download, metadata management, access control, and sharing capabilities for all platform
gears and users. It replaces ad-hoc per-gear file handling with a centralized, tenant-aware storage service.

### Key Capabilities

- **File operations** — upload, download, delete, list with rich metadata
- **Pluggable backends** — S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV, local filesystem
- **Access control** — tenant-scoped ownership, GTS file type classification, Authorization Service integration
- **Sharing** — shareable links (public/tenant/hierarchy scopes), signed URLs, direct transfer URLs
- **Access interfaces** — REST API, S3-compatible API, WebDAV API
- **Policies** — file type restrictions, size limits, sharing model restrictions, storage quotas
- **Lifecycle** — file versioning, retention policies, multipart upload, conditional requests (ETags)
- **Audit** — write operation audit trail, optional read audit logging

### Actors

| Actor               | Description                                                                   |
|---------------------|-------------------------------------------------------------------------------|
| Platform User       | Authenticated user managing files via UI or API                               |
| CF/Gears | Any gear requiring file operations (e.g., LLM Gateway, document management) |

### Dependencies

| Dependency            | Criticality |
|-----------------------|-------------|
| ToolKit Framework      | p1          |
| Authorization Service | p1          |
| Audit Infrastructure  | p2          |
| Usage Collector       | p2          |
| Quota Enforcement     | p2          |
| EventBroker           | p2          |
| Serverless Runtime    | p2          |

## Documentation

- [PRD.md](docs/PRD.md) — Product requirements document
- [DESIGN.md](docs/DESIGN.md) — Architecture and design
- [DECOMPOSITION.md](docs/DECOMPOSITION.md) — Feature decomposition strategy
- [api.md](docs/api.md) — HTTP API reference
- [ADR/](docs/ADR/) — Architecture decision records
- [features/](docs/features/) — Per-feature specs (multipart coordinator, …)

## Implementation status

### P1 (foundation)

The **P1 control plane** is implemented and tested. Highlights:

- Two crates: `cf-gears-file-storage-sdk` (public API) + `cf-gears-file-storage` (gear lib + `sidecar` binary).
- Control-plane REST under `/api/file-storage/v1` (create/presign/bind, download-URL, metadata CRUD, list,
  versions, storages) — JSON only; content never transits the control plane.
- Immutable-blob + content-pointer model with optimistic-CAS bind, FileStorage-level versioning, tenant isolation,
  Authorization-Service per-type checks, conditional requests (ETag / `If-Match` / `If-None-Match`).
- Pluggable backends (trait + `local-fs` + `in-memory`); Ed25519 signed URLs (codec-equivalent to, but not literal,
  PASETO `v4.public` — see [ADR-0004](docs/ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md)'s
  Implementation note); SHA-256 + magic-byte content-type validation; HTTP `Range`. Data-plane **sidecar** binary
  verifies tokens and streams bytes, then calls a token-authenticated `finalize` callback back to the control plane
  (`pending → available`); binding a version as the file's live content (`content_id`) is always a separate,
  client-issued request (see [DESIGN.md](docs/DESIGN.md) §3.6 and
  [ADR-0003](docs/ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)).

### P2 (this branch)

Built on top of P1, the following shipped:

- **Policy engine** — allowed-types / size / custom-metadata-limit policies, resolved at tenant and user scope
  (`GET`/`PUT /policy`, `GET /policy/effective`). See
  [docs/features/policy-engine.md](docs/features/policy-engine.md).
- **Retention rules + background cleanup sweep** — per-tenant retention rules (`/retention-rules`) plus a background
  process that prunes expired files and reconciles orphaned backend objects. See
  [docs/features/retention-cleanup.md](docs/features/retention-cleanup.md).
- **Idempotent create** — `POST /files` is safe to retry.
- **Audit outbox** — a transactional outbox recording write operations (create, finalize, bind, metadata update,
  delete, ownership transfer, backend migration, …) for downstream audit consumption. The write side is fully
  shipped and tested; **the drain/relay side is not implemented** — see
  [docs/features/audit-trail.md](docs/features/audit-trail.md) for the tracked gap.
- **Events outbox** — file lifecycle events (`file.created`, `file.content_updated`, `file.metadata_updated`,
  `file.owner_transferred`, `file.deleted`) are written transactionally alongside the mutation. **Not yet drained**
  to the platform EventBroker — see Tier 4 item 4.1 in the P2 remediation plan (same undrained-relay gap as the
  audit outbox above). **`enabled_event_types` policy knob stored but not yet enforced** — `PolicyBody` round-trips
  an `enabled_event_types` list through `GET`/`PUT /policy`, but no enqueue path consults it: every event type above
  is always enqueued, regardless of policy. See [docs/features/policy-engine.md](docs/features/policy-engine.md)
  for the tracked gap.
- **Ownership transfer** — `POST /files/{id}/transfer`, atomic owner swap with audit + event + usage-delta reporting.
  Target-owner validation is **partial** — only the nil-UUID sentinel is rejected; see
  [docs/features/ownership-transfer.md](docs/features/ownership-transfer.md) for the tracked gap.
- **Backend migration** — `POST /files/{id}/migrate`, relocates a non-versioned file's content to a different backend
  with a verified, mode-aware content-hash check before committing. See
  [docs/features/backend-migration.md](docs/features/backend-migration.md).
- **Multipart upload** — server-authoritative parts plan, per-part signed URLs, and the sidecar's report-part
  callback are wired end-to-end. The richer `complete` contract (`200` with version id/size/composite-hash/manifest,
  optional `If-Match`, `409`-with-missing-parts) and the `GET .../multipart/{upload_id}` introspect/resume endpoint
  are both **shipped**, as is abort deleting the part rows and pending version. **Functional only against a
  `multipart_native` backend** (today: the non-durable in-memory backend for dev/test, and configured S3 backends)
  — the default `local-fs` backend does not declare `multipart_native`, so `POST /files/{id}/multipart` is rejected
  against the real default topology. S3 remains opt-in and merge/release-gated by the ADR-0005 external-dependency
  security review. See [docs/features/multipart-coordinator.md](docs/features/multipart-coordinator.md) for the full
  contract and its one remaining tracked gap (the part-hash trust gap noted below).
- **Storage quota — consumer scaffolding only, not enforced.** `check_quota`/`check_quota_bytes` gate every
  storage-increasing operation (`create_file`, `presign_version`, multipart initiate) via the `QuotaClient` port
  (`src/infra/external_clients.rs`), and are designed to fail **closed** once a real client is wired (a client
  error is propagated and denies the request — see `tests/enforce_test.rs`). **But `gear.rs` always constructs both
  services with `quota_client: None`** (`TODO(P2)`, Tier 1 item 1.4), and `None` makes the check a no-op
  (`Ok(())`) — so **storage quota is not enforced in any deployment today**; the effective default is permissive /
  fail-**open**, the opposite of the fail-closed design intent. This is blocked on a Quota Enforcement SDK crate,
  which does not exist yet: `gears/system/quota-enforcement/` is docs-only (PRD/DESIGN/ADRs, no Rust crate).
  Contrast with usage reporting, which is further along — a `usage-collector-sdk` crate exists (P2 1.12), even
  though `usage_reporter` is also still wired as `None` pending its own integration work.
- **Multipart part-hash trust — known gap, not yet closed.** Single-shot uploads re-derive
  size/hash/MIME from a real backend read-back at finalize time, so a forged claim cannot corrupt
  stored metadata. Multipart uploads have no equivalent: `report_part` persists the caller-supplied
  per-part hash after only a length/size check, and `complete` builds the composite hash from those
  stored hashes with no re-read of the assembled object. See
  [ADR-0003](docs/ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)'s "Known gap" note for the
  mitigation available today (`require_finalize_internal_secret`) and the tracked follow-up.

**Not yet implemented**: sharing (shareable links), WebDAV, and quota enforcement wiring (Tier 1 item 1.4, see the P2
storage-quota status above). The S3 backend (Tier 1 item 1.7) is implemented and wired as an opt-in backend for the
control plane and sidecar, but it remains gated by the ADR-0005 external-dependency security review before merge or
release use.

### Run

```bash
cargo build -p cf-gears-file-storage                 # control-plane gear (lib)
cargo build -p cf-gears-file-storage --bin sidecar   # data-plane sidecar
cargo test  -p cf-gears-file-storage -p cf-gears-file-storage-sdk

# Sidecar env (4 of 9 — see docs/operations.md for the rest): FS_SIDECAR_ADDR,
# FS_SIDECAR_PUBLIC_KEY (base64url Ed25519), FS_SIDECAR_BACKEND_ROOT,
# FS_SIDECAR_CONTROL_URL (control-plane base URL for the finalize/report-part
# callbacks -- without it, uploads default to http://localhost:8080 and stay
# `pending` forever against any other control-plane address)
```
