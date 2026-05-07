---
status: accepted
date: 2026-04-27
supersedes_revisions: [2026-04-23]
---
# ADR-0002: Opaque File IDs as the External Addressing Scheme


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A — Human-readable path URL (strawman)](#option-a--human-readable-path-url-strawman)
  - [Option B — Opaque UUID URL, display name in metadata](#option-b--opaque-uuid-url-display-name-in-metadata)
  - [Option C — Opaque UUID URL with MIME-derived extension suffix](#option-c--opaque-uuid-url-with-mime-derived-extension-suffix)
- [More Information](#more-information)
  - [Why S3 path encoding is not "just a path"](#why-s3-path-encoding-is-not-just-a-path)
  - [Relation to ADR-0001 and the "S3-only" trade-off](#relation-to-adr-0001-and-the-s3-only-trade-off)
  - [Semantic shifts under Option B / C](#semantic-shifts-under-option-b--c)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-opaque-file-ids`

## Context and Problem Statement

A file-storage REST API must pick a shape for the canonical external handle of a file. The strawman shape — addressing files as `(tenant_id, backend_id, file_path)` — embeds the tenant, the backend name, and a user-controlled logical path in every URL. When the `s3-compatible` adapter is used, the physical S3 object key tends to mirror the same shape, which makes the URL self-describing: anyone who sees it learns the tenant, the owner kind, and the original filename (including any sensitive hint the path carries). It also makes every URL — REST, SDK, and presigned S3 — a carrier of user-controlled text whose encoding must be negotiated exactly between the client, the FileStorage REST router, the adapter, and the backend.

That last point is not a footnote. A REST URL like `PUT … /files/{file_path}` *looks* like "pass the path through" but actually requires the system to reconcile four different encoding regimes — HTTP percent-encoding, Unicode normalization, S3 object-key conventions, and AWS SigV4 canonical URI encoding — in both directions, on every request, while also picking a stable convention for how `/`, `%2F`, empty segments, dot segments, trailing slashes, and non-ASCII bytes collapse. Any disagreement between the layers manifests as either a 404 (encode/decode mismatch), a `SignatureDoesNotMatch` (SigV4 canonical mismatch), a silent key split (`/` treated as a segment boundary by one layer and as content by another), or — worst — an authorization check that succeeds on one canonicalization and a storage lookup that succeeds on another.

The open question is whether FileStorage URLs should embed a logical, human-readable path (inheriting all of the encoding reconciliation above), or switch to opaque file identifiers (UUIDs) — collapsing the URL to `/files/{file_id}`, the physical S3 key to `{file_id}`, and pushing the display name into metadata. A secondary question is whether the opaque identifier should carry a MIME-derived extension suffix (`{file_id}.pdf`) for clients that key filename inference off the URL tail.

Because of [ADR-0001](./0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), we already own a SQL metadata index for the `s3-compatible` adapter. That is the prerequisite for any opaque-addressing scheme (there must be *some* place that maps `file_id` → S3 location, owner, authz context, display name). The framing here, therefore, is not "should we build a resolver?" — that cost is already committed — but "given the resolver exists, which addressing scheme best exploits it?".

The decision is scoped to the external addressing contract used by the REST API, the SDK facade, and the S3 physical key. It is deliberately orthogonal to FileShare (P3 module) — shareable, human-friendly links are a separate concern and would live in FileShare even if FileStorage itself moves to opaque IDs.

## Decision Drivers

* **URL privacy** — the URL should not leak `tenant_id`, `user_id`, or the original filename. Today every presigned S3 URL handed to a browser exposes all three, because the object key encodes them (DESIGN §3.6, `seq-download-redirect-s3`).
* **URL stability vs. rename** — `cpt-cf-file-storage-nfr-url-availability` requires the URL to stay valid for the retention window. Path-based URLs break if the caller changes the display name; opaque IDs decouple identity from display attributes.
* **S3 key and URL encoding complexity** — S3 object keys are byte sequences, not strings, and the path from "user types a filename" to "presigned URL signed by the backend" crosses several encoding regimes (HTTP percent-encoding, Unicode normalization, S3 object-key conventions, SigV4 canonical URI). Opaque IDs collapse this to a fixed, 36-byte hex alphabet. See [Why S3 path encoding is not "just a path"](#why-s3-path-encoding-is-not-just-a-path).
* **Browser rendering** — a browser opening a FileStorage URL must still be able to (a) infer MIME for inline rendering, (b) suggest a sensible filename on Save-As. `Content-Type` and `Content-Disposition` headers handle both, but only when the client honours them; some clients (certain download managers, WebDAV gateways, some command-line tools, some mobile OS pickers) fall back to the URL tail.
* **Authorization correctness** — `cpt-cf-file-storage-fr-authorization` calls `authz` with the file's GTS type in the resource context. The resolver must map the URL-borne address to `(tenant_id, owner, gts_file_type)` *before* the authz call; the chosen scheme must support that resolution with O(1) cost and without opening a read-side enumeration oracle.
* **DB index already committed** — [ADR-0001](./0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md) accepted the SQL metadata index for the `s3-compatible` adapter. The per-request `file_id → S3 location` lookup is therefore a no-new-cost operation for this decision; the argument is about how to best use the resolver we already own.
* **Interop with pre-signed URLs** — direct-transfer (`cpt-cf-file-storage-fr-direct-transfer`) and signed download URLs (`cpt-cf-file-storage-fr-signed-urls`) hand a backend-native URL to the client; whatever physical S3 key is chosen is visible in that URL and must survive the SigV4 canonical-encoding path intact.

## Considered Options

* Option A — Human-readable path URL (current DESIGN)
* Option B — Opaque UUID URL, display name in metadata
* Option C — Opaque UUID URL with MIME-derived extension suffix

## Decision Outcome

Chosen option: **Option B — Opaque UUID URL, display name in metadata**.

External FileStorage addresses files as `/files/{file_id}` for every operation that targets an existing file (read, update, delete, content stream, presign), where `file_id` is a `uuid` assigned by FileStorage at upload (per `cpt-cf-file-storage-constraint-server-minted-file-id`). Backend selection only happens at file creation, in `POST /presign-batch` body (optional — falls back to tenant's `default_private` if omitted); the URL never carries `backend_id`. After creation, every URL is `/files/{file_id}/…` — the backend is resolved server-side from the row.

The physical S3 key is the same `file_id`. Display name, `mime_type`, and owner live in the metadata row; the download path sets `Content-Type: {mime_type}` and `Content-Disposition: attachment; filename="{display_name}"` (or `inline` when backend config allows it), so browsers render and save files correctly without decoding the URL.

**Option C is held in reserve, not on any roadmap.** The MIME-suffix variant (`{file_id}.{ext}`) is documented here so it can be reached for quickly if — and only if — a must-support client is later found to not honour `Content-Type` / `Content-Disposition` and writes files under the raw UUID. Until such a client surfaces, no backend capability flag is introduced, no mapping table is shipped, and Option C is explicitly out of scope for delivery.

Rationale in one sentence: since ADR-0001 has already committed us to owning a SQL metadata index for the `s3-compatible` adapter (absorbing the biggest cost of any opaque-addressing scheme), the marginal cost of switching the URL to a UUID is near zero, and the benefit — closing off an entire class of encoding bugs, eliminating URL-borne identifier leakage, decoupling URLs from display names, and unlocking trivially safe presigned URLs — is disproportionately large. When the DB is paid for anyway, UUID filenames are the natural complement for maximum downstream flexibility.

### Consequences

* Good, because URLs leak nothing about tenant identity, owner identity, or filename — a leaked URL is a capability token to *fetch* the file, not a revelation about *who owns what*.
* Good, because the whole class of URL-encoding reconciliation bugs (double-encoding, Unicode NFC/NFD, `/` vs `%2F`, trailing-slash ambiguity, SigV4 canonical mismatch, proxy case-normalisation, 1024-byte key-budget arithmetic) simply does not apply to a fixed, 36-character UUID alphabet.
* Good, because renaming a file (display name / logical path change) is a metadata-only update — the URL remains stable, satisfying `cpt-cf-file-storage-nfr-url-availability` even across renames.
* Good, because the `file_id` is the durable identity; authorization, audit, and cross-module references all key off a single stable value.
* Good, because the S3 physical key becomes uniform and bucket-layout-agnostic — buckets can be resharded, replicated, or migrated without key rewrites that would otherwise cascade through DB rows, URLs, and client caches.
* Good, because direct-transfer / presigned URLs no longer expose `tenant_id` or `owner_id` — the browser-visible S3 URL is `{bucket}/{file_id}` and nothing more.
* Good, because `Content-Type` and `Content-Disposition` deliver filename and MIME to compliant clients out-of-band from the URL — this is the browser-HTTP intended path.
* Bad, because authorization gains a strict resolve step: every request must map `file_id → (tenant, owner, gts_file_type)` before the authz call. A miss (unknown `file_id`) must return `404` to avoid an enumeration oracle — the distinction between "absent" and "unauthorized" collapses, which is both a feature (no enumeration) and a debugging cost.
* Bad, because `PUT … /files/{file_path}` upsert semantics (last-write-wins on the logical path) no longer fall out of the URL shape. The logical `(tenant, owner, display_path)` tuple becomes a secondary key in the metadata index, and clients either pass `file_id` to overwrite or rely on the secondary-key index to decide overwrite-vs-create — this is a semantic shift from today's DESIGN.
* Bad, because clients that save from the URL tail (not mainstream browsers — but some download managers, WebDAV gateways, command-line `wget` without `--content-disposition`) will write the file as `{file_id}`. Option C's extension suffix is documented as a fallback but is **not planned** — it would only be reached for if a must-support client is later shown to not honour `Content-Disposition` / `Content-Type`.
* Bad, because existing integrations that construct URLs from `(tenant, backend, file_path)` (e.g., today's DESIGN sequence diagrams, upstream modules that hold logical-path handles) need to migrate to `file_id` handles — this is a breaking API change that must be serialised with the ADR-0001 adoption.

### Confirmation

* External URL shape in `openapi.yaml` is updated so reading and mutating endpoints address the file by `/files/{file_id}` (UUID); `tenant_id` is absent from every URL — it is enforced server-side from `SecurityContext`. `backend_id` never appears in the URL path; it is an optional field in the `POST /presign-batch` request body (falls back to the tenant's `default_private`).
* A `GET /files/{file_id}` probe on a known `file_id` from another tenant returns `404 not_found`, not `403`, confirming the enumeration-oracle closure.
* A file renamed via `PUT /files/{file_id}` (display name change) is fetchable under the same URL before and after the rename.
* On download, a compliant browser opens the file with the stored display name (`Content-Disposition: attachment; filename="{display_name}"`), even when the URL ends in a UUID (Option B) or a `{uuid}.{ext}` pair (Option C).
* Option C stays unimplemented: no `url_mime_suffix` flag appears in backend config, no `mime_type → ext` mapping is shipped, and the REST contract assumes no suffix. Activation is a future ADR triggered by a concrete client-compatibility incident, not a flag to be flipped in a config file.
* Encoding-regression test: uploading files whose display names contain `/`, `%`, `#`, `?`, emoji, combining diacritics (NFC vs NFD), and non-ASCII punctuation all produce stable UUID URLs and are retrievable without any client-side encoding gymnastics.

## Pros and Cons of the Options

### Option A — Human-readable path URL (strawman)

External URL encodes `/{tenant_id}/{backend_id}/{file_path}`; physical S3 key mirrors that shape (under ADR-0001's rejected Option A, with an owner prefix; even under ADR-0001's chosen Option B, the physical key could still be the human path, but the DB row would remain authoritative on reads).

* Good, because URLs are human-debuggable; support engineers can read a URL and see what it points at.
* Good, because no resolver layer is needed to go from URL → backend address (though with ADR-0001 adopted, the resolver exists anyway).
* Good, because `PUT` upsert semantics follow naturally from the URL — re-uploading to the same `file_path` is well-defined without additional bookkeeping.
* Bad, because every URL leaks `tenant_id`, `owner_kind` / `owner_id` (when the S3 key encodes them), and the original filename — including any sensitive hint in the path.
* Bad, because renaming the file breaks the URL: the persistent-URL guarantee (`cpt-cf-file-storage-nfr-url-availability`) is only valid while the display name is immutable.
* Bad, because the URL surface inherits **all** of the S3 key / HTTP URL encoding complexity enumerated in [Why S3 path encoding is not "just a path"](#why-s3-path-encoding-is-not-just-a-path): double-encoding, Unicode normalization, `/` vs `%2F`, SigV4 canonical-form mismatch, 1024-byte key-budget arithmetic, bucket DNS rules. Every one of these is a production-incident category that Option B sidesteps by construction.
* Bad, because bucket layout is fixed by the URL — resharding or migrating the bucket means URLs have to change too.

### Option B — Opaque UUID URL, display name in metadata

Existing-file URLs encode `/files/{file_id}` (no tenant, no backend name — the backend is resolved server-side from the row); the creation endpoint is `POST /presign-batch` and accepts an optional `backend_id` field in the body (falls back to the tenant's `default_private`). The physical S3 key is `{file_id}`. Display name, MIME, owner, and `file_path` live in the metadata index (now always present per ADR-0001); the download handler sets `Content-Type` and `Content-Disposition` from that record.

* Good, because URLs carry no identity or naming information — opacity by construction.
* Good, because the encoding surface of the URL collapses to a fixed alphabet (`[0-9a-f-]`, 36 bytes). All of the encoding reconciliation issues in Option A become non-issues — there is nothing to reconcile.
* Good, because rename is a metadata-only operation; URLs are stable across the retention window even when the display path changes.
* Good, because bucket layout is free — the adapter can lay out `{file_id}` by first-byte prefix, by creation date, by tenant shard, etc., without changing the external URL.
* Good, because the authz model is clean: one resolver call per request yields `(tenant, owner, gts_file_type)` and feeds authz.
* Good, because direct-transfer and presigned URLs inherit the opacity for free — the browser-visible S3 URL is also `{bucket}/{file_id}`, with a clean SigV4 canonical form.
* Good, because gets the maximum leverage out of the SQL index we have already committed to owning in ADR-0001 — any addressing scheme that doesn't exploit the resolver underuses an already-paid cost.
* Bad, because upload semantics change — `PUT` can no longer be a pure upsert on the URL; the server must decide whether to mint a new `file_id` or reuse the existing one, which requires a secondary uniqueness key on `(tenant, owner, display_path)` or an explicit client-supplied `file_id`.
* Bad, because clients that ignore `Content-Disposition` will save the file as `{file_id}`; users relying on those clients need a workaround (Option C, or a client-side fix).

### Option C — Opaque UUID URL with MIME-derived extension suffix

Same as Option B, but the external URL tail is `{file_id}.{ext}`, with `ext` derived from `mime_type` at upload (e.g., `image/png → .png`). Physical S3 key mirrors the external tail so that presigned URLs look the same.

**Status: documented as a contingency, not scheduled for delivery.** This option exists on paper in case a must-support client turns out not to honour `Content-Disposition` / `Content-Type` — in that narrow scenario, Option C is the pre-analysed escape hatch. It is not otherwise on the roadmap; no flag, no mapping table, and no client contract around suffixes is introduced until such a client is identified.

* Good, because browsers and tools that infer filename from the URL tail now see a sensible extension, restoring inline rendering and Save-As defaults for clients that ignore response headers.
* Good, because the extension is bounded by a curated `mime_type → ext` map (the adapter would own the mapping), so the suffix carries minimum information — MIME class, not the original filename.
* Good, because it is a self-contained, well-scoped fallback that can be introduced as a later ADR without disturbing Option B's contract; Option B's URLs remain a prefix of Option C's URLs if the suffix is ever added.
* Bad, because the suffix leaks the MIME class, narrowing Option B's opacity; in adversarial settings (guessing file type from URL) this may be undesirable.
* Bad, because `mime_type` and URL are now coupled — MIME changes after upload either require a second URL for the same file or a re-key; neither is free.
* Bad, because the `ext` mapping would be a new piece of module surface that has to be kept consistent across clients, the adapter, and the metadata index — a cost we deliberately choose not to pay until a concrete client need forces it.

## More Information

### Why S3 path encoding is not "just a path"

The intuition "we receive a file path in the URL, we hand it to S3, done" is wrong in several independent ways. The surface that a request crosses when Option A is in force involves four encoding regimes, each with its own rules, and each reconciliation is a potential bug class.

- **S3 object keys are bytes, not strings.** The AWS guidance recommends UTF-8 and a "safe" alphabet (`0-9 A-Z a-z ! - _ . * ' ( )`); everything else is technically legal but "requires special handling" — meaning every layer that touches the key must pick a consistent handling strategy. Any byte outside the safe set must be URL-encoded in the HTTP request, but *not* in the object key itself. Getting that boundary wrong produces phantom keys that the API "wrote" but cannot "read".
- **Forward slash is not hierarchy at the protocol level, but every layer pretends it is.** `ListObjectsV2` treats `/` as a delimiter when asked; S3 consoles present slash-delimited keys as folders; HTTP routers treat `/` as a segment boundary by default; reverse proxies may collapse `//` to `/`; and some clients percent-encode `/` as `%2F` inside a segment. A file called `2026/04/report.pdf` can survive one round-trip and break on the next depending on which layer normalised which slash.
- **URL percent-encoding is not idempotent with itself.** If the REST router decodes `file_path` once, forwards it as "already-decoded text" to the adapter, and the adapter then percent-encodes it for SigV4, everything works. If any layer assumes it receives "still-encoded" and encodes again, or decodes twice, the key diverges silently. Double-encoding bugs typically pass tests with ASCII-only inputs and only surface on the first customer filename with a space or a Cyrillic letter.
- **Unicode normalization.** The string `é` can be one code point (NFC) or two (NFD). S3 treats them as distinct keys. Browsers may submit NFD (macOS-style filenames often are); the server may normalise to NFC for storage. A mismatch produces a 404 where both sides "see" the same visible filename.
- **SigV4 canonical URI encoding is its own regime.** Presigned URL generation percent-encodes each path segment per a *specific* rule set — unreserved RFC 3986 characters pass through, `~` is left as-is, space is `%20` (never `+`). If the adapter's percent-encoding of the key is not byte-for-byte identical to the canonicalisation the client uses when it fetches the presigned URL, the request fails with `SignatureDoesNotMatch` — an error whose message gives no hint that the culprit is encoding rather than auth.
- **Reserved characters specific to HTTP paths.** `#` becomes a fragment; `?` starts a query; `%` must be `%25` in literal form; `+` is ambiguous between query and path semantics. A filename containing any of these is safe to store in S3 but hostile to transport over an HTTP URL without escaping — and the escaping must be consistent with the SigV4 rule above.
- **Bucket hostname rules.** If the `storage_slug` is used as an S3 bucket name via virtual-hosted-style URLs, the slug must satisfy DNS-compliance: lowercase `[a-z0-9.-]`, 3–63 characters, no underscore, no uppercase, no leading/trailing hyphen. That is a different character class than what URL paths tolerate; the slug domain and the path domain must be kept separately compliant.
- **Key length budget.** S3 keys cap at 1024 bytes UTF-8. An owner-prefix scheme consumes ~80 bytes (tenant UUID + owner kind + owner UUID + separators) before the user's filename contributes a single byte. A UUID-only key consumes 36 bytes and is bounded regardless of input.
- **Case sensitivity and proxy behaviour.** S3 keys are case-sensitive; some SSL-terminating proxies lower-case the path; the round-trip is not case-safe unless every layer is known.
- **Empty segments, dot segments, trailing slash.** `a//b`, `a/./b`, `a/../b`, `a/` — each has at least one layer that normalises it and at least one that does not. The normalisation set chosen by the adapter becomes part of the external contract whether that is explicit or not.

Under Option B, none of these apply. The URL tail is a UUID; there is nothing to normalise, nothing to percent-encode differently, no key-budget arithmetic, no bucket-vs-path character-class reconciliation. The input alphabet is fixed at the module boundary, and every downstream layer sees the same bytes. This is not a marginal cleanup — it removes an entire category of production-incident classes from the module's future.

### Relation to ADR-0001 and the "S3-only" trade-off

Under ADR-0001's Option A (no DB), an opaque addressing scheme would have been infeasible — there would be nothing to resolve `file_id → S3 location, owner, authz context`. That made the "S3-only dependency" argument doubly important: it preserved a clean lightweight deployment **and** forced human-readable paths everywhere. The trade-off was a package deal.

ADR-0001 took a considered decision to pay the DB cost, because the P2/P3 roadmap (audit, quota, retention, versioning, ownership transfer, usage reporting, eventing) needs a metadata index regardless of this ADR's outcome. Since the DB cost is already paid, the argument here simplifies: any addressing scheme that does *not* exploit the resolver underuses an already-paid cost. Option B (UUID) uses the resolver fully and closes an entire class of encoding bugs as a bonus. Option A (human paths) works, but leaves the resolver underutilised and inherits all the encoding reconciliation we would otherwise have been paying for.

We do not split this into a per-deployment flag the way Option C of ADR-0001 considered splitting the DB. URL shape is not a per-deployment choice: cross-module handles, SDK trait signatures, and client integrations all key off one canonical URL form, and diverging it per backend would create a worse API than either endpoint of the spectrum.

### Semantic shifts under Option B / C

Opacity is not a drop-in swap for the current URL shape — a few existing semantics change and must be redesigned downstream of this ADR:

- **Upsert vs. create.** A path-based `PUT /files/{file_path}` would be a last-write-wins upsert. Under Option B, the lifecycle splits the two intentions explicitly: `create_presigned_url` always mints a fresh `file_id` and lets the partial unique index on `(tenant_id, backend_id, file_path) WHERE status = 'uploaded'` collapse colliding logical paths to the latest finalized row; in-place overwrite of an existing `file_id` is `put_file` (in-process SDK) or a fresh-`file_id` presigned PUT followed by `sync(file_id)`. The two operations are no longer the same call — a clarification that pays off in audit, quota, and concurrency reasoning.
- **In-place rename.** Without opacity, changing the logical path would mean copy-to-new-key + delete-old-key at the backend. Under Option B, it is a single metadata field update — this is a clear win but changes the rename's cost model and hence the `PUT /files/{file_id}` contract.
- **Cross-module handles.** Modules that carry a FileStorage handle (e.g., `chat-engine`, `llm-gateway`) hold `(file_id, etag)`. The `etag` is what they use to detect drift across asynchronous boundaries (antivirus scans `e1`, llm-gateway pins `e1`, the file is overwritten to `e2`, the next read fails fast with `EtagMismatch`).
- **Display-name lifetime.** With opacity, the "name a user typed" and the "key the backend stores" are independent fields. The display name can be refreshed by `PUT /files/{file_id}` without touching the backend key.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related ADR**: [ADR-0001](./0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md) — metadata index decision that makes this one feasible.

This decision directly addresses the following requirements and design elements:

* `cpt-cf-file-storage-fr-upload-file` — upload returns a `file_id` as the canonical handle; upsert semantics are redefined as part of this decision's downstream DESIGN update.
* `cpt-cf-file-storage-fr-download-file` — download resolves `file_id` to physical key and serves `Content-Type` / `Content-Disposition` from metadata; the URL carries no identity.
* `cpt-cf-file-storage-fr-get-metadata` — display name (`name`) is a metadata field, not a URL component.
* `cpt-cf-file-storage-fr-signed-urls` and `cpt-cf-file-storage-fr-direct-transfer` — presigned URLs use the same opaque physical key, inheriting URL opacity and the clean SigV4 canonical form end-to-end.
* `cpt-cf-file-storage-nfr-url-availability` — URL stability is strengthened: rename no longer invalidates the URL.
* `cpt-cf-file-storage-principle-file-id-address` — reinforces the file-id-as-canonical-address invariant with a stable, opaque identifier.
* `cpt-cf-file-storage-component-rest-api`, `cpt-cf-file-storage-component-s3-backend` — both shift to `file_id`-addressed operations.
