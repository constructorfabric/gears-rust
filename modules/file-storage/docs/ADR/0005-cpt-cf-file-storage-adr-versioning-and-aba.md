---
status: accepted
date: 2026-05-05
---
<!-- Created: 2026-05-05 by Constructor Tech -->

# ADR-0005: Bucket Versioning Awareness and ABA-Safe Content CAS


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A — Always require bucket versioning](#option-a--always-require-bucket-versioning)
  - [Option B — Per-backend `versioning` flag, trust the operator](#option-b--per-backend-versioning-flag-trust-the-operator)
  - [Option C — Probe `GetBucketVersioning` at boot](#option-c--probe-getbucketversioning-at-boot)
- [More Information](#more-information)
  - [Why ABA matters here](#why-aba-matters-here)
  - [VersionId length and shape](#versionid-length-and-shape)
  - [Historical-version GET semantics](#historical-version-get-semantics)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-versioning-and-aba`

## Context and Problem Statement

S3 supports per-bucket **object versioning**: when enabled, every PUT (and every CopyObject) creates a new VersionId, and DELETE creates a delete marker rather than physically removing data. The semantics are bucket-wide and operator-controlled — FileStorage does not turn versioning on or off; it observes the bucket's configuration.

The S3 ETag is a content fingerprint (MD5 of the bytes for single-PUT uploads under 5 GB, a structured `<hex>-<N>` value for multipart uploads). Two distinct PUTs of bit-identical bytes produce **identical** ETags. This opens a small but real **ABA window** on the etag-CAS path:

1. A reader observes `etag = e1` for a file.
2. Concurrently, somebody PUTs new bytes (`etag → e2`), then PUTs again with bit-identical content matching the original (`etag → e1` again).
3. The original reader's CAS check (`If-Match: e1`) succeeds — but the reader has actually missed a generation.

Without versioning, FileStorage cannot detect this case at the S3 boundary. With versioning enabled, S3 assigns a fresh `VersionId` on every PUT (and on every CopyObject), so two PUTs of identical bytes are still distinguishable by `(etag, version_id)`.

The decision is whether and how FileStorage should expose versioning awareness — both to close the ABA window on the strong-CAS variant of `PUT /meta` and to enable callers to fetch historical generations via `GET ?versionId=…`.

## Decision Drivers

- **ABA likelihood vs. cost** — ABA on content requires either a deliberate adversary or a very specific accidental "delete then restore" pattern. For files large enough to have a non-trivial size, the practical likelihood is low. But for small files (config blobs, JSON manifests), the likelihood is non-zero, especially under retry storms.
- **Operator control over bucket configuration** — versioning has cost implications: every overwritten object lingers in the bucket until lifecycle rules expire it, and DELETE creates delete markers that must be cleaned up. Mandating versioning would force operational overhead on every deployer.
- **Boot-time probing has costs** — `GetBucketVersioning` is one network call per backend, gated on the bucket existing and credentials being valid at boot. This conflicts with `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`, which deliberately keeps boot fast and decoupled from third-party health.
- **Historical-version reads are useful** — even without ABA concerns, callers who have a `version_id` from a prior reconcile may want to fetch that exact generation later (e.g. an audit trail that pinned `(file_id, version_id)`). Surfacing the field in the SDK costs little and unlocks the use case.

## Considered Options

* Option A — Always require bucket versioning
* Option B — Per-backend `versioning` flag, trust the operator
* Option C — Probe `GetBucketVersioning` at boot

## Decision Outcome

Chosen option: **Option B — Per-backend `versioning` flag, trust the operator**.

The `Backend` configuration in TOML carries a boolean `versioning` field, declared by the operator alongside the bucket's actual configuration. FileStorage trusts the declaration without runtime validation. Behavioural consequences:

1. **Per-row `version_id` column.** When `Backend.versioning = true`, every uploaded row records the current generation's S3 VersionId in the `version_id` column. When `false`, the column stays `NULL`. This is reflected in `FileInfo.version_id` (typed `Option<String>` in the SDK, `[string, 'null']` in OpenAPI).
2. **ABA-safe CAS on `PUT /meta` strong-CAS variant.** When the caller passes `If-Match: <etag>` to `PUT /files/{id}/meta` and the hosting backend has `versioning = true`, the server's HEAD-then-CopyObject sequence verifies BOTH `s3_etag` and `s3_version_id` against the row before issuing the CopyObject. A bit-identical re-upload would have rotated the version_id even though etag stayed the same, so the CAS detects the missed generation. When `versioning = false`, the strong-CAS verifies `s3_etag` only, and the ABA window is an accepted P1 risk (`cpt-cf-file-storage-constraint-versioning-aware-cas`).
3. **Historical-version GET.** `PresignDownloadItem.version_id` is honoured when the file's hosting backend has `versioning = true`. The server includes `versionId=<vid>` in the signed URL so the caller fetches that exact generation. When the backend has `versioning = false`, the field is silently ignored (the URL resolves to current bytes).
4. **No boot-time probe.** FileStorage does not call `GetBucketVersioning` at module init. If the operator declares `versioning = true` on a bucket that actually has versioning off, every uploaded row will have `version_id = "null"` (the literal string S3 returns for non-versioning buckets in the `x-amz-version-id` header) or simply absent — the operator-declared mismatch is observable at runtime via reconcile but not detected at boot.

### Consequences

- Good, because **operators choose their own cost / safety trade-off**. Deployments that already enable bucket versioning (most production AWS deployments do, for compliance reasons) get ABA-safe CAS for free; deployments that prefer a simpler bucket layout (typical MinIO / Ceph dev installations) opt out cleanly.
- Good, because **the SDK shape is unconditional**: `version_id: Option<String>` is always present on `FileInfo`, just `None` when versioning is off. Consumers do not branch on backend kind.
- Good, because **historical-version reads are unlocked uniformly** through `PresignDownloadItem.version_id`. Audit trails, compliance exports, and "show me what this file looked like at time T" workflows become a one-line SDK change.
- Good, because **boot stays fast**. No third-party network call at module init.
- Bad, because **operator misconfiguration is not detected at boot**. A `versioning = true` declaration on a non-versioning bucket silently degrades to "version_id never populated"; the ABA-safe CAS path becomes a no-op without a clear signal. Mitigation: the operator runbook explicitly calls out this dependency, and a P2 health check could probe `GetBucketVersioning` as a non-blocking warning.
- Bad, because **ABA on content without versioning is accepted as a P1 risk**. Documented in `cpt-cf-file-storage-constraint-versioning-aware-cas`. The risk is lowest for files with non-trivial size (where bit-identical re-uploads are vanishingly unlikely outside deliberate adversaries) and highest for tiny config-shaped files.

### Confirmation

- `Backend` schema in OpenAPI declares `versioning: boolean` (required field).
- `FileInfo.version_id` is `[string, 'null']`; populated for backends with `versioning = true`, `null` otherwise.
- `PresignDownloadItem.version_id` is `[string, 'null']`.
- The `migration.sql` schema includes `version_id VARCHAR(1024)` (nullable). The 1024-byte ceiling is dictated by AWS S3 — see "VersionId length and shape" below.
- The strong-CAS variant of `PUT /meta` HEADs S3 and verifies both `s3_etag` and `s3_version_id` against the row when `versioning = true`; verifies only `s3_etag` when `versioning = false`.
- `reconcile` populates `version_id` from the HEAD response on versioning-on backends; on versioning-off backends `s3_version_id` from HEAD is `None` and the row's `version_id` stays `NULL`.
- Integration test on a versioning-on bucket: bit-identical re-upload path, strong-CAS `PUT /meta` after the re-upload returns `412 etag_mismatch` (the ABA detection).
- Integration test on a versioning-off bucket: same scenario, strong-CAS `PUT /meta` succeeds (ABA window is accepted).
- Integration test on a versioning-on bucket: presign-download with `version_id = <historical>` returns a URL that resolves to the historical bytes.

## Pros and Cons of the Options

### Option A — Always require bucket versioning

Make versioning a precondition for any FileStorage backend. Refuse to register a backend whose bucket does not have versioning enabled.

* Good, because eliminates ABA on content entirely.
* Good, because makes the SDK shape uniform — `version_id` is always `Some(_)` on `FileInfo`.
* Bad, because **forces operational overhead on every deployer**: bucket versioning means every overwritten object lingers (storage cost), every DELETE creates a delete marker (housekeeping), and lifecycle rules must be configured to keep the bucket from growing unboundedly.
* Bad, because **breaks several existing on-prem deployment patterns**: dev MinIO clusters, ephemeral CI buckets, single-version legacy buckets — none are usable as FileStorage backends without a migration.
* Bad, because **gates module init on a third-party probe** (whichever check verifies versioning is on) — conflicts with `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`.

### Option B — Per-backend `versioning` flag, trust the operator

The chosen option. See above.

### Option C — Probe `GetBucketVersioning` at boot

Auto-detect versioning state by calling `GetBucketVersioning` on every backend at module init. Populate `Backend.versioning` from the response.

* Good, because eliminates the operator-declaration-mismatch failure mode.
* Bad, because **gates module init on every backend's reachability and credential validity at boot** — directly conflicts with `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`.
* Bad, because **adds a layer of "magical" behaviour** that operators cannot inspect from the TOML alone — debugging "why is my CAS not ABA-safe?" becomes a runtime question rather than a configuration question.
* Bad, because **the probe must be re-issued whenever bucket configuration changes**, which it can do at any time on the operator's side. The "single boot-time probe" model is fragile to that.

## More Information

### Why ABA matters here

Strong-CAS via `If-Match` exists to give callers a "do this only if the file hasn't changed under me" guarantee. The intent is that two callers racing on the same file end up with one winner and one loser, deterministically. ABA breaks that intent for the specific corner case where the bytes returned to "current" between the loser's read and the loser's write — even though the loser observed an intermediate state, the CAS pretends nothing happened.

In FileStorage's lifecycle, ABA can occur via:

1. **Bit-identical re-upload.** Caller A uploads bytes B1 (etag e1). Caller A re-uploads identical bytes (etag still e1). Caller B observed e1 between the two uploads. Caller B's CAS-protected `PUT /meta` succeeds, even though caller B's observation was effectively from before the re-upload — the metadata they're patching is "as of e1 round-1", but it lands "as of e1 round-2".
2. **Restore-after-overwrite.** Caller A uploads B1 (e1). Caller B overwrites with B2 (e2). Caller A re-uploads B1 (etag back to e1). Caller B's `PUT /meta` (still pinning e1 from earlier observation) succeeds, even though their assumption "the file is unchanged since I last looked" is now false.

In both cases, versioning observably breaks the ABA: the version_id rotates on every PUT regardless of byte equality. The strong-CAS check `(etag, version_id)` detects the missed generation.

### VersionId length and shape

The `version_id` DB column is sized to the maximum length AWS S3 promises for a VersionId. The exact contract from the AWS User Guide ("How S3 Versioning works → Version IDs"):

> Only Amazon S3 generates version IDs, and they cannot be edited. Version IDs are Unicode, UTF-8 encoded, URL-ready, opaque strings that are no more than 1,024 bytes long. The following is an example:
>
>     3sL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY+MTRCxf3vjVBH40Nr8X8gdRQBpUMLUo

What this gives FileStorage:

- **Storage budget** — `VARCHAR(1024)` accommodates any VersionId S3 may emit. We do not parse, normalize, or shorten the value; the column is a verbatim mirror of the S3 response header.
- **URL-readiness** — passing `version_id` straight into `?versionId=<vid>` on a presigned GetObject URL never requires percent-encoding. This keeps the presigned-URL pipeline simple.
- **Opaqueness** — FileStorage **MUST NOT** sort, compare-numerically, or assume monotonicity over `version_id`. The only operations it performs on the value are exact-equality and pass-through to S3. The optimistic-concurrency primitives use the row's `etag` and `updated_at`, not the version_id.
- **Client-side immutability** — only S3 generates the value; clients (and FileStorage on their behalf) cannot mint, edit, or replay version_ids. A re-upload always produces a new VersionId; FileStorage records it via `reconcile`.

This contract lives at User-Guide level. The S3 API references for `PutObject`, `GetObject`, and `ListObjectVersions` describe `VersionId` as `Type: String` without a length cap — the 1024-byte ceiling is the User-Guide-authoritative value, and S3-compatible implementations (MinIO, Ceph RGW, Wasabi, GCS S3-compat) follow it.

### Historical-version GET semantics

When `PresignDownloadItem.version_id` is set and the backend has versioning on, the resulting signed URL embeds `versionId=<vid>` (per the S3 SigV4 GetObject contract). The browser following the URL fetches that exact historical generation, even after newer generations or delete markers exist.

When the backend has versioning off, the `version_id` field is silently ignored — passing it on a non-versioning bucket would produce a URL that returns 400 from S3 ("InvalidArgument"), so the safer behaviour is to issue a current-version URL. This is documented on `PresignDownloadItem` so callers can branch on `Backend.versioning` if they require strict historical-fetch semantics.

The retention of historical generations is operator-controlled at the bucket level (S3 lifecycle rules). FileStorage does not track or enforce historical-version retention; if the operator's lifecycle expires generation V_old, a presign-download URL embedding `versionId=V_old` resolves to `404 NotFound` from S3.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md) §2.2 (`cpt-cf-file-storage-constraint-versioning-aware-cas`), §3.7 (`version_id` column).
- **Related ADRs**: [ADR-0001](./0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), [ADR-0003](./0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), [ADR-0004](./0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md).
- **Companion specs**: [rust-traits.md](../rust-traits.md), [openapi.yaml](../openapi.yaml), [migration.sql](../migration.sql).
- **External**: [logs/s3-limitations.md](../../../logs/s3-limitations.md) — running record of S3-compat quirks the FileStorage adapter has had to work around.

This decision directly addresses:

* `cpt-cf-file-storage-fr-conditional-requests` — strong-CAS on `PUT /meta` becomes ABA-safe on versioning-on backends.
* `cpt-cf-file-storage-fr-backend-capabilities` — `Backend.versioning` is part of the per-backend declarative configuration.
* `cpt-cf-file-storage-constraint-versioning-aware-cas` — the constraint this ADR establishes.
* `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check` — preserved by deferring the versioning check to runtime rather than boot.
