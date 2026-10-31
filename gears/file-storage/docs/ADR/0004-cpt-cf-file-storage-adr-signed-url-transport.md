---
status: accepted
date: 2026-06-20
---

# ADR-0004: Signed-URL Token Format & Transport

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [Implementation note (P2, 2026-07)](#implementation-note-p2-2026-07)
  - [Claim-set evolution (P2 1.11, 2026-07)](#claim-set-evolution-p2-111-2026-07)
- [Token Opacity Contract](#token-opacity-contract)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Encoding: opaque token (chosen) vs discrete fields (rejected)](#encoding-opaque-token-chosen-vs-discrete-fields-rejected)
  - [Transport: query and header (both adopted)](#transport-query-and-header-both-adopted)
- [More Information](#more-information)
- [Option Comparison](#option-comparison)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-signed-url-transport`

## Context and Problem Statement

FileStorage authorizes every content operation with a control-minted credential verified by the sidecar
(`cpt-cf-file-storage-adr-sidecar-data-plane`, `cpt-cf-file-storage-design-signed-urls`). That credential is a set of
**claims** — operation, resource (`file_id`, `content_id`/`version_id`), `exp`, constraints (`ip`, token-claim
predicates, upload size/hash, P2 rate/conns), baked response headers — plus a **signature** over them.

Two decisions, previously conflated and re-litigated during review:

1. **Encoding** — carry the claims as **discrete named fields** (one signature over the canonical string, S3 SigV4
   style) **or** as a **single opaque token** that bundles claims + signature into one self-contained string.
2. **Transport / envelope** — **URL query string** **vs.** **HTTP header**.

Earlier drafts chose discrete fields. After team review we **reverse to an opaque token**. The decisive premise:
**FileStorage is deliberately not S3-wire-compatible** — our own host, parameter names, semantics, and crypto (ECDSA P-256/SHA-256,
not HMAC) mean no S3 client, CDN, or tool can ever generate or consume our credential (see *More Information*).
Therefore the *only* parties that ever need to read the claims are the **control plane** (which mints) and the
**sidecar** (which verifies). With no external reader to serve, discrete-field readability is not an asset — it is a
liability that would couple intermediaries to a layout we want free to change.

## Decision Drivers

* **No S3 / external compatibility** — nothing outside control+sidecar parses the credential, so on-the-wire field
  readability buys nothing and only risks coupling
* **Atomicity** — a single self-contained credential is simpler to pass, store, log-redact, and rotate, is signed and
  verified as **one unit**, and cannot be partially stripped, reordered, or tampered
* **Format encapsulation & evolvability** — keeping the credential opaque to everyone but control+sidecar lets us change
  the claim-set *and* the signature/encryption scheme over time **without coordinating with browsers, CDNs, proxies, or
  consuming apps**
* **Sidecar cannot mint** — asymmetric signature: control signs with the private key, the sidecar only verifies with the
  public key
* **Two access intents** — an embeddable **bare URL** (browser, `<img>`/`<video>`, `curl`, media `Range`) and a
  **programmatic / batch** path, both must work
* **A fixed signature scheme** — no caller-controlled algorithm negotiation

## Considered Options

* **Encoding:** opaque token **vs.** discrete fields
* **Token standard (if token):** **PASETO `v4.public`** vs. JWT vs. a bespoke format
* **Transport:** query **vs.** header **vs.** both

## Decision Outcome

* **Encoding = one opaque, atomically-signed token.**
* **Token format = private ECDSA P-256/SHA-256 compact token**, not JWT or PASETO.
  `base64url(json(claims)).base64url(ASN.1-DER-signature)` signs the original JSON bytes.
  The claim set and transport remain unchanged. There is no algorithm negotiation or `kid`.
* **Transport = both query and header**, chosen by access intent; the **token bytes are identical** either way:
  * **query** — `?fs-token=<token>` — for bare, embeddable URLs (browser, `<img>`/`<video>`, `curl`, media `Range`; the caller
    cannot set headers). Because a query token can leak via server/proxy logs, browser history, and the `Referer`
    header, and P1 has **no per-token revocation**, query-token URLs **MUST** be issued with a **very short TTL**
    (the short `default_url_ttl`, minutes — see DESIGN §4.5); they are for immediate fetch/embed, not durable links.
    Long-lived, shareable, or anonymous links are explicitly **out of P1** and belong to the future **FileShare/P3**
    surface, which will own its own revocation, expiry, and download-count semantics;
  * **header** — `X-FS-Token: <token>` — for programmatic / SDK / batch callers: keeps the credential out of the URL
    (clean logs / no `Referer` leak) and the **URL stable** across re-issue (clean CDN cache). (The token is **never**
    carried in `Authorization` — that header always carries the standard platform JWT.)
  * the query parameter is named **`fs-token`** and the header **`X-FS-Token`**.
  * **Either envelope alone is accepted.** When both are present and agree, that shared value is used. When both are
    present and *disagree*, the sidecar rejects the request outright with **`400 Bad Request`** ("conflicting
    fs-token in query and header") rather than picking one silently — a caller or intermediary that attached two
    different credentials to the same request has a bug worth surfacing, not a query-wins/header-wins ambiguity to
    resolve quietly. When neither is present, the sidecar rejects with `401 Unauthorized`. Callers should send
    exactly one envelope.
* **Why a token, not discrete fields:** because we are not S3-compatible, the discrete-field benefits (external
  readability, edge/CDN/WAF/tooling interop, S3-shape familiarity) are moot — and they would lock intermediaries to our
  field layout. The token is **atomic** (signed/verified/rotated as one unit) and **opaque**, which is what makes the
  format **freely evolvable** (see the Token Opacity Contract).

The private token replaces the earlier discrete fields while preserving both query and header transport.
Only the control plane can sign; the sidecar holds the public key.

### Consequences

* `cpt-cf-file-storage-design-signed-urls` (DESIGN §4.5), api.md, and the worked examples (§4.6/§4.7) change: the
  discrete `X-FS-*` parameters are replaced by a **single token** carried as `?fs-token=<token>` (query) or an
  `X-FS-Token` header. SigV4-style canonical-string signing is replaced by **asymmetric mint (control) / verify
  (sidecar) — the private compact token specified under [Implementation note](#implementation-note-p2-2026-07),
  not a literal PASETO library**; all claims live inside the token.
* **Crypto boundary.** The token codec calls `SignatureProvider` / `SignatureVerifier`;
  `EcdsaP256Provider` delegates cryptographic operations to `toolkit_security::crypto`.
  The codec does not depend directly on a primitive crate or backend.
* **FIPS posture.** Key generation, signing and verification use AWS-LC FIPS on Linux,
  Apple corecrypto on macOS, and CNG on Windows. Missing backend witnesses fail startup;
  ordinary-mode fallback is forbidden. Deployment claims still depend on the module's
  operational environment and security policy.
* **Observability/debuggability** is **sanitized server-side structured logging** by control/sidecar (tenant/file ids,
  outcome; never the token or raw claims). It is **not** done by decoding the token at the edge.
* **Embeddable vs. leak vs. cache:** query envelope (token in URL, short `exp`) for embeddable; header envelope (token
  out of URL, stable cacheable URL) for programmatic.

### Confirmation

* Code review confirming the control plane mints the signed token (ECDSA P-256/SHA-256, the bespoke codec-equivalent-to-PASETO
  format described in the Implementation note below) and the sidecar verifies it with the public key, and that
  **no component other than control and sidecar parses the token**.
* Integration tests: the token authorizes via **query** (bare URL, `Range` works) and via **header** (no signing
  material in the URL); a deliberate claim-set / format-version bump verifies end-to-end **without changing any
  intermediary** (browser/CDN/proxy/SDK pass it through unchanged).

### Implementation note (P2, 2026-07)

The codec is `base64url(json(claims)).base64url(ecdsa_der_signature)`.
The control plane holds a PKCS#8 DER P-256 private key. The sidecar receives only
its uncompressed SEC1 public point (`0x04 || X || Y`, 65 bytes). Signatures use
SHA-256 and ASN.1 DER. Tokens are opaque to clients and intermediaries.

The original implementation used Ed25519. It now uses P-256 so the same algorithm
is available across all three selected native FIPS providers. This is a breaking
key and token change: deploy a new `signing_key_pkcs8` and matching
`FS_SIDECAR_PUBLIC_KEY` together. Old Ed25519 seeds/public keys/tokens are rejected.
See [migration procedure](../operations.md#ed25519-to-p-256-migration).

### Claim-set evolution (P2 1.11, 2026-07)

The `Claims` struct gained two fields since the implementation note above: `content_type`
and `etag`, both `String`, `#[serde(default, skip_serializing_if = "String::is_empty")]`. They are populated only on a
download (`op = get`) token — the control plane stamps the version's stored MIME type and its content ETag
(`domain::etag::content_etag`) into the claims at `download-url` issuance time — so that the sidecar, which has no DB
access, can emit real `Content-Type`/`ETag` response headers instead of a generic `application/octet-stream` fallback
with no `ETag` at all. Upload (`op = put`) and multipart-part (`op = multipart_part`) tokens never populate them
(always the empty string, which the `skip_serializing_if` keeps out of the serialized payload).

This is exactly the kind of claim-set change the Token Opacity Contract below anticipates: it required coordinated
changes only to the minter (control plane) and verifier (sidecar), no change to any intermediary, and it is
version-skew-tolerant in both directions by construction — `#[serde(default)]` means a sidecar running this code
verifies a token minted before this change (falls back exactly as it did before), and a sidecar running the prior
code simply ignores the two new fields on a token minted after this change (same as the pre-existing `request_id`
(P2 1.8) and `backend_handle` (P2 1.7) fields, which established this pattern first).

## Token Opacity Contract

This is a hard interface boundary, not a nicety:

* The token's internal format — its **claim-set, encoding, and signature/encryption scheme** — is known **only to the
  control plane (minter) and the sidecar (verifier)**.
* **Every other participant** that sees or forwards the token — browser, CDN, reverse proxy, API gateway, the consuming
  app/LMS, logging/telemetry, the SDK transport layer — **MUST treat it as an opaque, custom byte string**: forward it
  verbatim, and **never parse, base64-decode, inspect, cache-key on, or depend on any part of it**.
* The format **can and will change** — fields may be added, removed, or renamed, and the signature/encryption method may
  be swapped — coordinated **only** between control and sidecar (which deploy together). Anything that parsed the token
  would break on such a change; **opacity is precisely the contract that lets the format evolve without a cross-system
  migration**.
* Therefore: **do not** build CDN/WAF/router/log rules on token internals. Any needed observability comes from
  control/sidecar emitting sanitized structured logs (they know the format), never from decoding the token elsewhere.
* Note on secrecy: PASETO `v4.public` is **signed, not encrypted**, so the payload is technically base64-decodable. That
  does **not** weaken this contract — opacity here is an **encapsulation / evolvability boundary**, enforced by
  convention and by a deliberately-changing format, not a secrecy guarantee. (Field values such as `exp` were never
  secrets anyway; the only secret is the signing key, held solely by control.)

## Pros and Cons of the Options

### Encoding: opaque token (chosen) vs discrete fields (rejected)

**Opaque token (PASETO v4.public) — chosen:**

* Good, because it is **atomic** — one signed unit, impossible to partially strip/reorder/tamper, trivial to pass and rotate
* Good, because the format is **private to control+sidecar and therefore freely evolvable** — claim-set and crypto can
  change with zero coordination with intermediaries (the core driver)
* Good, because no intermediary couples to our field layout; the credential is just bytes everywhere else
* Good, because the codec fixes P-256/SHA-256 without algorithm negotiation; the sidecar cannot mint
* Bad, because it is not human-readable on the wire — **by design**; debugging is server-side, not by eyeballing a URL
* Bad, because the edge cannot read claims — **by design**; this is the encapsulation we want
* Bad, because the token is signed rather than encrypted (payload decodable) — addressed by the Opacity Contract, not relied on for secrecy

**Discrete fields — rejected:**

* Their only real advantages are external readability, edge/CDN/WAF/tooling interop, and S3-shape familiarity — **all
  moot** because we are not S3-wire-compatible and explicitly **do not want** intermediaries reading or depending on our
  fields
* They cannot evolve the format freely: adding/renaming a field or changing crypto is an externally-visible wire change
* (They do remain trivially edge-observable — but we are replacing that with sanitized server-side logging on purpose)

### Transport: query and header (both adopted)

* **Query (`?fs-token=<token>`)** — Good: a bare, shareable URL usable in a browser/`<img>`/`curl`/media `Range` with no
  headers. Bad: the token sits in the URL (logs/`Referer`/history — mitigated by short, capped `exp`) and the URL changes
  on re-issue (CDN cache-key churn).
* **Header (`X-FS-Token`)** — Good: token out of the URL (clean logs), stable URL across re-issue
  (clean cache), tidy for batch/SDK. Bad: not a bare URL — the caller must set headers, so it cannot be embedded.

Both are adopted; the caller (or SDK) picks by intent — query for embedding, header for programmatic.

## More Information

The codec uses toolkit-security's ECDSA API directly. It does not implement PASETO
framing or pre-authentication encoding and must not be advertised as PASETO.

**Why not mimic S3's discrete-field shape:** S3 SigV4 (and its many re-implementations) deliberately uses readable
discrete params because S3 *is* an open, multi-client wire contract. We are the opposite: a closed credential between
two components we control. Our crypto (ECDSA P-256/SHA-256 vs HMAC-SHA256), param semantics, and resource addressing (the URL points
at our sidecar, not a backend) already make S3 compatibility impossible — so we gain nothing by imitating its shape, and
an opaque, evolvable token serves our actual two-party, evolvable contract far better.

## Option Comparison

✓ = yes / good · ✗ = no / bad

| Aspect | Token + query (chosen) | Token + header (chosen) | Fields + query | Fields + header |
|---|---|---|---|---|
| Bare, shareable URL (no headers) | ✓ | ✗ | ✓ | ✗ |
| Credential kept out of the URL | ✗ | ✓ | ✗ | ✓ |
| Stable URL across re-issue (cache) | ✗ | ✓ | ✗ | ✓ |
| Format evolvable without external coupling | ✓ | ✓ | ✗ | ✗ |
| Atomic credential (one signed unit) | ✓ | ✓ | ✗ | ✗ |
| One signature | ✓ | ✓ | ✓ | ✓ |
| **Verdict** | **Chosen (embeddable)** | **Chosen (programmatic)** | Rejected | Rejected |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related**: [ADR-0003: Split the Data Plane into a Signed-URL Sidecar](./0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-storage-fr-signed-urls` — the credential is a PASETO `v4.public` token carried in the query or a header
* `cpt-cf-file-storage-design-signed-urls` — claims move inside the token; private-token mint/verify replaces canonical-string signing
* `cpt-cf-file-storage-principle-signed-urls` — control-minted (private key), sidecar-verified (public key); the sidecar cannot mint
* `cpt-cf-file-storage-nfr-bandwidth` — the header envelope gives programmatic callers a stable, cache-friendly URL
