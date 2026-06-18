---
status: accepted
date: 2026-06-18
---

# ADR-0004: Signed-URL Encoding & Transport

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Discrete fields in the query (chosen)](#a-discrete-fields-in-the-query-chosen)
  - [B. Opaque token in the query (rejected)](#b-opaque-token-in-the-query-rejected)
  - [C. Discrete fields in a header (chosen)](#c-discrete-fields-in-a-header-chosen)
  - [D. Opaque token in a header (rejected)](#d-opaque-token-in-a-header-rejected)
- [More Information](#more-information)
- [Option Comparison](#option-comparison)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-signed-url-transport`

## Context and Problem Statement

FileStorage authorizes every content operation with an Ed25519 **signed URL** verified by the sidecar
(`cpt-cf-file-storage-adr-sidecar-data-plane`, `cpt-cf-file-storage-design-signed-urls`). A signed request is a set of
**fields** — operation, resource (`file_id` in the path, `content_id`/`version_id`), `exp`, constraints (`ip`,
token-claim predicates, upload size/hash, P2 rate/conns), baked response headers — plus one **signature** over their
canonical form.

How those fields and the signature ride on the wire is **two orthogonal decisions**, which an earlier draft conflated:

1. **Encoding** — discrete named fields (one signature over all of them, S3 SigV4 style) **vs.** a single **opaque token
   blob** (e.g. PASETO) that hides the fields.
2. **Envelope / transport** — the **URL query string** **vs.** **HTTP headers**.

The earlier draft picked "discrete fields, in the query, header deferred". Review showed the two axes are independent and
that there are **two genuinely different access intents** whose needs conflict, so a single envelope cannot be optimal
for both.

## Decision Drivers

* **Bare, shareable URL** — a download link must work directly in a browser, `<img>`/`<video>`, `curl <url>`, or a media
  player issuing `Range`, with **no headers** the embedder cannot set
* **Programmatic / batch access** — SDK and server-to-server callers want the credential **out of the URL** (clean
  access logs / `Referer` / history), a **stable URL** across re-issue (CDN cache), and low per-request ceremony
* **Edge observability without cracking crypto** — CDN cache-key normalization, WAF rules, and access-log analytics
  should be able to read fields (e.g. `exp`, scope) **without decoding a token**
* **Fields are not secrets** — `exp`, tenant, hash, op are not capabilities; the **only** secret is the signature, which
  is present on the wire regardless of encoding. Hiding the *other* fields buys no security
* **One composite signature** — a single signature over the whole canonical field-set (not per-field)
* **Low-risk, proven shape** — prefer a design pattern with a long, broad production track record

## Considered Options

Two axes → four concrete combinations:

| | Query string | HTTP header |
|---|---|---|
| **Discrete fields** | **A** | **C** |
| **Opaque token** | B | D |

## Decision Outcome

**Encoding = discrete named fields** (`X-FS-*`) with one composite Ed25519 signature over the canonical string — **not**
an opaque token. **Transport = both envelopes, chosen by access intent:**

* **Query (A)** — the default. `X-FS-*` query parameters produce a **bare, shareable URL** for embeddable/anonymous-ish
  reads (browser, `<img>`/`<video>`, `curl`, media `Range`), where the caller cannot set headers.
* **Header (C)** — for **programmatic / SDK / batch** callers: the *same* `X-FS-*` fields and the *same* signature are
  carried as **request headers** instead of query parameters (analogous to S3 SigV4's `Authorization`-header form). This
  keeps the credential **out of the URL** (clean logs, no `Referer` leak), keeps the **URL stable** across re-issue
  (clean CDN cache key), and is tidy for batch.

Both envelopes carry the **identical field-set and signature**; the canonical string the signature covers is the same
regardless of where the fields ride, so the sidecar verifies a request whether the fields arrive as query params or as
headers. The caller (or SDK) picks the envelope by intent.

**Reject the opaque token (B and D)** in both envelopes. In the query it provides **no** advantage over discrete fields:
the supposed gains attributed to it are actually gains of the **header envelope**, not of the token encoding.
Specifically:

* "one signature, not per-field" is already true for discrete fields — SigV4-style signing produces a **single
  composite** signature over all params; discrete ≠ per-field;
* "hide fields from logs" gives **no security** — the fields are not secrets and the signature is in the URL either way;
  if a caller truly needs the credential out of the URL, that is the **header envelope**, not opacity;
* an opaque blob **loses edge observability** (CDN/WAF/logs can no longer read `exp`/scope without decoding) and is a
  non-standard format every edge component would have to learn.

S3 (and the many independent vendors that implemented the same SigV4 presigned scheme over ~20 years) is cited **only as
evidence that the discrete-fields + dual-envelope shape is sound and enterprise-sufficient** — note SigV4 itself offers
*both* a header form and a presigned-query form, both using discrete fields, never an opaque blob. **We do not adopt the
S3 wire format and make no claim of S3 compatibility.**

This supersedes the earlier draft's "query-only, header deferred" outcome: discrete fields are kept, and the header
envelope is promoted to a first-class peer of the query envelope.

### Consequences

* `cpt-cf-file-storage-design-signed-urls` (DESIGN §4.5), api.md, and the worked examples (§4.6/§4.7) carry the **same
  `X-FS-*` fields in two carriers**: query params (embeddable) or request headers (programmatic). The canonical signing
  input and verification are unchanged; only the carrier differs. The SDK chooses the header envelope by default for
  in-process/S2S calls and emits a query URL when a bare, embeddable link is requested.
* **Caching:** the query envelope changes the URL when re-issued (new `exp`) → CDN cache-key churn, mitigated by a long
  `max_url_ttl` and cache-key normalization; the header envelope keeps the URL stable → clean cache key. Programmatic
  callers that care about caching use the header envelope.
* **Credential exposure:** the query envelope puts the (short-lived, fully-constrained, bearer) signature in the URL —
  acceptable for embeddable links; programmatic callers use the header envelope to keep it out of URLs, logs, and
  `Referer`.
* **Debuggability is sanitized server-side structured logging** (log tenant/file ids and outcomes; **never** the
  signature or raw `exp`), independent of the envelope. It is explicitly **not** derived from field visibility in the
  URL — that earlier "pro" is removed as it contradicted the leak concern.
* **No opaque-token format** is introduced; no new dependency (PASETO/JWT), no per-edge token-format support burden.

### Confirmation

* Code review confirming the SDK emits and the sidecar verifies **discrete `X-FS-*` fields** with a single composite
  Ed25519 signature, from **either** the query string **or** request headers, over the same canonical string.
* Integration tests: (a) a query-envelope download URL is consumable as a **bare URL** (browser/`curl`/`<img>`) and
  serves `Range`; (b) a header-envelope request authorizes with no signing material in the URL; (c) re-issuing a
  header-envelope credential leaves the URL byte-identical (cache-friendly).
* Review confirming access logs/CDN/WAF can key on discrete fields without decoding, and that logging never records the
  signature or raw expiry.

## Pros and Cons of the Options

### A. Discrete fields in the query (chosen)

* Good, because it is a **bare, shareable URL** — embeddable, browser-openable, `curl`-able, `Range`-seekable, no headers
* Good, because edge components (CDN/WAF/logs) read fields **without decoding** a token
* Good, because it is REST-native and **versionable per field** (add a constraint param later, no format migration)
* Good, because it carries **one composite signature** (SigV4-style), not per-field
* Bad, because re-issuing changes the URL → CDN cache-key churn (mitigated: long `max_url_ttl`, cache-key normalization)
* Bad, because the signature appears in the URL (logs/`Referer`/history) — accepted for embeddable use; short, capped `exp`

### B. Opaque token in the query (rejected)

* Bad, because it gives **no security** over A — the fields are not secrets and the signature is in the URL regardless
* Bad, because it **loses edge observability** (CDN/WAF/logs can't read `exp`/scope without decoding) and is non-standard
* Bad, because it does not even solve the leak concern — the bearer token is still in the URL
* Neutral — the "single signature" it advertises is already provided by A; no real advantage remains

### C. Discrete fields in a header (chosen)

* Good, because the credential stays **out of the URL** — clean access logs, no `Referer`/history leak
* Good, because the **URL is stable** across re-issue → clean CDN cache key, batch-friendly
* Good, because edge/proxies can still read the discrete header fields for observability when needed
* Good, because it reuses the *same* fields and signature as A — no second contract, only a second carrier
* Bad, because it is **not a bare URL** — every caller must set headers, so it is unsuitable for embedding; that is
  exactly why A coexists

### D. Opaque token in a header (rejected)

* Good, because URL is stable and the credential is out of the URL (same as C)
* Bad, because it adds the opaque-blob downsides (no edge observability, non-standard, decode ceremony) on top of C for
  no gain — even S3's header form packs **discrete** fields, not an opaque blob
* Bad, because a bearer-blob-in-a-header invites treating it as a reusable credential, which it is not

## More Information

The two access intents have opposite constraints, so the design serves each with its own envelope rather than forcing
one:

* **Embeddable / browser-driven** — cannot set headers → **query** (A).
* **Programmatic / SDK / batch** — can set headers, wants the credential out of the URL and a stable cacheable URL →
  **header** (C).

AWS S3 SigV4 demonstrates exactly this split — a header (`Authorization`) form and a presigned-**query** form, both built
from **discrete fields** — and has done so across AWS and many independent implementations for ~20 years. We take that as
evidence the *shape* (discrete fields, single composite signature, dual envelope) is sound and enterprise-sufficient. We
**do not** implement the S3 wire format and make **no** S3-compatibility claim; our hosts, field names, and semantics are
our own.

## Option Comparison

✓ = yes / good · ✗ = no / bad · ~ = partial

| Aspect | A · fields/query | B · token/query | C · fields/header | D · token/header |
|---|---|---|---|---|
| Bare, shareable URL (no headers) | ✓ | ✓ | ✗ | ✗ |
| Credential kept out of the URL (logs/Referer) | ✗ | ✗ | ✓ | ✓ |
| Stable URL across re-issue → clean CDN cache | ✗ | ✗ | ✓ | ✓ |
| Edge observability without decoding (CDN/WAF/logs) | ✓ | ✗ | ✓ | ✗ |
| REST-native / per-field versionable | ✓ | ✗ | ✓ | ✗ |
| One composite signature | ✓ | ✓ | ✓ | ✓ |
| **Verdict** | **Chosen (embeddable)** | Rejected | **Chosen (programmatic)** | Rejected |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related**: [ADR-0003: Split the Data Plane into a Signed-URL Sidecar](./0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-storage-fr-signed-urls` — fixes the encoding (discrete fields) and transport (query + header) of the signed-URL fields and signature
* `cpt-cf-file-storage-design-signed-urls` — the canonical signing input is unchanged; fields ride in the query (embeddable) or in headers (programmatic)
* `cpt-cf-file-storage-principle-signed-urls` — control-minted, sidecar-verified discrete-field credential, one composite signature
* `cpt-cf-file-storage-nfr-bandwidth` — the header envelope gives programmatic callers a stable, cache-friendly URL
