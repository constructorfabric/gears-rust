---
status: accepted
date: 2026-06-26
decision-makers: BFF gear authors
---

# Opaque server-side sessions; no JWT minting in v1


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Opaque server-side session; no JWT in v1](#opaque-server-side-session-no-jwt-in-v1)
  - [JWT-in-cookie](#jwt-in-cookie)
  - [Opaque cookie + mint downstream JWT now](#opaque-cookie--mint-downstream-jwt-now)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bff-adr-opaque-sessions-no-jwt`
## Context and Problem Statement

The browser needs a credential to reference its authenticated session. Two shapes are
common: a self-contained signed token (JWT) in the cookie, or an opaque reference to
server-side state. Separately, the upstream Insight design has a "Router" that mints
an EdDSA gateway-JWT for downstream services. What should the browser hold, and does
the BFF mint any JWT in v1?

## Decision Drivers

* Instant revocability (`cpt-cf-bff-fr-session-revoke`) and short stolen-cookie lifetime.
* No identity material readable by the browser (`cpt-cf-bff-fr-cookie`, `cpt-cf-bff-nfr-secret-confidentiality`).
* v1 scope excludes the Router / downstream-JWT role (PRD §4.2).
* Workspace crypto posture is FIPS-oriented; EdDSA is acceptable but not mandated, and
  introducing signing now would force an algorithm/FIPS decision for no v1 benefit.

## Considered Options

* Opaque server-side session reference; no JWT minted in v1
* JWT-in-cookie (self-contained signed session)
* Opaque cookie **and** mint a downstream gateway-JWT in the BFF now

## Decision Outcome

Chosen option: **opaque server-side session reference, no JWT minted in v1**, because
it gives instant revocation, keeps all identity server-side, and — since the Router
role is out of scope — removes any signing-algorithm/FIPS decision from v1 entirely.

### Consequences

* The cookie carries only an opaque `SessionId`; identity is resolved server-side via
  `SessionResolver` (DESIGN §3.3). Downstream consumers use the in-process
  `SecurityContext`, not a token.
* The gear has no JWT-signing dependency and serves no JWKS in v1.
* If/when the downstream-JWT (Router) role is adopted, it will consume
  `SessionResolver` and add signing; the EdDSA-vs-ES256/FIPS trade-off is decided in a
  dedicated ADR **at that time**, not now.
* Revocation is a store delete; it takes effect on the next resolve.
* The mandatory per-request lookup is also the substrate for **device/session
  inventory**: the session record carries `user_agent`, `ip`, `created_at`, and a
  `last_used_at` updated (coalesced, no TTL slide) on the same pipelined round-trip,
  powering `/auth/sessions` and future anomaly detection (new device/IP) — capabilities
  JWT cannot offer without its own liveness store.

### Confirmation

Code review confirms no signing crate / JWKS endpoint in v1; a revocation test shows a
revoked session is rejected on the next request.

## Pros and Cons of the Options

### Opaque server-side session; no JWT in v1

* Good: instant revocation; nothing sensitive in the browser; no crypto/FIPS decision now; smallest v1.
* Good: the per-request store lookup enables device/session inventory, `last_used_at`, and an anomaly-detection substrate — for free on a lookup that is mandatory anyway.
* Bad: every request needs a store lookup (one round-trip, `cpt-cf-bff-nfr-latency`) plus a coalesced `last_used_at` write — unavoidable given revocation + device-management requirements, so not a real disadvantage versus JWT here.

### JWT-in-cookie

* Good: stateless validation, no per-request store lookup.
* Bad: cannot revoke before expiry; identity readable if decoded; reintroduces signing/FIPS decision; conflicts with `cpt-cf-bff-fr-session-revoke`.
* Bad: statelessness is illusory once revocation or device/session management is required — those force a per-request liveness/denylist check anyway, erasing the round-trip saving while still lacking a server-side record to attach device/last-used/IP to.

### Opaque cookie + mint downstream JWT now

* Good: ready for downstream propagation immediately.
* Bad: pulls the out-of-scope Router role into v1; forces the EdDSA/FIPS decision prematurely; larger surface.

## More Information

Aligns with Insight DD-BFF-01 (opaque session) and DD-BFF-05 (EdDSA) — the latter
deferred here because v1 mints no token.

## Traceability

* PRD: `cpt-cf-bff-fr-cookie`, `cpt-cf-bff-fr-session-revoke`, `cpt-cf-bff-nfr-secret-confidentiality`; Open Question on signing posture
* DESIGN: [DESIGN.md](../DESIGN.md) §1.1, §3.3
