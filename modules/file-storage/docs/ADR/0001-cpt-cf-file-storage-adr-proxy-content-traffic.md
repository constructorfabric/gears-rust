---
status: accepted
date: 2026-05-12
---

# ADR-0001: Proxy All File Content Traffic Through FileStorage

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Proxy all content traffic through FileStorage](#proxy-all-content-traffic-through-filestorage)
  - [Direct-to-backend transfer via presigned URLs](#direct-to-backend-transfer-via-presigned-urls)
  - [Hybrid — proxy by default, presigned URLs for bulk paths](#hybrid--proxy-by-default-presigned-urls-for-bulk-paths)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-proxy-content-traffic`

## Context and Problem Statement

FileStorage backs heterogeneous storage backends (S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV, local filesystem) and serves both interactive platform users and other Cyber Ware modules. The original PRD adopted the S3-style pattern of issuing presigned URLs that let clients upload and download content directly against the underlying backend (originally named `fr-direct-transfer` and `fr-signed-urls` in PRD drafts; both since removed), with FileStorage acting only as a metadata and signing service for the bulk-transfer paths.

The architectural question is whether file content traffic should flow through FileStorage at all, or whether FileStorage should remain a thin control plane that hands clients a signed URL pointing at the backend. The choice determines the shape of the client protocol, the visibility of underlying backends, and the surface on which audit, accounting, and policy enforcement operate.

## Decision Drivers

* Transparent backend swapping — operators must be able to migrate a tenant from one backend to another without touching clients or client-side code
* Backend opacity — clients must not depend on a specific backend protocol surface (S3, Azure Blob REST, WebDAV) so that the platform retains substitution freedom and avoids client coupling to vendor-specific SDKs
* Centralized accounting — per-tenant and per-user bandwidth metering (bytes uploaded, bytes downloaded) is required for usage reporting (`cpt-cf-file-storage-fr-usage-reporting`); presigned URLs bypass FileStorage and make accurate byte-level metering impossible
* Single client protocol regardless of backend — a single proprietary protocol over REST simplifies SDKs, tooling, and any eventual S3-compatible or WebDAV facade modules built on top of FileStorage, which would expose a uniform front independent of the backend choice
* Universal policy and audit coverage — content-type validation (`cpt-cf-file-storage-fr-content-type-validation`), policy enforcement (`cpt-cf-file-storage-fr-allowed-types-policy`, `cpt-cf-file-storage-fr-size-limits-policy`), and read audit (`cpt-cf-file-storage-fr-read-audit`) must apply to 100% of operations without per-path carve-outs for presigned flows
* Operational and cost trade-off — proxying terabyte-scale traffic through FileStorage adds bandwidth cost and turns FileStorage into a horizontally-scaled data plane rather than a thin control plane; this cost is accepted in exchange for the properties above

## Considered Options

* Proxy all content traffic through FileStorage
* Direct-to-backend transfer via presigned URLs
* Hybrid — proxy by default, presigned URLs for bulk paths

## Decision Outcome

Chosen option: "Proxy all content traffic through FileStorage", because it is the only option that preserves backend opacity, enables per-byte centralized metering, and removes per-flow carve-outs from audit, policy, and content validation. FileStorage exposes a single proprietary protocol over its REST surface; backends are an internal implementation detail and are never addressed by clients directly. Presigned URLs may continue to exist between FileStorage and its own backends as an internal optimization (e.g., for backups, migrations, or backend-side replication), but they are never returned to clients and not part of any public interface.

### Consequences

* The PRD requirements that defined direct-to-backend transfer, signed URLs, and orphaned-direct-upload garbage collection (named `fr-direct-transfer`, `fr-signed-urls`, and `fr-gc-direct-uploads` in earlier PRD drafts) are superseded by this ADR and have been removed from the PRD in the same change set that adopts this decision; the "Direct Upload from External Client" and "Generate and Access Signed URL" use cases are removed for the same reason
* External, unauthenticated sharing was originally delivered through scope-based shareable links served by FileStorage. In the current PRD revision all external/anonymous access is deferred to P3 (see PRD `§5.3` and DESIGN.md §1.1 "Sharing boundary") — its delivery shape (separate sibling module vs. FileStorage extension) will be settled by a future ADR. When implemented, that surface will still inherit FileStorage's proxy-level audit and access controls via the FileStorage SDK
* The "Presigned URLs" entry in the backend capability model (`cpt-cf-file-storage-fr-backend-capabilities`) is reframed as an internal-only capability — declared per backend, used by FileStorage itself, and never surfaced to clients; the public capability discovery surface no longer exposes it
* `cpt-cf-file-storage-fr-content-type-validation` applies uniformly to every upload — the carve-out for direct uploads is removed
* `cpt-cf-file-storage-fr-read-audit` applies uniformly to every download — the carve-out for presigned URL downloads is removed
* `cpt-cf-file-storage-fr-conditional-requests` applies to every download — the carve-out for presigned URL downloads is removed
* `cpt-cf-file-storage-fr-usage-reporting` is extended to report per-byte bandwidth in addition to stored bytes, because both ingress and egress are now observable to FileStorage
* FileStorage becomes a data-plane service: the scalability NFR (`cpt-cf-file-storage-nfr-scalability`) and content transfer latency NFR (`cpt-cf-file-storage-nfr-transfer-latency`) become first-order constraints; the service must scale horizontally to absorb terabyte-scale bandwidth and must stream content without buffering whole files in memory
* HTTP Range support (`cpt-cf-file-storage-fr-range-requests`) is required end-to-end through the proxy, because clients can no longer seek directly against the backend; resumable downloads and partial reads must work through FileStorage
* The signed-URL-key-compromise risk in the PRD risk register is narrowed: backend credentials remain a high-impact asset, but they are no longer exposed to clients in any form; the compromise surface is reduced to FileStorage's own credential store

### Confirmation

Implementation verified via:

* Code review confirming that no public SDK method or REST endpoint returns a backend-addressable URL to a client
* Code review confirming that backend client construction lives behind the storage abstraction (`cpt-cf-file-storage-fr-backend-abstraction`) and is never reachable from the client-facing API layer
* Integration tests verifying that every upload and every download flows through FileStorage handlers, including under multipart, range, and the S3-compatible and WebDAV facade paths
* Usage reports include per-byte bandwidth counters (upload bytes, download bytes) per owner, in addition to stored bytes

## Pros and Cons of the Options

### Proxy all content traffic through FileStorage

All upload and download traffic flows through FileStorage over a single proprietary protocol on top of its REST API; backends are not addressable by clients.

* Good, because clients depend on one protocol — backends can be replaced (or run in parallel) without any client change
* Good, because backend implementation is fully hidden from clients, reducing client-side blast radius if a backend is swapped, deprecated, or compromised
* Good, because every byte transits FileStorage, enabling exact per-user and per-tenant bandwidth metering for usage reporting and quota enforcement
* Good, because policy enforcement, content-type validation, and audit run on 100% of operations without per-path carve-outs
* Good, because heterogeneous backends (S3, Azure, WebDAV, local FS, NFS, FTP, SMB) appear identical to clients, which is required for the SDK, S3-compatible API, and WebDAV facades to behave consistently across deployments
* Bad, because bandwidth cost shifts onto FileStorage; terabyte-scale traffic now consumes platform egress and ingress instead of going edge-to-backend
* Bad, because FileStorage becomes a data-plane bottleneck and must be horizontally scaled and operated as a streaming service rather than a thin control plane
* Bad, because end-to-end transfer latency includes an extra hop; backend-native acceleration (e.g., S3 Transfer Acceleration, regional edge endpoints) is unavailable to clients

### Direct-to-backend transfer via presigned URLs

FileStorage only signs URLs; clients talk to S3, Azure, GCS, or WebDAV directly using the backend-native protocol.

* Good, because FileStorage carries no content bandwidth — the data plane is the backend itself, which is already designed for it
* Good, because backend-native features (multipart upload tuning, transfer acceleration, multi-region edge) are directly available to clients
* Bad, because clients must support N backend protocols (S3, Azure REST, WebDAV, GCS, FTP, SMB) — substituting a backend either breaks clients or forces a parallel client release
* Bad, because bandwidth is invisible to FileStorage — per-byte metering is impossible; usage reports can only count stored bytes, not transferred bytes
* Bad, because audit, read audit, content-type validation, and policy must add per-flow carve-outs for presigned paths, fragmenting the enforcement story (the original PRD already had to exclude presigned downloads from read audit and conditional requests)
* Bad, because backend credentials must be operated to support delegated signing for clients, and a signed-URL key compromise has direct, non-revocable consequences for downloads in flight
* Bad, because the FileStorage-served shareable link model loses force — external sharing inevitably leaks the backend identity through the signed URL

### Hybrid — proxy by default, presigned URLs for bulk paths

Small or auth-sensitive operations go through FileStorage; clients receive presigned URLs for bulk transfer (large uploads or downloads) on an opt-in basis.

* Good, because cuts the bandwidth bill for the heavy tail while keeping the proxy story for the median request
* Good, because each backend's native bulk-transfer tuning can still be used when it matters
* Bad, because clients still need to implement N backend protocols for the bulk path — the "single client protocol" property is lost
* Bad, because metering, audit, and policy must reason about two flows; per-byte accounting becomes correct only for the proxy slice and is structurally incomplete for the presigned slice
* Bad, because the threshold ("when does bulk apply") becomes a coupling between client and FileStorage that drifts with backend pricing and limits
* Bad, because backend opacity is partial; whenever a client hits the bulk path the backend identity and protocol leak through the URL

## More Information

This decision aligns FileStorage with the Google Drive style of object storage — a control and data plane operated as one service over a proprietary protocol — and against the S3 style of a control plane that delegates the data plane via presigned URLs. The S3 style remains a legitimate pattern (Backblaze B2, MinIO, S3 itself), but the requirements that drive this decision — backend substitution, single client protocol across heterogeneous backends, per-byte centralized accounting, and uniform audit and policy coverage — are not compatible with delegated data planes.

Presigned URLs remain a legitimate internal mechanism between FileStorage and its own backends (e.g., a backend-side replication job using a backend-issued signed URL, or backup tooling running inside the platform). The boundary is that they are never returned to a client and are not part of any public interface, SDK return type, or REST response body.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `fr-direct-transfer` (historical PRD ID, since removed) — Superseded by this ADR; removed from PRD in the same change set
* `fr-signed-urls` (historical PRD ID, since removed) — Superseded by this ADR; removed from PRD in the same change set
* `fr-gc-direct-uploads` (historical PRD ID, since removed) — Removed because the orphaned-direct-upload class no longer exists
* `fr-shareable-links` (historical PRD ID, since removed) — Originally intended as the sole external sharing mechanism. In the current PRD revision all external/anonymous sharing (anonymous URLs, per-principal grants, expirations, etc.) is deferred to P3 (see PRD `§5.3`); its delivery shape is left to a future ADR
* `cpt-cf-file-storage-fr-backend-capabilities` — "Presigned URLs" capability is reframed as internal-only; it remains useful between FileStorage and its backends but is removed from the public capability discovery surface
* `cpt-cf-file-storage-fr-content-type-validation` — Applies uniformly to every upload; the direct-upload carve-out is removed
* `cpt-cf-file-storage-fr-read-audit` — Applies uniformly to every download; the presigned-URL carve-out is removed
* `cpt-cf-file-storage-fr-conditional-requests` — Applies uniformly to every download; the presigned-URL carve-out is removed
* `cpt-cf-file-storage-fr-usage-reporting` — Extended to include per-byte bandwidth (upload and download) per owner, in addition to stored bytes
* `cpt-cf-file-storage-fr-range-requests` — Required end-to-end through the proxy to preserve seek and resumable-download UX without backend addressability
* `cpt-cf-file-storage-nfr-scalability` — Recharacterized as a data-plane scalability constraint; FileStorage must scale horizontally to absorb terabyte-scale bandwidth
* `cpt-cf-file-storage-nfr-transfer-latency` — Recharacterized; the 50ms p95 fixed-overhead budget now includes the proxy hop and end-to-end streaming behavior
* `cpt-cf-file-storage-fr-backend-abstraction` — Reinforces that the storage abstraction is the only path to backend protocol surfaces; backend clients must never be reachable from the public API layer
