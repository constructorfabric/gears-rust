---
status: accepted
date: 2026-05-05
supersedes_revisions: [2026-04-23, 2026-04-27, 2026-05-04]
---
# ADR-0003: Direct-Transfer Uploads Use Presigned PUT (SigV4), Not POST Policy


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A — Presigned PUT with SigV4 header signing](#option-a--presigned-put-with-sigv4-header-signing)
  - [Option B — Presigned POST with Policy conditions](#option-b--presigned-post-with-policy-conditions)
  - [Option C — Hybrid: PUT by default, POST per backend capability](#option-c--hybrid-put-by-default-post-per-backend-capability)
- [More Information](#more-information)
  - [Why the SigV4 drawbacks are tolerable for FileStorage](#why-the-sigv4-drawbacks-are-tolerable-for-filestorage)
  - [What Option A cannot protect against (and what to do instead)](#what-option-a-cannot-protect-against-and-what-to-do-instead)
  - [Revisit triggers](#revisit-triggers)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-presigned-put-sigv4`

## Context and Problem Statement

`cpt-cf-file-storage-fr-direct-transfer` requires FileStorage to hand the client a signed URL so the client can PUT bytes directly to the S3-class backend without FileStorage proxying the payload (`cpt-cf-file-storage-nfr-scalability`, `cpt-cf-file-storage-principle-stream-by-default`, `cpt-cf-file-storage-principle-presign-first`). S3 offers two fundamentally different signing mechanisms for this, and they sit at different points on the compatibility-versus-contract-strength curve. FileStorage must pick one as the default for its `s3-compatible` adapter.

**Mechanism 1 — Presigned PUT with SigV4 header signing.** The adapter generates a URL for `PUT /{bucket}/{key}` and signs it together with a specific set of HTTP headers (the `SignedHeaders` list). The client must send those exact headers with exactly those values, or the signature check fails with `SignatureDoesNotMatch`. This is the mechanism described today in DESIGN §3.6 (`cpt-cf-file-storage-seq-presign-upload-s3`).

**Mechanism 2 — Presigned POST with a Policy document.** The adapter generates a base64-encoded Policy containing a list of `conditions` (exact match, `starts-with`, `content-length-range`) over every form field, signs the Policy, and returns `{ url, fields: {...} }` to the client. The client submits a `multipart/form-data` POST that includes the policy and the fields; any field **not enumerated** in the Policy causes the upload to be rejected.

The two mechanisms are not minor API variants — they differ on the strongest guarantee FileStorage can make to itself about what actually ends up in S3:

- Under **PUT + SigV4**, FileStorage can **pin** specific metadata values (`x-amz-meta-file-id`, `Content-Type`, etc.) — the client must send those exact values. But FileStorage **cannot forbid** the client from adding additional unsolicited headers (e.g. `x-amz-meta-attacker: anything`, `x-amz-acl: public-read`) that were not part of `SignedHeaders` — the signature does not cover them, and S3 happily accepts them.
- Under **POST + Policy**, FileStorage can do both: pin values **and** reject any field the Policy does not list. The `s3-compatible` object emerges with exactly the fields FileStorage signed off on, and nothing else.

The open question is whether the stronger contract of POST Policy justifies its significantly narrower compatibility footprint across S3-compatible implementations. Pricing the tradeoff is inseparable from the architectural context established in ADR-0001 (the SQL metadata index is authoritative; S3 user-metadata is a mirror for DR) — without that context, the compatibility-contract tradeoff looks very different.

## Decision Drivers

* **Universal S3-compatible interoperability** — per ADR-0001, the single largest operational advantage of the `s3-compatible` adapter is that it drops into any S3-shaped bucket. Direct-transfer is a core upload path (`cpt-cf-file-storage-fr-direct-transfer`); a mechanism that works on AWS/MinIO/Ceph but is flaky on GCS S3-compat or on a customer's appliance undermines the adapter's universal-bucket promise.
* **Contract strength over what ends up in S3** — for any signed upload the client is, by construction, talking to S3 directly with no FileStorage in the data path. The only enforcement FileStorage has is whatever the signing mechanism provides. The decision is whether "pin values, allow extras" is sufficient or whether "pin values, reject extras" is required.
* **Authoritative metadata location** (ADR-0001) — the SQL index is the single source of truth for everything a reader queries; S3 user-metadata is a mirror for DR-reconstruction and drift detection. What leaks into S3 beyond what FileStorage signed does not reach readers through FileStorage's read path.
* **Client-side ergonomics and SDK surface** — PUT is a single URL and raw body; POST Policy returns `{url, fields: {...}}` and requires `multipart/form-data` assembly. Consumer-side complexity differs materially.
* **Adapter-side response shape** — the SDK direct-transfer response type must be one of the two shapes (or both, if hybrid); this propagates through every consumer that holds a `FileStorageClient` handle.
* **Behavioural consistency across a mixed roster** (`cpt-cf-file-storage-principle-modular-backend-roster`) — a deployment with several `s3-compatible` backends should not expose visibly different upload semantics per backend.

## Considered Options

* Option A — Presigned PUT with SigV4 header signing
* Option B — Presigned POST with Policy conditions
* Option C — Hybrid: PUT by default, POST per backend capability

## Decision Outcome

Chosen option: **Option A — Presigned PUT with SigV4 header signing**.

The `s3-compatible` adapter issues upload URLs from `create_presigned_url` and `create_presigned_overwrite_url` through `generate_presigned_url(PutObject, …)`, signing the system headers FileStorage wants pinned: `Content-Type` (from `meta.mime_type`), `Content-Disposition` (from `meta.name` — `attachment; filename="<URL-encoded name>"`), and every `x-amz-meta-<k>` (from `meta.custom_metadata`). **`gts_file_type` is NEVER signed into the URL** — that field is DB-only and must not appear on the S3 object (specific exception to the meta-mirror rule, see ADR-0004). The client sends raw bytes with those exact headers and gets `200 OK` from S3 (or `SignatureDoesNotMatch` if any pinned header is altered). The SDK `PresignedUploadHandle` carries a single `upload_url` string plus the FileStorage-pinned `etag_pinned`; there is no form-field descriptor.

This decision is taken on compatibility grounds. POST Policy is the stronger security contract, but it narrows the universal-bucket promise that motivates the `s3-compatible` adapter in the first place. The known drawbacks of PUT SigV4 — chiefly that the client can attach unsolicited metadata the signature does not cover — are real and are enumerated in [Consequences](#consequences) and [What Option A cannot protect against (and what to do instead)](#what-option-a-cannot-protect-against-and-what-to-do-instead). Two architectural facts make those drawbacks tolerable in practice: the SQL index (ADR-0001) is authoritative on reads, so junk user-metadata in S3 never reaches readers; and the self-healing reconciliation introduced by ADR-0004 closes the drift loop on the next read or `sync`. POST Policy would not *add* capability on the read path — it would just prevent junk from being written. Given that the adapter's downstream contract does not rely on S3 being clean, we trade the cleanliness for the compatibility.

P1 deliberately ships the bare-bones SigV4 PUT contract on the upload presign path — no conditional-PUT preconditions (`If-Match` / `If-None-Match: *`) and no SignedHeaders pinning of optimistic-lock tokens. P1 correctness on the upload path is upheld entirely by FileStorage's own primitives (conditional UPDATE with race-detection on `(etag, updated_at[, xmin])`, partial unique index, `reconcile`-driven HEAD-and-pull, lazy self-healing on `read_file`) — see ADR-0004. Note that `PUT /files/{id}/meta`'s strong-CAS variant DOES use a backend-side precondition: it issues `CopyObject` with `x-amz-copy-source-if-match`, because that path mutates an existing object and the precondition is universally honoured by S3 servers for `CopyObject`. Layering optional backend-side preconditions on the upload PUT path is a future enhancement; see DESIGN §4 Future deltas.

### Consequences

* Good, because every S3-compatible endpoint we expect to support (AWS S3, MinIO, Ceph RGW, Wasabi, GCS S3-compat, Backblaze B2 S3-compat, on-prem appliances) implements presigned SigV4 PUT identically — there is no "this backend handles direct uploads differently" matrix.
* Good, because the client-side contract is the simplest possible: `PUT {url}` with raw bytes and a short list of mandatory headers. No form encoding, no policy field enumeration, no base64 policy parsing. Mainstream HTTP libraries handle this in one line.
* Good, because the SDK `PresignedUploadHandle` stays a single `upload_url` string — consumers do not need to understand form-field assembly, and the type is stable across P1 embedded / P3 remote modes (§2.1 `cpt-cf-file-storage-principle-batch-presigned-urls`).
* Good, because pinned values are enforced — `x-amz-meta-file-id`, `Content-Type`, `x-amz-meta-gts-file-type`, `x-amz-meta-owner`, `x-amz-meta-tenant-id` etc. cannot be altered by the client without the upload failing. FileStorage can rely on the pinned subset.
* Good, because the decision stays consistent with `cpt-cf-file-storage-principle-modular-backend-roster` — no per-backend capability divergence on the direct-transfer path.
* Bad, because the client can send **additional** `x-amz-meta-*` headers that FileStorage did not sign, and S3 will store them on the object. FileStorage cannot prevent this at the S3 boundary. Mitigation: the SQL row is authoritative on reads; the unsolicited user-metadata is observable only via `reconcile`, which pulls every `x-amz-meta-*` header from the HEAD response. The next `reconcile` after such an injection therefore writes the unsolicited entries into the row's `custom_metadata` — not a leakage but a behavioural footnote: clients that re-PUT and never `reconcile` cannot inject metadata that any FileStorage reader sees, but a `reconcile` after such a PUT will pick up whatever S3 holds. Operators concerned about adversarial uploads should restrict the bucket's accepted `x-amz-meta-*` set via bucket policy.
* Bad, because the client can send unsolicited non-metadata headers that the bucket policy allows — `x-amz-website-redirect-location` (if the bucket is a static website), `Cache-Control`, `x-amz-storage-class` — which can change how the object behaves when served through native S3 interfaces (not through FileStorage's presigned download which always overrides `response-content-type` and `response-content-disposition`). Mitigation: native S3 interfaces are not an exposed surface for FileStorage deployments; public buckets are a deployment misconfiguration and are out of scope for this ADR; sensitive buckets should block these headers via bucket policy.
* Bad, because `content-length-range` is not expressible in SigV4 PUT signing. The adapter can pin an exact `Content-Length` if it knows the size in advance (rarely) but cannot say "accept anywhere between 1 byte and 50 MB". Mitigation: per-backend `max_file_size_bytes` is verified by FileStorage on `reconcile` (the HEAD response carries the actual size); rows that come back oversize are rejected and queued for delete.
* Bad, because `x-amz-meta-*` field names are lowercased and all user-metadata together is capped at 2 KB by AWS (and most compatible implementations). Any `custom_metadata` we would want to mirror into S3 has to fit that budget or be truncated / omitted from the S3 mirror (SQL remains the authoritative store). This is a deployment configuration concern, not a correctness issue.
* Bad, because the contract "what is in S3 === what FileStorage signed" is not enforced at the S3 boundary — it is enforced by the fact that nothing reads from the S3 boundary as a source of truth. If a future design ever promotes S3 user-metadata to authoritative, this ADR would have to be reopened before that promotion landed.

### Confirmation

* The S3 adapter's `issue_presigned_put` implementation uses `PutObject` presigned URLs with a SigV4 canonical request that includes exactly the mandatory header set and no others; no POST-Policy code path is shipped in P1.
* A client-side test that fetches a presigned URL and attempts to PUT with altered pinned header values (e.g. wrong `x-amz-meta-file-id`) receives `SignatureDoesNotMatch` from S3 — confirming the pinning.
* A client-side test that fetches a presigned URL and attempts to PUT with extra `x-amz-meta-foo: bar` headers succeeds — confirming the accepted drawback that extras are not blocked. A follow-up read through FileStorage's REST path confirms `x-amz-meta-foo` is **not** returned in the metadata response — the SQL row is the only source read from.
* An uploaded object is retrievable through FileStorage's download path (`POST /presign-batch` with `kind: "download"` items); the metadata returned by `getFileMeta` matches the SQL row and ignores any unsolicited `x-amz-meta-*` present in S3.

## Pros and Cons of the Options

### Option A — Presigned PUT with SigV4 header signing

Adapter signs `PutObject` with a list of mandatory headers in `SignedHeaders`. Client sends `PUT {url}` with raw body and those exact header values.

* Good, because universally implemented across every S3-compatible endpoint we expect to encounter, including older appliances and partial-compatibility cloud vendors. Zero per-backend capability matrix on the direct-transfer path.
* Good, because the client contract is a one-line `PUT` — no form encoding, no policy parsing, no multi-field form assembly.
* Good, because the SDK response is a single URL string; the response type is stable and simple, and does not differ between the P1 embedded and P3 remote modes.
* Good, because pinned header values are cryptographically enforced — clients cannot forge `x-amz-meta-file-id`, `x-amz-meta-owner`, or `Content-Type` without breaking the signature.
* Good, because the mechanism aligns with FileStorage's authoritative-SQL reading model (ADR-0001) — the adapter does not require S3 user-metadata to be clean for reads to be correct.
* Bad, because extra headers the client adds are not part of the signature and therefore are accepted by S3 — `x-amz-meta-*` and several `x-amz-*` system headers can be supplied unsolicited.
* Bad, because `content-length-range` is not expressible; per-upload size bounds must be enforced elsewhere (server-side check on `sync`).
* Bad, because the contract "S3 contains exactly what FileStorage signed" is upheld only by "nothing reads S3 as authoritative" — an implicit invariant rather than a bucket-level guarantee.

### Option B — Presigned POST with Policy conditions

Adapter signs a base64 Policy with `conditions` over every form field (exact match, `starts-with`, `content-length-range`). Returns `{ url, fields: {...} }`. Client submits `multipart/form-data` POST. S3 rejects any field not enumerated in the Policy.

* Good, because Policy conditions are an **allow-list**: fields not enumerated are refused. This is the only S3 mechanism that can express "only these four `x-amz-meta-*` fields, no others" as an S3-boundary-level guarantee.
* Good, because `content-length-range` is a first-class condition, so per-upload size bounds can be enforced by S3 itself without a reconciliation step.
* Good, because `starts-with` on keys/metadata supports tenant- or prefix-scoped validation at the S3 boundary (e.g. `["starts-with", "$x-amz-meta-gts-file-type", "gts.x.fstorage."]`).
* Good, because the resulting S3 objects are byte-for-byte what FileStorage authorised — no drift possible via client header injection.
* Bad, because **S3-compatibility coverage is not uniform**. AWS S3, MinIO, and Ceph RGW support POST Policy well; Wasabi generally does; GCS S3-compat has meaningful gaps in Policy semantics; Backblaze B2 S3-compat historically lagged; some on-prem appliances and edge devices simply do not implement POST Object. Under the `s3-compatible` adapter's universal-bucket promise (ADR-0001), a mechanism that requires a per-backend support matrix is a significant regression.
* Bad, because the client contract is `multipart/form-data` — form field assembly, boundary encoding, ordering (the file field must come last), and a larger SDK surface to describe `{ url, fields: {...} }`.
* Bad, because the SDK upload-handle type grows: it must carry the URL plus a `fields: BTreeMap<String, String>` map; every consumer that holds the type through P3 must serialise it over the wire.
* Bad, because the Policy document has its own specification grammar — base64 encoding, ISO-8601 UTC expiration, specific condition types — adding a class of bugs (wrong date format, missing field, case sensitivity) that SigV4 PUT does not have.
* Bad, because introducing POST Policy for consistency across the mixed backend roster conflicts with `cpt-cf-file-storage-principle-modular-backend-roster` — not every backend will be able to honour it, so SDK behaviour would either diverge per backend or be capped at "lowest common denominator", which is Option A anyway.

### Option C — Hybrid: PUT by default, POST per backend capability

Declare a backend capability (e.g. `direct_transfer_policy = true`). Adapters that support POST Policy on the configured bucket use it; others fall back to PUT.

* Good, because deployments on AWS/MinIO/Ceph can opt into the stronger contract; deployments on GCS or exotic S3-compat can stay on PUT.
* Good, because preserves universal compatibility as the default while letting rich deployments trade up.
* Bad, because **doubles the direct-transfer code path in the adapter**: two S3 signing implementations, two response shapes, two test matrices.
* Bad, because **the SDK response type diverges per backend** — consumers must handle either `upload_url: String` or `upload_url: String + fields: Map<String,String>`. In P3 remote mode, this means the wire protocol must carry both shapes, plus a discriminator — a long-term interface cost for a tactical security gain.
* Bad, because deployments with mixed rosters (some POST-capable, some not — `cpt-cf-file-storage-principle-modular-backend-roster`) must document which backend gives which guarantee, and consumers become aware of backend internals they should not need to know.
* Bad, because the drawback Option A leaves open (unsolicited metadata) is the same drawback consumers would have to tolerate on every non-POST-capable backend — so Option C does not actually eliminate the problem; it just narrows the deployments where it occurs.

## More Information

### Why the SigV4 drawbacks are tolerable for FileStorage

Option A's headline drawback — the client can attach unsolicited `x-amz-meta-*` headers — is a real S3-boundary weakness. What makes it tolerable in this specific module is the combination of two pre-existing decisions:

- **ADR-0001**: the SQL index is the authoritative metadata store. The download and metadata endpoints read from the SQL row; S3 user-metadata is a mirror for DR reconstruction and for drift detection, not a read source. Any user-metadata the client injects is invisible to callers of the FileStorage REST API and SDK.
- **ADR-0002**: external URLs carry only an opaque `file_id`; there is no logical-path information to be spoofed or re-interpreted. The only thing a client can do by injecting extra metadata is bloat its own S3 object; it cannot change what FileStorage's readers see.

If either decision reverses — if the S3 mirror is ever promoted to an authoritative source, or if URLs ever re-encode owner/tenant information decoded from metadata — this ADR must be reopened before that promotion ships.

### What Option A cannot protect against (and what to do instead)

Option A does not protect against:

1. **Unsolicited `x-amz-meta-*`** — mitigated by reading from SQL; harmless to FileStorage consumers.
2. **Unsolicited non-meta headers** (`x-amz-website-redirect-location`, `x-amz-acl`, `Cache-Control`, `Content-Disposition`) — mitigated at the bucket policy layer. Deployment runbook should state that direct-transfer buckets must reject these headers via bucket policy; this is an **operational control**, not a module-level one. `cpt-cf-file-storage-constraint-static-config-p1` places the backend configuration in TOML; the runbook accompanies each backend instance.
3. **Content-length range enforcement** — mitigated at commit time: `reconcile` reads the object's size from the HEAD response and rejects rows that exceed `max_file_size_bytes`. This is slower than S3-level rejection, but bounded.
4. **Replay** — the expiration on the presigned URL bounds the replay window; an attacker who captures the URL has at most the configured TTL. This is not Option A-specific; POST Policy has the same property.

### Revisit triggers

Reopen this ADR if any of the following holds:

- A concrete attack vector emerges where unsolicited `x-amz-*` headers, injected by a client via presigned PUT, cause harm through a read path that FileStorage directly exposes (not through native S3).
- POST Policy support matures across the `s3-compatible` backends we support — specifically, when GCS S3-compat, Backblaze B2 S3-compat, and the enterprise appliances we target all implement the full Policy grammar.
- A future ADR promotes S3 user-metadata to authoritative (e.g. for a no-DB deployment mode), making drift in S3 a correctness concern rather than a cleanliness concern.
- `content-length-range` or a similarly expressive mechanism is added to the SigV4 PUT signing model, closing the size-enforcement gap without switching mechanisms.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related ADRs**: [ADR-0001](./0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md) — authoritative SQL metadata index; [ADR-0002](./0002-cpt-cf-file-storage-adr-opaque-file-ids.md) — opaque file-id addressing; [ADR-0004](./0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md) — self-healing reconciliation as the base correctness mechanism (since this ADR ships PUT-SigV4 without backend-side preconditions).

This decision directly addresses the following requirements and design elements:

* `cpt-cf-file-storage-fr-direct-transfer` — direct-transfer upload URLs are issued as presigned SigV4 `PutObject` URLs; the REST / SDK response shape (`PresignedUploadHandle`) carries `file_id`, a single `upload_url`, the FileStorage-pinned `etag_pinned`, and `expires_at`.
* `cpt-cf-file-storage-fr-signed-urls` — the same SigV4 signing path powers presigned download URLs (`GetObject`) returned by `presign_urls`; the decision for uploads extends naturally to downloads. Public-read backends (with `PublicReadUrls` capability and `default_public = true`) issue bare-HTTPS URLs without signing.
* `cpt-cf-file-storage-fr-backend-capabilities` — the `PresignedUrls` capability implies PUT-SigV4 semantics specifically, not POST Policy.
* `cpt-cf-file-storage-nfr-scalability` — bytes never transit FileStorage on the presign-first path; this decision is compatible with unbounded byte-volume scaling.
* `cpt-cf-file-storage-principle-stream-by-default` — upholds "redirect-capable backends bypass the data plane entirely" via the simplest available S3 signing mechanism.
* `cpt-cf-file-storage-principle-presign-first` — presigned PUT is the default upload path.
* `cpt-cf-file-storage-principle-modular-backend-roster` — preserves uniform direct-transfer semantics across all `s3-compatible` backend instances; no per-backend capability matrix on the upload path.
* `cpt-cf-file-storage-component-s3-backend` — confirms the adapter's presigned-URL surface is PUT-only on the upload side and batch-GET on the download side in P1; `issue_presigned_put` and `issue_presigned_gets` both use the SigV4 PUT/GET signing path respectively.
