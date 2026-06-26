# PRD — BFF (Backend-for-Frontend) Gear


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Authentication & Login](#51-authentication--login)
  - [5.2 Session & Cookie](#52-session--cookie)
  - [5.3 Logout & Revocation](#53-logout--revocation)
  - [5.4 Protection & Integrity](#54-protection--integrity)
  - [5.5 Extensibility & Reuse](#55-extensibility--reuse)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

<!--
Product Requirements Document. Defines WHAT the BFF gear must do and WHY.
Implementation, architecture, and technology choices live in DESIGN.md / ADR/.
Requirement language: MUST/SHALL = mandatory; use priority p2/p3 instead of SHOULD/MAY.
-->
## 1. Overview

### 1.1 Purpose

The **BFF (Backend-for-Frontend) gear** provides browser-facing authentication and
session management for single-page applications (SPAs). It runs the OIDC
Authorization Code + PKCE flow as a confidential client on the server side, holds
all identity-provider (IdP) tokens server-side, and exposes the browser only an
opaque, hardened session cookie. It offers an `/auth/*` API for login, logout,
refresh, session listing/revocation, and CSRF, and a programmatic interface that
lets a co-hosted gateway turn a session cookie into an authenticated identity.

The gear is **product-agnostic and reusable**: the session storage backend is a
swappable plugin (e.g. Redis for production, in-memory for development), and the
IdP integration reuses the existing `authn-resolver` gear. Any consumer can link
the gear, choose a session-store plugin, and obtain a working cookie-based auth
layer for their SPA.

### 1.2 Background / Problem Statement

SPAs that store IdP tokens in the browser (sessionStorage / in-memory) and send
them as `Authorization: Bearer` headers expose those tokens to XSS, cannot revoke
a leaked token before expiry, and force the frontend to decode JWTs to derive
user identity. This is the pattern in use today across consuming products and it
is a recurring security and maintenance liability.

The BFF pattern removes tokens from the browser entirely: the server performs the
OIDC handshake, stores tokens and session state server-side, and the browser holds
only an opaque session reference in a `__Host-`-prefixed, `HttpOnly`, `Secure`,
`SameSite=Strict` cookie. Sessions become individually revocable, refresh is
explicit and server-controlled, and the frontend stops handling tokens.

Today the gears-rust workspace has an `api-gateway` gear (routing, middleware,
Bearer validation via `authn-resolver`) but **no cookie/session/OIDC-login layer**.
This gear fills that gap as a standalone, reusable unit rather than as
product-specific code.

### 1.3 Goals (Business Outcomes)

- Eliminate browser-held IdP tokens for consuming SPAs (zero tokens in
  `localStorage`/`sessionStorage`).
- Provide individually revocable sessions: a single session, or all sessions for a
  user, can be terminated and take effect within the session TTL.
- Ship a reusable gear adoptable by any product with two-line composition plus a
  session-store plugin choice — no auth logic re-implementation per product.
- Keep added authentication-path overhead within the gateway's latency budget so
  the BFF is viable in the request hot path.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| BFF | Backend-for-Frontend: server-side component that mediates auth between a browser SPA and IdPs/APIs. |
| SPA | Single-Page Application (browser frontend). |
| OIDC | OpenID Connect, the identity layer over OAuth 2.0. |
| PKCE | Proof Key for Code Exchange (RFC 7636) — protects the authorization code flow. |
| Session | Server-side record of an authenticated user, referenced by an opaque session ID held in a cookie. |
| Session store | Pluggable backend persisting session records (e.g. Redis, in-memory). |
| RP-initiated logout | Relying-Party-initiated OIDC logout: BFF redirects the browser to the IdP `end_session_endpoint`. |
| Back-channel logout | IdP-initiated logout delivered server-to-server (OIDC Back-Channel Logout). |
| CSRF | Cross-Site Request Forgery. |
| Refresh rotation | Replacing the session identifier on each explicit refresh to bound the value of a stolen cookie. |

## 2. Actors

### 2.1 Human Actors

#### End User

**ID**: `cpt-cf-bff-actor-end-user`

- **Role**: A person using a consuming SPA in a browser who authenticates via the
  organization's IdP and holds an active session.
- **Needs**: Sign in once via the IdP; remain signed in across page reloads and
  multiple tabs without re-entering credentials; sign out reliably; see and revoke
  their active sessions.

### 2.2 System Actors

#### SPA / Browser Client

**ID**: `cpt-cf-bff-actor-spa`

- **Role**: The consuming frontend. Initiates login by navigation, sends the
  session cookie on subsequent requests, primes a refresh timer from
  server-supplied values, and coordinates refresh across tabs.

#### Identity Provider (OIDC)

**ID**: `cpt-cf-bff-actor-idp`

- **Role**: External OIDC provider. Authenticates the user, issues tokens via the
  code exchange, supports RP-initiated logout, and may emit back-channel logout
  notifications.

#### Session Store Plugin

**ID**: `cpt-cf-bff-actor-session-store`

- **Role**: Pluggable backend that persists and expires session records and
  supporting indexes on behalf of the gear. Selected at composition time.

#### Co-hosted Gateway

**ID**: `cpt-cf-bff-actor-gateway`

- **Role**: The `api-gateway` (or equivalent) that serves `/api/*`. It consumes the
  BFF's session-resolution interface to convert a session cookie into an
  authenticated identity for downstream request handling.

#### Audit Sink

**ID**: `cpt-cf-bff-actor-audit`

- **Role**: External service receiving authentication lifecycle events (login,
  logout, refresh, revoke, back-channel logout).

## 3. Operational Concept & Environment

> Project-wide runtime, security, and lifecycle baselines are defined in
> [`docs/ARCHITECTURE_MANIFEST.md`](../../../../docs/ARCHITECTURE_MANIFEST.md) and
> [`guidelines/`](../../../../guidelines). This gear has no parent/root PRD. Only
> gear-specific deviations are listed below.

### 3.1 Gear-Specific Environment Constraints

- Requires a session-store plugin to be present at composition time; with no store
  configured the gear MUST refuse to start (fail-closed, see `cpt-cf-bff-nfr-fail-closed`).
- Requires HTTPS at the browser edge: the hardened cookie attributes
  (`__Host-` prefix, `Secure`) are only valid over TLS. TLS termination is provided
  by the deployment environment, not this gear.
- Requires a reachable OIDC IdP exposing standard discovery and code-flow endpoints.
- Assumes the SPA and the gateway are served same-site so the session cookie is
  first-party (a `SameSite=Strict` constraint, see `cpt-cf-bff-fr-cookie`).

## 4. Scope

### 4.1 In Scope

- Server-side OIDC Authorization Code + PKCE login as a confidential client.
- Server-side session lifecycle: create, validate, explicit refresh with rotation,
  expire, and revoke.
- Hardened, opaque session cookie issuance and clearing.
- `/auth/*` API: login, callback, refresh, logout, current-identity, session list,
  session revoke (one and all), CSRF token, back-channel logout receiver.
- A programmatic session-resolution interface consumed by a co-hosted gateway.
- Pluggable session storage (plugin contract + at least one production-grade and one
  development-grade plugin).
- Pluggable IdP integration by reuse of the `authn-resolver` gear for ID-token
  validation and identity extraction.
- Tenant association of sessions via the `tenant-resolver` gear.
- Emission of authentication lifecycle audit events.
- A defined SPA integration contract (cookie behavior, refresh timing semantics,
  CSRF, multi-tab coordination expectations).

### 4.2 Out of Scope

- Reverse proxying of `/api/*` and request routing — owned by the `api-gateway` gear.
- Minting and serving downstream/internal service JWTs (the "Router"/gateway-JWT
  role) and JWKS publication — a separable concern, not part of this gear's first
  release (tracked in Open Questions).
- The IdP itself and IdP administration.
- A development/fake IdP for local stands — a testing enabler that belongs to test
  infrastructure, not a product capability of this gear (see Assumptions).
- Suspicious-activity / anomaly **detection**, alerting, and step-up re-auth. v1
  *captures* device context (last-used, IP, user agent) and lists sessions, but acting
  on it (new-device detection, impossible-travel, notifications, step-up) is deferred to
  a later feature or a separate gear.
- The consuming SPA's own UI and routing.
- Authorization / policy decisions beyond producing an authenticated identity
  (owned by `authz-resolver`).

## 5. Functional Requirements

> All requirements verified via automated tests (unit, integration, e2e) at 90%+
> coverage unless a non-standard verification method is noted.

### 5.1 Authentication & Login

#### OIDC login initiation

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-login`

The system **MUST** start an OIDC Authorization Code flow with PKCE as a
confidential client, redirecting the browser to the IdP, and **MUST** carry
protection against CSRF and replay on the authorization request (per-attempt
state and nonce) and preserve a caller-supplied post-login return location.

- **Rationale**: Code + PKCE is the secure browser login flow; tokens never reach
  the browser.
- **Actors**: `cpt-cf-bff-actor-spa`, `cpt-cf-bff-actor-idp`

#### Login completion and session establishment

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-callback`

The system **MUST** complete the OIDC flow on the IdP redirect: validate the
returned state/nonce, exchange the authorization code for tokens server-side,
validate the ID token, resolve the authenticated identity and its tenant, create a
server-side session holding the IdP tokens, and establish the session cookie.

- **Rationale**: Turns a successful IdP handshake into a durable, server-held session.
- **Actors**: `cpt-cf-bff-actor-idp`, `cpt-cf-bff-actor-session-store`

### 5.2 Session & Cookie

#### Hardened session cookie

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-cookie`

The system **MUST** convey the session to the browser only as an opaque,
unguessable reference in a cookie carrying the `__Host-` prefix, `HttpOnly`,
`Secure`, `SameSite=Strict`, and `Path=/`, with no `Domain` attribute. The browser
**MUST NOT** receive any IdP token or any user identity claim in client-readable form.

- **Rationale**: Removes token exposure to XSS and confines the cookie to first-party use.
- **Actors**: `cpt-cf-bff-actor-spa`

#### Session resolution

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-resolve`

The system **MUST** provide a means to resolve a presented session reference into
the current authenticated identity (user, tenant, and session validity/expiry),
returning an unauthenticated result when the session is absent, expired, or revoked.

- **Rationale**: This is how the co-hosted gateway and the `/auth/me` endpoint learn
  who the caller is without the browser holding identity.
- **Actors**: `cpt-cf-bff-actor-gateway`, `cpt-cf-bff-actor-spa`

#### Explicit session refresh with rotation

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-refresh`

The system **MUST** extend a session's lifetime only by an explicit refresh
request (no sliding/passive extension), **MUST** rotate the session reference on
each refresh, **MUST** bound total session lifetime by a configurable absolute cap,
and **MUST** tolerate benign concurrent/duplicate refreshes (e.g. multiple tabs,
page reloads, retries) within a short grace window without forcing re-login.

- **Rationale**: Short TTL plus rotation bounds the value of a stolen cookie; the
  grace window prevents multi-tab races from logging users out.
- **Actors**: `cpt-cf-bff-actor-spa`, `cpt-cf-bff-actor-session-store`

#### Refresh timing contract

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-refresh-timing`

On session establishment and on refresh, the system **MUST** return a
server-supplied next-refresh deadline and absolute expiry, and the deadline
**MUST** be jittered so that refreshes across clients do not align to a predictable
instant. Clients use the server-supplied deadline rather than computing their own.

- **Rationale**: Server-controlled, jittered timing prevents thundering-herd and
  timing-aligned attacks; keeps refresh logic out of the frontend.
- **Actors**: `cpt-cf-bff-actor-spa`

### 5.3 Logout & Revocation

#### Logout

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-logout`

The system **MUST** support local logout (revoke the current session and clear the
cookie) and RP-initiated logout (provide the IdP end-session redirect so the IdP
session is also terminated).

- **Rationale**: A user signing out must end both the local session and the IdP session.
- **Actors**: `cpt-cf-bff-actor-spa`, `cpt-cf-bff-actor-idp`

#### Back-channel logout

- [ ] `p2` - **ID**: `cpt-cf-bff-fr-backchannel-logout`

The system **MUST** accept IdP-initiated back-channel logout notifications,
validate them, protect against replay, and revoke the matching session(s).

- **Rationale**: Lets the IdP terminate sessions (e.g. admin action, credential
  compromise) without a browser round-trip.
- **Actors**: `cpt-cf-bff-actor-idp`

#### Session listing

- [ ] `p2` - **ID**: `cpt-cf-bff-fr-session-list`

The system **MUST** let an authenticated user enumerate their active sessions with
enough context to recognize each device (creation time, expiry, last-used time, source
IP, a device/browser label derived from the user agent, and which one is current).

- **Rationale**: Transparency, device inventory, and a prerequisite for selective
  revocation and anomaly detection — a capability the opaque-session model provides on
  the per-request lookup that authentication already requires.
- **Actors**: `cpt-cf-bff-actor-end-user`

#### Session revocation

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-session-revoke`

The system **MUST** let an authenticated user revoke a specific session by
reference and revoke all of their sessions ("sign out everywhere"); revocation
**MUST** take effect for subsequent requests.

- **Rationale**: Core security control for lost devices or suspected compromise.
- **Actors**: `cpt-cf-bff-actor-end-user`

### 5.4 Protection & Integrity

#### CSRF protection

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-csrf`

The system **MUST** protect state-changing `/auth/*` operations against CSRF using
a token bound to the session in addition to the cookie's `SameSite=Strict` posture,
and **MUST** provide a way for the SPA to obtain the current token.

- **Rationale**: Defense-in-depth for state-changing auth operations.
- **Actors**: `cpt-cf-bff-actor-spa`

#### Expired-session housekeeping

- [ ] `p2` - **ID**: `cpt-cf-bff-fr-housekeeping`

The system **MUST** reclaim references to expired sessions from supporting indexes
so that listings and per-user indexes do not accumulate stale entries, coordinating
so that the work is not duplicated across replicas.

- **Rationale**: Keeps the per-user session index accurate and bounded over time.
- **Actors**: `cpt-cf-bff-actor-session-store`

### 5.5 Extensibility & Reuse

#### Pluggable session store

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-pluggable-store`

The system **MUST** define the session-store backend as a plugin selected at
composition time, **MUST** function with any conformant plugin without code
changes, and **MUST** ship at least one production-grade plugin and one
development-grade plugin.

- **Rationale**: The core reuse mechanism — consumers choose or supply a backend.
- **Actors**: `cpt-cf-bff-actor-session-store`

#### Pluggable IdP integration

- [ ] `p1` - **ID**: `cpt-cf-bff-fr-pluggable-idp`

The system **MUST** obtain ID-token validation and identity extraction through the
`authn-resolver` gear rather than embedding IdP-specific validation, so that the
set of supported IdPs is governed by `authn-resolver` plugins.

- **Rationale**: Reuses existing, plugin-based IdP support; avoids duplicating
  JWKS/validation logic per product.
- **Actors**: `cpt-cf-bff-actor-idp`

#### Audit of authentication events

- [ ] `p2` - **ID**: `cpt-cf-bff-fr-audit`

The system **MUST** emit an audit event for each authentication lifecycle action:
login, logout, refresh, revoke (single and all), and back-channel logout.

- **Rationale**: Security accountability and incident forensics.
- **Actors**: `cpt-cf-bff-actor-audit`

## 6. Non-Functional Requirements

> Project-wide NFRs (baseline security, observability, tenancy isolation) are
> inherited from [`docs/ARCHITECTURE_MANIFEST.md`](../../../../docs/ARCHITECTURE_MANIFEST.md)
> and [`guidelines/`](../../../../guidelines). Only gear-specific NFRs are listed.

### 6.1 Gear-Specific NFRs

#### Authentication-path latency

- [ ] `p1` - **ID**: `cpt-cf-bff-nfr-latency`

Session resolution on the request hot path **MUST** add no more than 15 ms at p95
under nominal load, exclusive of session-store round-trip time.

- **Threshold**: ≤ 15 ms p95 added latency, nominal load.
- **Rationale**: The BFF sits in front of every authenticated request; it must not
  dominate latency.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Cookie hardening

- [ ] `p1` - **ID**: `cpt-cf-bff-nfr-cookie-attrs`

100% of session-cookie responses **MUST** carry the `__Host-` prefix, `HttpOnly`,
`Secure`, `SameSite=Strict`, and `Path=/`, with no `Domain` attribute.

- **Threshold**: 100% of set-cookie responses conform.
- **Rationale**: Any non-conforming cookie reintroduces the exposure the gear exists
  to remove.

#### Fail-closed

- [ ] `p1` - **ID**: `cpt-cf-bff-nfr-fail-closed`

If the session store is unreachable or no store/IdP configuration is present, the
system **MUST** deny authentication (reject auth mutations and report not-ready)
rather than serve any degraded or cached-auth path. No per-user session state is
held in process memory as an authority.

- **Threshold**: Zero authenticated outcomes produced while the store is unreachable.
- **Rationale**: Auth must never silently weaken under partial failure.

#### Stateless horizontal scale

- [ ] `p1` - **ID**: `cpt-cf-bff-nfr-stateless`

The system **MUST** hold no authoritative per-user session state in process memory;
terminating any single replica **MUST NOT** sign any user out.

- **Threshold**: Session survives replica loss; any replica can serve any session.
- **Rationale**: Horizontal scalability and zero-downtime deploys.
- **Note**: This is satisfied only by a **shared/distributed** session-store plugin
  (e.g. Redis). The in-memory plugin is process-local and does **not** satisfy it — it
  is for development, tests, and single-instance use only.

#### Session lifetime bounds

- [ ] `p1` - **ID**: `cpt-cf-bff-nfr-session-ttl`

Session TTL and absolute lifetime cap **MUST** be configurable within bounded
ranges with safe short defaults; a stolen cookie's usable lifetime is limited to the
TTL plus the refresh grace window.

- **Threshold**: TTL and absolute cap configurable; defaults short (sub-hour TTL).
- **Rationale**: Bounds exposure from a leaked cookie.

#### Token & secret confidentiality

- [ ] `p1` - **ID**: `cpt-cf-bff-nfr-secret-confidentiality`

IdP tokens, session references, and CSRF tokens **MUST NOT** appear in logs, error
responses, or any browser-readable surface.

- **Threshold**: No secret material in logs or client-visible output.
- **Rationale**: Server-side token custody is the gear's reason to exist.

#### Abuse resistance on `/auth/*`

- [ ] `p2` - **ID**: `cpt-cf-bff-nfr-rate-limit`

The `/auth/*` surface **MUST** be protected against abuse by per-client rate
limiting and a bound on concurrent in-flight login attempts, such that sustained
request floods do not exhaust the session store or CPU.

- **Threshold**: Configurable per-client limit and concurrent-login cap; sustained
  flood does not degrade healthy traffic.
- **Rationale**: Login endpoints are a common DoS and credential-stuffing target.

### 6.2 NFR Exclusions

- **Persistent relational storage / migrations**: Not applicable — session state is
  ephemeral and lives in the session-store plugin, not a relational database.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Session-resolution interface

- [ ] `p1` - **ID**: `cpt-cf-bff-interface-session-resolver`

- **Type**: Rust gear/trait (SDK), registered via ClientHub.
- **Stability**: stable.
- **Description**: Resolves a presented session reference into an authenticated
  identity (user, tenant, validity). Consumed in-process by the co-hosted gateway
  and by the gear's own `/auth/me` handler. Satisfies `cpt-cf-bff-fr-resolve`.
- **Breaking Change Policy**: Major version bump required.

#### Session-store plugin interface

- [ ] `p1` - **ID**: `cpt-cf-bff-interface-session-store`

- **Type**: Rust plugin trait (SDK) discovered via the registry, implemented by
  session-store plugins.
- **Stability**: stable.
- **Description**: The contract a backend must implement to persist, look up,
  rotate, expire, index, and revoke sessions. Satisfies `cpt-cf-bff-fr-pluggable-store`.
- **Breaking Change Policy**: Major version bump required.

#### `/auth/*` HTTP API

- [ ] `p1` - **ID**: `cpt-cf-bff-interface-auth-api`

- **Type**: REST API (registered through the host gateway, documented via OpenAPI).
- **Stability**: stable.
- **Description**: Browser-facing endpoints covering login, callback, refresh,
  logout, current identity, session list, session revoke (one/all), CSRF token, and
  back-channel logout receiver. Concrete paths, methods, and payloads are specified
  in DESIGN.md, not here.
- **Breaking Change Policy**: Additive changes only within a major version.

### 7.2 External Integration Contracts

#### SPA integration contract

- [ ] `p1` - **ID**: `cpt-cf-bff-contract-spa`

- **Direction**: provided by library (to consuming SPAs).
- **Protocol/Format**: HTTP + cookie; server-supplied refresh timing.
- **Compatibility**: The SPA navigates to login, sends the cookie with credentials,
  primes its refresh timer from server-supplied values, echoes the CSRF token on
  state-changing requests, and coordinates refresh across tabs (single refresher per
  browser). Stable within a major version.

#### OIDC provider contract

- [ ] `p1` - **ID**: `cpt-cf-bff-contract-oidc`

- **Direction**: required from environment.
- **Protocol/Format**: OpenID Connect (discovery, Authorization Code + PKCE,
  RP-initiated logout; Back-Channel Logout where `cpt-cf-bff-fr-backchannel-logout` is enabled).
- **Compatibility**: Standards-conformant OIDC providers.

## 8. Use Cases

#### First-time sign-in

- [ ] `p1` - **ID**: `cpt-cf-bff-usecase-signin`

**Actor**: `cpt-cf-bff-actor-end-user`

**Preconditions**: User is unauthenticated; IdP reachable; session store available.

**Main Flow**:
1. SPA detects no session and navigates the browser to the login initiation endpoint.
2. BFF redirects to the IdP with a code+PKCE authorization request.
3. User authenticates at the IdP; IdP redirects back to the BFF callback.
4. BFF validates the response, exchanges the code, validates the ID token, resolves
   identity and tenant, creates a session, and sets the session cookie.
5. SPA loads its authenticated state from the current-identity endpoint.

**Postconditions**: A server-side session exists; the browser holds only the cookie.

**Alternative Flows**:
- **IdP denies / state mismatch**: BFF rejects the callback and returns the user to
  an unauthenticated state without creating a session.

#### Staying signed in across tabs

- [ ] `p2` - **ID**: `cpt-cf-bff-usecase-refresh`

**Actor**: `cpt-cf-bff-actor-spa`

**Preconditions**: An active session exists.

**Main Flow**:
1. SPA primes a timer from the server-supplied next-refresh deadline.
2. At the deadline, a single tab issues a refresh; the session reference rotates and
   a new deadline is returned.
3. Other tabs consume the rotation result rather than each refreshing.

**Postconditions**: Session lifetime extended within the absolute cap; cookie rotated.

**Alternative Flows**:
- **Concurrent refresh within grace**: A near-simultaneous refresh from another tab
  resolves to the rotated session without forcing re-login.
- **Refresh after expiry**: Session is gone; user is sent back to sign-in.

#### Sign out everywhere

- [ ] `p2` - **ID**: `cpt-cf-bff-usecase-revoke-all`

**Actor**: `cpt-cf-bff-actor-end-user`

**Preconditions**: User has one or more active sessions.

**Main Flow**:
1. User requests revocation of all sessions.
2. BFF revokes every session for the user and clears the current cookie.

**Postconditions**: All of the user's sessions are invalid for subsequent requests.

## 9. Acceptance Criteria

- [ ] A consuming SPA completes IdP sign-in and obtains an authenticated session
      with zero IdP tokens stored in the browser.
- [ ] Every session-cookie response conforms to the hardened-cookie attribute set.
- [ ] A revoked session (single or "all") is rejected on the next request.
- [ ] An explicit refresh rotates the session reference and returns a new
      server-supplied deadline; concurrent refreshes within the grace window do not
      log the user out.
- [ ] With the session store unreachable, no request is served as authenticated and
      readiness reports not-ready.
- [ ] The gear runs against at least two session-store plugins (production and
      development) with no change to gear code.
- [ ] All listed authentication lifecycle actions produce audit events.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `authn-resolver` gear | ID-token validation and identity extraction (IdP plugin host) | p1 |
| `tenant-resolver` gear | Resolves/validates the tenant associated with a session | p1 |
| Session-store plugin | Persists and expires session state | p1 |
| `api-gateway` gear | Hosts `/auth/*` routes and consumes session resolution for `/api/*` | p1 |
| `types-registry` gear | Plugin discovery/selection mechanism | p1 |
| `credstore` gear | Custody of OIDC client secret / any signing material | p2 |
| OIDC IdP | External authentication authority | p1 |
| Audit sink | Receives authentication lifecycle events | p2 |

## 11. Assumptions

- TLS is terminated at the deployment edge; the gear is reached over HTTPS so
  `__Host-`/`Secure` cookies are valid.
- The SPA and gateway are served same-site, satisfying `SameSite=Strict`.
- `authn-resolver` with a suitable IdP plugin is composed into the same binary.
- Exactly one logical session store is available per deployment.
- Local development uses a development session-store plugin and a development/fake
  IdP supplied by test infrastructure (outside this gear) so the real code path runs
  without external IdP infrastructure.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Session store is a hard auth dependency | Store outage = no logins/refresh | Fail-closed by design (`cpt-cf-bff-nfr-fail-closed`); rely on store HA at deployment. |
| Refresh-rotation race correctness | Spurious logouts or accepted stale cookies | Grace window + atomic rotation; isolated tests of the rotation path before wiring (detailed in DESIGN). |
| `SameSite=Strict` breaks cross-site deep links | Inbound links from external sites land unauthenticated | Accepted for v1; revisit a separate cookie class later (DESIGN/ADR). |
| OIDC provider behavior varies | Edge-case login failures per IdP | Delegate validation to `authn-resolver` plugins; conformance-test against target IdPs. |
| Gateway-JWT / downstream identity propagation deferred | Downstream services still need a trusted identity assertion | Tracked in Open Questions; current consumers use in-process `SecurityContext` from session resolution. |

## 13. Open Questions

- Where does the IdP-subject → internal-user mapping live: entirely within
  `authn-resolver` identity extraction, or via a separate identity service? Affects
  the shape of `cpt-cf-bff-fr-callback` and `cpt-cf-bff-fr-resolve`.
- Is the downstream/internal-service JWT mint + JWKS publication (the "Router"
  role) a future extension of this gear, an extension of `api-gateway`, or a
  separate gear? Out of scope for v1 but needs an owner.
- Is back-channel logout (`cpt-cf-bff-fr-backchannel-logout`) required for the first
  adopter, or can it follow? Drives p1/p2 sequencing.
- Default values for session TTL, absolute cap, refresh grace window, and jitter —
  to be fixed in DESIGN with rationale.
- Signing-algorithm posture for any gear-issued assertion: EdDSA is acceptable but
  not mandatory; the choice (and any FIPS implications) is deferred to an ADR.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
- **Upstream spec inputs**: Insight api-gateway/bff PRD & DESIGN (product-specific
  origin of this reusable gear), and `BFF.md` (cookie-auth handoff).
