---
status: accepted
date: 2026-06-16
---

# ADR-0003: Split the Data Plane into a Signed-URL Sidecar

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Single monolith proxies all content (prior proxy-all design)](#single-monolith-proxies-all-content-prior-proxy-all-design)
  - [Direct-to-backend presigned URLs (rejected by the prior proxy-all design)](#direct-to-backend-presigned-urls-rejected-by-the-prior-proxy-all-design)
  - [Signed-URL sidecar data plane (chosen)](#signed-url-sidecar-data-plane-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-sidecar-data-plane`

## Context and Problem Statement

the original proxy-all design made FileStorage a single
in-process monolith through which **every byte of every upload and download** flows. It chose this
over direct-to-backend presigned URLs to preserve backend opacity, per-byte metering, and uniform
audit / policy coverage. the prior proxy-all design consciously accepted the cost: FileStorage becomes a
terabyte-scale data-plane bottleneck whose bandwidth — not CPU or memory — is the binding capacity
constraint (`cpt-cf-file-storage-nfr-bandwidth`).

That cost is the problem. Coupling the control logic (auth, metadata, policy, versioning,
conditional requests) to the byte-moving data plane in one process means the whole stack must scale
on the bandwidth dimension, the data plane cannot be moved closer to heavy consumers or the edge
without relocating the control logic with it, and a slow client streaming a 50 GB object ties up a
process slot that also serves cheap metadata reads.

The question is whether the byte-moving data plane can be separated from the control plane
**without** giving up the properties the prior proxy-all design was protecting (backend opacity, central metering,
uniform enforcement) — i.e. without falling back to the direct-to-backend presigned-URL model that
the prior proxy-all design rejected.

## Decision Drivers

* Backend opacity — clients must never depend on a backend protocol surface (S3, Azure Blob REST,
  WebDAV) or learn a backend-addressable URL (`cpt-cf-file-storage-principle-backend-opacity`)
* Centralized per-byte metering for usage reporting (`cpt-cf-file-storage-fr-usage-reporting`) and
  uniform content-type validation, audit, and read audit on 100% of transfers — no per-flow
  carve-outs
* Independent scaling of the data plane — byte-moving capacity must scale (and relocate to the edge)
  without dragging the control logic and its database connections along
* Operability of long transfers — a slow or large transfer must not consume a control-plane request
  slot or hold a metadata-DB connection
* A single client protocol regardless of backend, so SDKs and any future facade gears stay uniform
* The cost the prior proxy-all design accepted (`cpt-cf-file-storage-nfr-bandwidth`) should be confined to the part of
  the system that actually moves bytes, not the part that makes decisions

## Considered Options

* Single monolith proxies all content (the prior proxy-all status quo)
* Direct-to-backend transfer via presigned URLs (the option the prior proxy-all design rejected)
* **Signed-URL sidecar data plane** — split into a control plane that issues signed URLs and a
  data-plane sidecar that moves the bytes

## Decision Outcome

Chosen option: **"Signed-URL sidecar data plane"**. FileStorage is split into two cooperating
planes:

* **Control plane** — the FileStorage **API / SDK**. Owns metadata, authorization, versioning, and
  conditional-request semantics. **Its HTTP REST surface never accepts or returns file content.**
  It issues short-lived **signed URLs** that point at the sidecar. The only path where control-side
  code touches bytes is the in-process **SDK proxy mode**, which streams inside the *consumer
  gear's* process — never through the control-plane service.
* **Data plane** — the **sidecar**. It has its own domain and URL and is the only component that
  moves user bytes. It is connected to N storage backends and validates the signed-URL signature
  (and a platform token only when the signed URL carries a token-claim predicate — see DESIGN §3.2).
  **P1 path (explicit):** the sidecar reaches the control plane **over the FS SDK in s2s REST mode**
  (its own app-token plus an on-behalf-of `<user>` claim) to pre-register and bind a version — it
  holds **no** direct database connection and is a thin, stateless byte-mover. This trades a little
  per-op latency (an extra control round-trip at pre-register/bind) for a clean security/failure
  boundary: the data plane never gets DB credentials and cannot mutate metadata except through the
  control plane's authorized operations. The alternative **direct-DB** mode — the sidecar as a full
  FileStorage instance over the shared metadata DB (lower latency, no control round-trip) — is
  **deferred to P2** as a co-located-deployment optimization, behind the same FS SDK interface.

The critical difference from the direct-to-backend model the prior proxy-all design rejected: **the signed URL points
at our own sidecar, never at the raw backend.** Therefore every property the prior proxy-all design protected is
retained — they simply move into the sidecar, which is platform-controlled infrastructure:

* backend opacity — the client only ever talks to the sidecar; backend identity, native URLs, and
  protocol never leak;
* per-byte metering, content-type validation, audit, and read audit run on 100% of transfers, in
  the sidecar, with no per-flow carve-outs;
* a single client protocol independent of backend.

Every operation is therefore **at least two HTTP requests**: a control request (obtain a signed
URL) plus one or more data requests against the sidecar.

Signed URLs are **Ed25519, stateless** (S3-presigned-style): the control plane signs with a private
key and is the **sole minter**; the sidecar verifies with the public key and can never forge a URL.
Constraints are AND-combined into the signed payload — `exp` (required, capped at a configured
`max_url_ttl`, recommended 7 days, enforced by the control plane at signing), optional
`ip`/CIDR, optional predicates over token claims (`tok.typ`, `tok.sub`, `tok.tenant_id`, …), and — on
upload URLs — an optional size bound (`max_size` or `exact_size`, mutually exclusive) and
`expected_hash`. Bandwidth (`max_rate`) and connection (`max_conns`) caps are declared but enforced in
P2. In P1 there is one static keypair (private in control config, public in sidecar config);
rotation/keyset is deferred to P2; there is no per-URL revocation — emergency revocation is the
platform auth module's token revocation, not the URL layer.

### Consequences

* The bandwidth cost the prior proxy-all design accepted (`cpt-cf-file-storage-nfr-bandwidth`) is now confined to the
  sidecar. The data plane scales (and can be relocated to the edge / co-located with heavy
  consumers) by adding stateless sidecar replicas, independently of the control plane. The
  DESIGN.md "bandwidth escape hatch" stops being a thought experiment and becomes the architecture.
* `cpt-cf-file-storage-fr-rest-api` is restated: the control-plane REST surface carries metadata and
  signed URLs only; **content endpoints live on the sidecar**, addressed by signed URL.
* Content I/O becomes a two-request dance (presign + transfer), reflected in every upload/download
  sequence (`cpt-cf-file-storage-seq-*`). Upload uses an **immutable-blob + pointer** model:
  content is written to an immutable backend object `/{file_id}/{version_id}`; the file's current
  content is a DB pointer (`content_id`) swapped under optimistic CAS — see DESIGN §4.x and
  `cpt-cf-file-storage-fr-upload-file`. A new version is a new object plus a pointer swap; backend
  content is never mutated in place.
* `cpt-cf-file-storage-fr-content-type-validation`, `cpt-cf-file-storage-fr-usage-reporting`, and
  `cpt-cf-file-storage-fr-read-audit` are reallocated from the monolith to the sidecar; coverage
  stays 100%.
* `cpt-cf-file-storage-fr-range-requests` are served by the sidecar; a single signed download URL
  serves many `Range` requests (random access) because the `Range` header is not part of the
  signature.
* The sidecar acts on the control plane under its **own app-token plus an on-behalf-of `<user>`**
  claim; authorization for metadata writes (e.g. version bind) is decided against the delegated
  user (`cpt-cf-file-storage-fr-authorization`).
* A new signed-URL contract (`cpt-cf-file-storage-fr-signed-urls`) and the constraint model become
  part of the public surface; the response-header set the sidecar must echo verbatim is baked into
  the signed URL.
* The OoP/gRPC SDK escape hatch in `cpt-cf-file-storage-constraint-toolkit-gear` is reframed: an
  out-of-process caller is handed a signed URL (or proxied via the sidecar), not streamed through
  the control plane.

### Confirmation

Implementation verified via:

* Code review confirming the control-plane REST surface neither reads nor writes a request/response
  body containing file content (only metadata + signed URLs).
* Code review confirming the sidecar is the only component that opens backend clients for content
  I/O, and that no signed URL or SDK return value exposes a backend-addressable URL to a client.
* Code review confirming the control plane is the sole signer (holds the Ed25519 private key) and
  the sidecar only verifies (holds the public key).
* Integration tests covering presign → transfer for upload and download, the bind/rebind CAS path
  (including the `412` retry that does not re-upload bytes), and signed-URL constraint enforcement
  (expiry, ip, token-claim predicates).
* Usage reports include per-byte ingress/egress counters emitted by the sidecar.

## Pros and Cons of the Options

### Single monolith proxies all content (prior proxy-all design)

* Good, because one component, one deployment, no signed-URL machinery, no two-request dance
* Good, because all of the prior proxy-all design's properties hold trivially (everything is in one place)
* Bad, because control logic and the byte data plane scale on the same (bandwidth) dimension
* Bad, because the data plane cannot be relocated to the edge or a heavy consumer without moving the
  control logic and its DB connections with it
* Bad, because long/slow transfers consume control-plane request slots and DB connections

### Direct-to-backend presigned URLs (rejected by the prior proxy-all design)

* Good, because FileStorage carries no content bandwidth at all
* Bad, because the client must speak N backend protocols; backend identity leaks through the URL;
  per-byte metering, audit, and content validation fragment into per-flow carve-outs — the exact
  reasons the prior proxy-all design rejected it. **Not reconsidered here.**

### Signed-URL sidecar data plane (chosen)

* Good, because it keeps every property the prior proxy-all design protected (backend opacity, central metering,
  uniform enforcement) — the signed URL points at our sidecar, not the backend
* Good, because the data plane scales and relocates independently of the control plane
* Good, because the control plane stays thin: metadata + authz + signed URLs, no byte streaming, no
  request slot held for the duration of a transfer
* Good, because the immutable-blob + pointer model makes versioning backend-agnostic and makes
  conflict retries cheap (re-bind a `version_id`, never re-upload bytes)
* Bad, because every operation is at least two HTTP requests (presign + transfer)
* Bad, because it introduces signed-URL machinery (signing, verification, key distribution, the
  constraint model) and a delegation path (sidecar acting on-behalf-of the user)
* Bad, because there are now two deployable units (control plane + sidecar) and a shared metadata DB
  contract between them

## More Information

This realizes, as the baseline architecture, the "full-FileStorage-instance escape hatch" that
the prior proxy-all design's accepted-cost discussion (`cpt-cf-file-storage-nfr-bandwidth`,
`cpt-cf-file-storage-topology-overview`) described as a future option. The sidecar is not a
byte-mover trait extracted from the monolith; it is a full FileStorage data plane over the shared
(or remote) metadata DB, which is why it needs no wire-contract change to relocate.

ADR-0002 (content hash selection: SHA-256 in P1, configurable in P2) is unaffected — hashing still
happens on the streaming path, now in the sidecar.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related**: [ADR-0002: Content Integrity Hash](./0002-cpt-cf-file-storage-adr-content-hash-selection.md)

> Supersedes the original proxy-all monolith design (its ADR was removed; see `git log` for the history).

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-storage-fr-rest-api` — control REST carries metadata + signed URLs only; content lives on the sidecar
* `cpt-cf-file-storage-fr-signed-urls` — new: the Ed25519 stateless signed-URL contract and constraint model
* `cpt-cf-file-storage-fr-upload-file` / `cpt-cf-file-storage-fr-download-file` — two-request presign + transfer; immutable-blob + pointer model
* `cpt-cf-file-storage-fr-range-requests` — served by the sidecar; one signed URL, many ranges
* `cpt-cf-file-storage-fr-usage-reporting`, `cpt-cf-file-storage-fr-content-type-validation`, `cpt-cf-file-storage-fr-read-audit` — reallocated to the sidecar, still 100% coverage
* `cpt-cf-file-storage-nfr-bandwidth`, `cpt-cf-file-storage-nfr-scalability` — the bandwidth dimension is confined to the independently-scaled sidecar
* `cpt-cf-file-storage-fr-authorization` — sidecar acts under app-token + on-behalf-of user (delegation)
