# PRD — Token Issuer

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
  - [5.1 Capability Token Issuance](#51-capability-token-issuance)
  - [5.2 Grant Token Issuance](#52-grant-token-issuance)
  - [5.3 OBO Re-Mint](#53-obo-re-mint)
  - [5.4 Key Publication and Rotation](#54-key-publication-and-rotation)
  - [5.5 Signing Plugin Selection](#55-signing-plugin-selection)
  - [5.6 Configuration and Error Reporting](#56-configuration-and-error-reporting)
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

## 1. Overview

### 1.1 Purpose

The Token Issuer is a system gear that mints short-lived, ES256-signed JSON Web Tokens for three
distinct purposes inside the platform, and publishes the JWKS and discovery documents that let any
party verify them offline. It is the single place where platform-issued bearer credentials come into
existence, and it deliberately holds no private key material of its own — every signature is produced
by a signing plugin discovered through the GTS types-registry.

The gear issues three token classes, each with its own issuer identifier and its own signing key so
that a token of one class can never be accepted as another: **capability tokens** (`cap+jwt`, minted
per caller context with a reuse cache), **grant tokens** (`grant+jwt`, bound to exactly one adapter
audience and a closed set of operations, verified offline by that adapter), and **on-behalf-of
tokens** (OBO, a down-scoped re-mint of a presented capability token, gated off by default).

### 1.2 Background / Problem Statement

Platform components and external resource adapters need to call each other with credentials that are
narrower and shorter-lived than a user's session token. Passing the user's original bearer token
onward gives the callee the caller's full authority for the token's full lifetime — too much
authority, for too long, with no way to attenuate it per hop.

Minting such credentials ad hoc in each gear would scatter signing-key access, claim conventions, and
JWKS publication across the codebase. Every consumer would need its own key custody story, and every
verifier would need to know which of several key sets to trust for which purpose. Rotation would have
to be coordinated by hand.

Centralizing issuance in one gear makes the trust surface small and auditable: one component holds
the signing-plugin handles, one component decides claim shape, and verifiers fetch one well-known
JWKS per token class. Because the gear signs through a plugin port rather than a local key, the
private keys stay in whatever backend the deployment chose (an `OpenBao` Transit mount, an HSM) and
the issuer process never sees them.

### 1.3 Goals (Business Outcomes)

- Every platform-issued bearer credential is short-lived and independently verifiable offline, so a
  leaked token has a bounded blast radius and verification adds no round-trip to the issuer.
- No private signing key material is resident in the issuer process, so compromising the issuer does
  not yield the ability to forge tokens beyond its configured key handles.
- Cross-class token confusion is cryptographically impossible: a capability token can never be
  accepted where a grant token is required, and vice versa.
- A capability-token mint for a repeated caller context costs no signing round-trip while the cached
  token remains comfortably valid, keeping the hot path cheap.
- Adapter callbacks can obtain an attenuated credential without the platform ever handing an adapter
  the caller's original authority.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Capability token (`cap+jwt`) | Short-lived JWT asserting a verified caller context plus an optional operation and resource type, addressed to one audience. |
| Grant token (`grant+jwt`) | Data-plane JWT bound to a single adapter audience, a specific RMS resource, and a closed operation set; verified offline by the adapter. |
| OBO token | On-behalf-of token produced by re-minting a presented capability token with a narrowed scope set, bound to the calling peer. |
| Down-scope | Intersection of an adapter's operator-granted allowlist with the presenting capability token's scopes, optionally narrowed further by the caller. |
| Adapter | An external resource-management component that verifies grant tokens offline and may call back into the platform under an OBO token. |
| JWKS | JSON Web Key Set — the public keys a verifier uses to check signatures, published per token class. |
| `kid` | Key identifier inside the signed JWT header, of the form `{key-name}-v{version}`, naming the exact signing-key version used. |
| Signing plugin | A GTS-registered plugin implementing `SigningClientV1`; owns private key custody and performs the actual signature. |
| Reuse floor | Remaining-TTL threshold below which a cached capability token is re-minted instead of reused. |
| Clock skew | Allowance, in seconds, for clock disagreement between issuer and verifier when checking expiry. |
| Loop guard | Refusal to mint from a bearer token that was itself minted by the OBO issuer, preventing OBO chains. |
| Transit mount | Path of the signing backend's key store, as understood by the signing plugin. |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-token-issuer-actor-operator`

- **Role**: Configures the gear per deployment — the public issuer base URL, the per-class signing key
  names and Transit mount, token lifetimes, clock-skew allowance, the signing-plugin vendor, and
  whether OBO issuance is enabled at all.
- **Needs**: Configuration that fails fast and loudly on a nonsensical combination rather than
  producing tokens with unsafe lifetimes; a deployment that refuses traffic rather than serving an
  unverifiable JWKS.

### 2.2 System Actors

#### Consumer Gear

**ID**: `cpt-cf-token-issuer-actor-consumer-gear`

- **Role**: An in-process gear that obtains `TokenIssuerClientV1` from the `ClientHub` and mints
  capability or grant tokens on behalf of a verified caller. The `grants` gear is the primary
  grant-token consumer; it resolves the resource identity and clamps the lifetime before asking for a
  mint.

#### Signing Plugin

**ID**: `cpt-cf-token-issuer-actor-signing-plugin`

- **Role**: A GTS-registered plugin implementing the `SigningClientV1` port. Holds private key
  custody, returns a signature plus the key version that produced it, and enumerates current public
  key versions for JWKS construction.

#### Types Registry

**ID**: `cpt-cf-token-issuer-actor-types-registry`

- **Role**: Resolves the active signing-plugin instance for the configured vendor, so the issuer
  binds to a signing backend by GTS type and vendor rather than by compile-time dependency.

#### Resource Adapter

**ID**: `cpt-cf-token-issuer-actor-adapter`

- **Role**: An external component that fetches the grant JWKS and verifies presented grant tokens
  offline, enforcing `resource_id`, `resource_name`, `resource_type`, and the granted operation set
  itself. When permitted, it calls back into the platform presenting a capability token to obtain a
  down-scoped OBO token.

#### External Verifier

**ID**: `cpt-cf-token-issuer-actor-verifier`

- **Role**: Any party that fetches a published JWKS or discovery document to validate a token this
  gear minted, without contacting the issuer per verification.

#### RMS Adapter Registry

**ID**: `cpt-cf-token-issuer-actor-rms-registry`

- **Role**: Authoritative source of adapter facts used by the OBO path — the adapter's GTS ID,
  whether it is active, whether OBO callbacks are granted, and its scope allowlist. Not yet exposed
  as a client; the integration seam is present and fails closed.

## 3. Operational Concept & Environment

Runtime, lifecycle policy, and integration patterns are defined at the repository level in
[docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational
[guidelines/](../../../../guidelines/). This gear has no parent gear PRD. Only gear-specific
deviations are recorded here.

### 3.1 Gear-Specific Environment Constraints

- Requires a reachable signing backend fronted by a GTS-registered `SigningClientV1` plugin whose
  vendor matches the configured `vendor`. Without one the gear stays not-ready and serves 503.
- Requires the public `issuer_base_url` to be the URL at which verifiers can actually reach the
  published JWKS and discovery documents, because it is baked into the `iss` claim of every minted
  token and into the discovery payload.
- The OBO re-mint surface requires an external mTLS terminator that supplies the verified client
  certificate subject. Until that layer is wired, peer identity resolution fails closed and the OBO
  surface is expected to remain disabled.
- Depends on the `types_registry` gear being initialized first, declared via
  `#[toolkit::gear(deps = [types_registry])]`.

## 4. Scope

### 4.1 In Scope

- Minting capability tokens (`cap+jwt`) for a verified caller context, with a get-or-mint reuse cache
  keyed on the caller context.
- Minting data-plane grant tokens (`grant+jwt`) bound to one adapter audience, one RMS resource, and a
  closed operation set.
- Re-minting a presented capability token into a down-scoped OBO token, behind a configuration gate,
  with peer binding, allowlist intersection, loop prevention, and idempotent retry.
- Publishing a JWKS and an OIDC-style discovery document per enabled token class.
- Selecting the signing plugin by GTS type and configured vendor, and signing through that port.
- Producing a `kid` that names the exact signing-key version used, and keeping the published JWKS
  current when the backend rotates keys.
- Readiness gating: refusing traffic until signing keys are readable and a non-empty JWKS is
  buildable.
- Validating the configuration's internal consistency at startup.
- Emitting metrics for cache behaviour, signing volume, signing failures, and mint latency.

### 4.2 Out of Scope

- **Private key custody, generation, and rotation** — owned by the signing plugin and its backend.
  This gear only references keys by name and reads public key versions.
- **Authorization decisions** — the gear mints what it is asked for once its gates pass. Deciding
  whether a caller may perform an operation belongs to the authz resolver; an OBO token carries
  identity and scope, not a policy verdict, and the policy decision point re-checks at use time.
- **Authentication of the original caller** — the caller's identity arrives already verified in the
  `SecurityContext`.
- **mTLS termination and client-certificate validation** — supplied by an external layer.
- **The RMS adapter registry itself** — this gear consumes adapter facts; it does not own or serve
  them.
- **Token revocation** — tokens are short-lived and verified offline; there is no revocation list.
- **Verification of grant tokens** — performed offline by the adapter, not by this gear. The gear
  verifies capability tokens only, and only on the OBO re-mint path.
- **Persistent storage** — all caches are in-memory and per-process; nothing is written to a database.

## 5. Functional Requirements

> **Testing strategy**: All requirements are verified via automated unit and integration tests.
> Referenced test names are the authoritative acceptance evidence.

### 5.1 Capability Token Issuance

#### Mint capability token

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-mint-cap`

The system **MUST** mint an ES256-signed `cap+jwt` for a verified caller context, carrying the caller
subject, home tenant, and optional user type from the `SecurityContext`, the context tenant, optional
project, audience, optional operation, and optional resource type from the request, and a fresh `jti`
per mint.

The token's `scopes` **MUST** be inherited from the caller's own token scopes in the `SecurityContext`,
canonicalized (ordered, de-duplicated, space-joined) — never taken from the request. This is what makes
attenuation meaningful: a capability token can carry no authority the caller did not already hold, and
the OBO down-scope in § 5.3 intersects against exactly these scopes.

- **Rationale**: Consumers need an attenuated, independently verifiable credential to present to a
  narrower audience than the caller's original token addresses.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::service::tests::mint_capability_produces_es256_cap_jwt`,
  `domain::claims::tests::builds_cap_claims_from_context`

#### Reject malformed mint requests

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-validate-cap-request`

The system **MUST** reject a capability mint request, without signing, unless `audience` is non-empty
and `audience`, `operation`, and `resource_type` each satisfy the field contract of
`cpt-cf-token-issuer-fr-field-contract`.

- **Rationale**: A malformed request must not consume a signing round-trip or produce a token whose
  claims are meaningless.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::service::tests::mint_capability_rejects_invalid_request`

#### Constrain free-form mint-request fields

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-field-contract`

Every free-form string field of a capability or grant mint request **MUST** be non-empty, at most 256
characters, and composed only of ASCII alphanumerics and `. - _ : / ~ *`.

- **Rationale**: The charset is deliberately wide enough for GTS identifiers — which use `.`, `~`, and
  `_` — and for scope and path-like operation ids, while excluding whitespace, quotes, and control
  characters that could confuse a downstream verifier parsing a claim. The length bound keeps an
  oversized field from reaching the signing backend inside a claim set.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::service::tests::mint_capability_rejects_invalid_request` covers
  the capability side (empty, over-long at 257 characters, and a rejected charset). **The grant side
  is not directly covered**: `validate_grant_request` shares the same `is_valid_field` predicate, but
  only its empty-operations branch is tested. See the open question on this gap.

#### Reuse a cached capability token above the reuse floor

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-cap-reuse`

The system **MUST** return the cached capability token for an identical caller context while its
remaining lifetime exceeds `cap_reuse_floor_secs`, and **MUST** mint a fresh one once it does not.

- **Rationale**: Repeated calls in the same context must not each cost a signing round-trip, while a
  token near expiry must not be handed to a caller who would then present it after it lapses.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::cache::tests::get_or_mint_reuses_within_floor`,
  `domain::cache::tests::get_or_mint_resigns_when_stale`

#### Distinguish cache entries by full caller context

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-cap-cache-key`

The system **MUST** key the capability reuse cache on the canonicalized claim set, such that a
differing operation or resource type yields a distinct entry, and **MUST** canonicalize scopes by
ordering and de-duplication before hashing.

- **Rationale**: A cache that collapsed distinct contexts would hand a caller a token asserting an
  authority it did not request.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::claims::tests::cache_key_differs_on_operation_and_resource_type`,
  `domain::claims::tests::cache_key_hashes_canonical_scopes`,
  `domain::claims::tests::canonicalizes_scopes_order_dedup_join`,
  `domain::claims::tests::scopes_hash_is_stable`

#### Refuse a capability mint driven by an OBO bearer

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-cap-loop-guard`

The system **MUST** refuse a capability mint when the inbound bearer token was itself minted by the
OBO issuer.

- **Rationale**: Defense in depth against an OBO → capability → OBO chain that would launder an
  attenuated credential back up to full authority.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::service::tests::mint_capability_refuses_under_obo_bearer`,
  `domain::loopguard::tests::obo_issuer_bearer_is_reentry`,
  `domain::loopguard::tests::other_issuer_bearer_is_not_reentry`,
  `domain::loopguard::tests::malformed_bearer_is_not_reentry`,
  `domain::loopguard::tests::missing_bearer_is_not_reentry`

### 5.2 Grant Token Issuance

#### Mint grant token

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-mint-grant`

The system **MUST** mint an ES256-signed `grant+jwt` addressed to exactly one adapter audience,
carrying the resolved resource identity (`resource_id`, `resource_name`, `resource_type`), the closed
granted operation set, the caller subject and home tenant, and the resource's owning tenant as the
authorization anchor — and **MUST** return its absolute expiry alongside the token.

- **Rationale**: Adapters verify grants offline and need every enforcement input inside the token.
  Returning the expiry avoids forcing the caller to re-decode the JWT to answer "until when".
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`, `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::service::tests::mint_grant_produces_es256_grant_jwt`,
  `domain::service::tests::mint_grant_includes_project_id_when_present`

#### Reject a grant with no operations

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-grant-nonempty-ops`

The system **MUST** reject a grant mint whose operation set is empty, and **MUST** apply the field
contract of `cpt-cf-token-issuer-fr-field-contract` to `audience`, `resource_name`, `resource_type`,
and every operation id.

- **Rationale**: A grant token authorizing nothing is a credential with no purpose; issuing one
  invites a verifier to misread absence as permissiveness. The per-operation check matters because
  each operation id is itself the RBAC action the adapter enforces offline.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::service::tests::mint_grant_rejects_empty_operations`

#### Apply the default grant lifetime

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-fr-grant-default-ttl`

The system **MUST** apply the configured `grant_ttl_secs` when the request supplies no explicit
lifetime.

- **Rationale**: The consuming gear clamps the lifetime to the smallest per-operation maximum; the
  configured value is the fallback upper bound when no clamp is supplied.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`
- **Acceptance Evidence**: `domain::service::tests::mint_grant_defaults_ttl_when_zero`

#### Mint every grant freshly

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-fr-grant-no-reuse`

The system **MUST NOT** reuse grant tokens across mints; each grant carries a fresh `jti` and is
signed anew.

- **Rationale**: Each grant is unique to a request-specific resource and operation set, so a reuse
  cache could only ever return a token asserting the wrong scope.
- **Actors**: `cpt-cf-token-issuer-actor-consumer-gear`

### 5.3 OBO Re-Mint

> The whole subsection is inert unless `obo.enabled` is set. When it is unset, the OBO issuer routes
> are not registered and re-mint attempts are refused.

#### Gate the OBO surface on configuration

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-gate`

The system **MUST** refuse re-mint and **MUST NOT** register the OBO JWKS, discovery, or re-mint
routes unless `obo.enabled` is set.

- **Rationale**: A deployment that does not use OBO must not expose the surface at all, so an
  unconfigured issuer presents no attack surface.
- **Actors**: `cpt-cf-token-issuer-actor-operator`
- **Acceptance Evidence**: `domain::service::tests::remint_disabled_is_obo_disabled`,
  `api::rest::routes::tests::obo_routes_absent_when_disabled`,
  `api::rest::routes::tests::obo_jwks_served_when_enabled`

#### Verify presented capability-token provenance

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-provenance`

The system **MUST** verify the presented capability token against the capability JWKS it serves,
requiring ES256, `typ == "cap+jwt"`, a `kid` resolvable in that JWKS, the configured capability
issuer, and an expiry within the allowed clock skew.

- **Rationale**: Re-minting from an unverified token would let anyone mint an OBO token for any
  audience.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::cap_verify::tests::verifies_cap_token_against_cap_jwks`,
  `domain::cap_verify::tests::rejects_tampered_signature`,
  `domain::cap_verify::tests::rejects_unknown_kid`,
  `domain::cap_verify::tests::rejects_wrong_issuer`,
  `domain::cap_verify::tests::rejects_wrong_typ`,
  `domain::cap_verify::tests::rejects_expired_cap_token`,
  `domain::service::tests::remint_rejects_bad_cap_signature`,
  `domain::service::tests::remint_rejects_expired_cap`,
  `domain::service::tests::obo_remint_rejects_grant_jwt_cross_class`

#### Bind the capability token to the calling peer

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-peer-binding`

The system **MUST** resolve the calling peer's adapter GTS ID from its verified mTLS client
certificate and **MUST** refuse the re-mint unless that ID equals the capability token's audience.
Resolution **MUST** fail closed when no verified certificate is present.

- **Rationale**: Without this binding, any adapter holding any capability token could re-mint for a
  different adapter's audience.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::service::tests::remint_rejects_peer_mismatch`,
  `domain::peer_identity::tests::fails_closed_without_certificate`,
  `domain::peer_identity::tests::resolves_known_cert_subject_to_gts_id`,
  `domain::peer_identity::tests::unknown_subject_is_peer_unknown`

#### Require an active, OBO-granted adapter

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-adapter-eligibility`

The system **MUST** refuse the re-mint unless the resolved peer is a registered adapter that is
active and has OBO callbacks granted.

- **Rationale**: OBO callback capability is an operator grant per adapter, not a platform-wide
  default.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`, `cpt-cf-token-issuer-actor-rms-registry`
- **Acceptance Evidence**: `domain::service::tests::remint_rejects_unknown_peer`,
  `domain::service::tests::remint_rejects_inactive_adapter`,
  `domain::service::tests::remint_rejects_obo_not_enabled_on_adapter`

#### Down-scope the grant

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-downscope`

The system **MUST** compute the OBO scope set as the intersection of the adapter's allowlist with the
capability token's scopes — treating a capability wildcard as the whole allowlist — optionally
narrowed to a caller-requested subset. A request that is not a subset of the computed grant, or a
computed grant that is empty, **MUST** be refused. The wildcard **MUST NOT** appear in the minted
token.

- **Rationale**: The OBO token must never carry authority beyond both what the operator granted the
  adapter and what the presented capability token itself held. Emitting a wildcard would defeat
  attenuation entirely.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::downscope::tests::intersection_when_no_wildcard`,
  `domain::downscope::tests::wildcard_cap_grants_full_allowlist`,
  `domain::downscope::tests::wildcard_never_appears_in_output`,
  `domain::downscope::tests::requested_subset_narrows_result`,
  `domain::downscope::tests::requested_must_be_subset_of_granted`,
  `domain::downscope::tests::empty_intersection_is_error`,
  `domain::service::tests::remint_happy_path_mints_downscoped_obo`,
  `domain::service::tests::remint_rejects_empty_intersection`,
  `domain::service::tests::remint_rejects_requested_exceeding_grant`,
  `domain::service::tests::remint_rejects_empty_requested_scope_set`,
  `domain::obo::tests::build_obo_claims_copies_and_downscopes`,
  `domain::obo::tests::obo_scope_is_space_joined_and_never_wildcard`

#### Refuse OBO re-entry

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-loop-guard`

The system **MUST** refuse a re-mint whose presented token was itself minted by the OBO issuer.

- **Rationale**: Prevents an OBO-on-OBO chain, where each hop could re-widen or indefinitely extend
  an attenuated credential.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::service::tests::remint_rejects_obo_reentry_loop_guard`

#### Return a byte-identical token on retry

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-obo-idempotency`

The system **MUST** return the byte-identical OBO token for a repeated re-mint with the same
capability token and the same computed scope set, for as long as that capability token remains
acceptable — including throughout the clock-skew window past its expiry. A differing scope set
**MUST** produce a distinct entry.

- **Rationale**: A retried adapter callback must not churn fresh credentials. Keying the entry's
  lifetime to the capability token's acceptance horizon rather than its bare expiry is what makes the
  guarantee hold for a retry inside the skew window.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::service::tests::remint_is_idempotent_by_cap_jti_and_scope`,
  `domain::service::tests::remint_idempotent_within_cap_skew_window`,
  `domain::obo_cache::tests::same_key_returns_same_token_minting_once`,
  `domain::obo_cache::tests::reuses_entry_within_cap_skew_window`,
  `domain::obo_cache::tests::different_scope_set_is_a_distinct_entry`,
  `domain::obo_cache::tests::entry_past_cap_exp_is_evicted_and_reminted`,
  `domain::obo_cache::tests::expired_obo_is_reminted_in_place`

#### Decouple OBO lifetime from the capability token

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-fr-obo-independent-ttl`

The system **MUST** set the OBO token's expiry to its own configured lifetime from the mint instant,
independent of the presented capability token's remaining lifetime.

- **Rationale**: The OBO token conveys identity and scope, not a policy verdict; the decision point
  re-checks authority at use time. A re-mint shortly before the capability token lapses should still
  yield a usable credential.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `domain::obo::tests::build_obo_claims_exp_is_decoupled_from_cap_exp`,
  `domain::obo::tests::sign_obo_produces_es256_obo_jwt_with_versioned_kid`

### 5.4 Key Publication and Rotation

#### Publish a JWKS per enabled token class

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-publish-jwks`

The system **MUST** publish, without authentication, a JWKS and an OIDC-style discovery document for
each enabled token class, with the discovery document pointing at that class's JWKS.

- **Rationale**: Offline verification requires public, unauthenticated key retrieval; requiring a
  credential to fetch verification keys would make verification circular.
- **Actors**: `cpt-cf-token-issuer-actor-verifier`, `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `api::rest::routes::tests::cap_jwks_always_served`,
  `api::rest::routes::tests::grant_jwks_and_discovery_always_served`,
  `domain::service::tests::discovery_documents_point_at_jwks`,
  `domain::service::tests::grant_discovery_points_at_jwks`,
  `domain::jwks::tests::builds_ec_jwk_from_pem`,
  `domain::jwks::tests::jwks_document_collects_versions`,
  `domain::jwks::tests::rejects_invalid_pem`

#### Isolate key sets per class

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-class-key-isolation`

The system **MUST** use a distinct signing key and issuer identifier per token class, and a key
belonging to one class **MUST NOT** appear in another class's JWKS.

- **Rationale**: Makes cross-class acceptance cryptographically impossible rather than merely
  discouraged by a `typ` check.
- **Actors**: `cpt-cf-token-issuer-actor-verifier`
- **Acceptance Evidence**: `domain::service::tests::grant_kid_is_isolated_from_the_cap_jwks`

#### Name the exact signing key version in `kid`

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-stable-kid`

The system **MUST** emit a `kid` that names the signing-key version that actually produced the
signature, and **MUST** fail the mint rather than return a token whose `kid` names a different
version.

- **Rationale**: `kid` lives inside the signed header and cannot be known before signing. If the
  backend rotates between learning the version and producing the final signature, a naively assembled
  token would be unverifiable — so the mint must retry and ultimately fail closed.
- **Actors**: `cpt-cf-token-issuer-actor-verifier`
- **Acceptance Evidence**: `domain::jws::tests::kid_matches_stable_signing_version`,
  `domain::jws::tests::re_signs_when_version_rotates_once`,
  `domain::jws::tests::fails_closed_when_version_never_stabilizes`

#### Keep the JWKS current across rotation

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-jwks-refresh-on-rotation`

When a mint produces a token whose `kid` is absent from the published JWKS, the system **MUST**
rebuild that JWKS. For **capability** tokens it **MUST** refuse the mint — before caching the token —
and for **grant** tokens it **MUST** refuse the mint, if the `kid` still cannot be published
afterwards. The **OBO** re-mint path refreshes its JWKS but does not refuse.

- **Rationale**: Caching a capability token whose key a verifier cannot find would guarantee downstream
  rejection for the whole reuse window, and a grant token is verified offline by an adapter that has no
  recourse. Failing the single mint is strictly cheaper than either. The OBO path's weaker guarantee is
  documented in DESIGN.md § 3.6.
- **Actors**: `cpt-cf-token-issuer-actor-verifier`
- **Acceptance Evidence**: `domain::service::tests::mint_rebuilds_cap_jwks_on_unseen_key_version`

#### Gate readiness on a buildable JWKS

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-readiness-gate`

The system **MUST** remain not-ready — serving 503 — until a non-empty JWKS is buildable for the
**capability** and **grant** classes, and additionally for **OBO** when `obo.enabled`, retrying with
capped exponential backoff. It **MUST NOT** publish an empty or invalid JWKS, and **MUST** abandon the
retry promptly on shutdown.

A missing or unreadable `grant_key_name` therefore blocks readiness just as a missing `cap_key_name`
does.

- **Rationale**: The signing backend may register after boot. A one-shot warm would permanently brick
  the issuer on a startup race, while publishing an empty JWKS would make every minted token
  unverifiable.
- **Actors**: `cpt-cf-token-issuer-actor-operator`
- **Acceptance Evidence**: `domain::service::tests::warm_jwks_caches_nonempty_cap_document`,
  `domain::service::tests::warm_jwks_fails_closed_on_empty_key_set`,
  `domain::service::tests::warm_jwks_fails_closed_when_signer_errors`

### 5.5 Signing Plugin Selection

#### Select the signing plugin by GTS type and vendor

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-plugin-selection`

The system **MUST** resolve the active signing plugin through the types-registry by the
`SigningPluginSpecV1` type chain and the configured vendor, resolve the scoped client lazily on first
use, and delegate every signing and public-key call to it.

- **Rationale**: Binding to a signing backend by GTS type and vendor lets a deployment swap backends
  without recompiling the issuer, and lazy resolution avoids an initialization-order dependency.
- **Actors**: `cpt-cf-token-issuer-actor-signing-plugin`, `cpt-cf-token-issuer-actor-types-registry`
- **Acceptance Evidence**: `infra::plugin_select::tests::sign_delegates_to_resolved_plugin_client`,
  `infra::plugin_select::tests::public_keys_delegates_to_resolved_plugin_client`

#### Fail closed on unresolvable signing plugin

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-plugin-fail-closed`

The system **MUST** fail the operation when no plugin instance matches the configured vendor, when the
scoped client is not registered, or when the types-registry is unavailable.

- **Rationale**: There is no safe fallback for signing. Absent a resolvable backend the correct
  outcome is a retryable failure, never a token.
- **Actors**: `cpt-cf-token-issuer-actor-signing-plugin`
- **Acceptance Evidence**: `infra::plugin_select::tests::sign_errors_when_no_vendor_matches`,
  `infra::plugin_select::tests::sign_errors_when_scoped_client_not_registered`,
  `infra::plugin_select::tests::sign_errors_when_types_registry_absent`

#### Validate signing key references

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-fr-key-ref-validation`

The system **MUST** constrain signing key references to 1–64 characters of `[a-z0-9-]`, enforced on
construction and on deserialization.

- **Rationale**: A validated newtype keeps a malformed or injected key name from ever reaching a
  signing backend.
- **Actors**: `cpt-cf-token-issuer-actor-signing-plugin`
- **Acceptance Evidence**: `models::tests::signing_key_ref_valid_roundtrip`,
  `models::tests::signing_key_ref_rejects_empty`,
  `models::tests::signing_key_ref_rejects_space`,
  `models::tests::signing_key_ref_rejects_uppercase`,
  `models::tests::signing_key_ref_boundary_length`,
  `models::tests::signing_key_ref_deserialize_validates`

### 5.6 Configuration and Error Reporting

#### Validate configuration at startup

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-config-validation`

The system **MUST** fail initialization unless `issuer_base_url` is non-blank,
`clock_skew_secs <= cap_reuse_floor_secs < cap_ttl_secs`, `0 < obo_ttl_secs <= 60`,
`clock_skew_secs < obo_ttl_secs`, and `grant_ttl_secs > 0`. Unknown configuration keys **MUST** be
rejected.

- **Rationale**: These orderings are what make reuse and skew handling coherent; a violated invariant
  yields tokens that are reused past usability or rejected on arrival. Rejecting unknown keys catches
  typos that would otherwise silently fall back to a default.
- **Actors**: `cpt-cf-token-issuer-actor-operator`
- **Acceptance Evidence**: `config::tests::validate_enforces_reuse_floor_invariant`,
  `config::tests::validate_bounds_obo_ttl`,
  `config::tests::validate_requires_positive_grant_ttl`,
  `config::tests::default_config_values`

#### Derive issuer identifiers from the base URL

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-fr-issuer-derivation`

The system **MUST** derive each class's issuer identifier as `{issuer_base_url}/issuers/{class}`,
tolerating a trailing slash on the configured base URL.

- **Rationale**: The issuer identifier and the discovery URL must agree, or verifiers resolve keys
  from a location that does not serve them.
- **Actors**: `cpt-cf-token-issuer-actor-verifier`
- **Acceptance Evidence**: `config::tests::issuer_helpers_trim_trailing_slash`

#### Map failures to stable HTTP semantics

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-fr-error-mapping`

The system **MUST** map failures to canonical errors as: invalid request → 400; signing failure or
not-ready → 503; capability-token provenance or expiry failure → 401; any peer, adapter, or loop-guard
authorization failure → an indistinguishable 403; OBO disabled → 404; internal failure → 500.

- **Rationale**: Collapsing every authorization failure to one opaque 403 denies an attacker a probing
  oracle that would otherwise reveal which adapters exist, which are active, and which hold OBO
  grants. 503 tells a caller to retry; 401 tells it to re-authenticate.
- **Actors**: `cpt-cf-token-issuer-actor-adapter`
- **Acceptance Evidence**: `infra::sdk_error_mapping::tests::maps_each_variant_to_expected_status`,
  `infra::sdk_error_mapping::tests::maps_obo_remint_variants_to_expected_status`,
  `api::rest::routes::tests::remint_without_bearer_is_401`,
  `api::rest::routes::tests::remint_with_unverifiable_bearer_is_401_not_404_or_500`

#### Emit issuance metrics

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-fr-metrics`

The system **MUST** emit `token_issuer_cache_hits_total`, `token_issuer_cache_misses_total`,
`token_issuer_sign_total`, `token_issuer_sign_errors_total`, and
`token_issuer_mint_duration_seconds`. Signing volume **MUST** carry the token class under the label
`key`, and mint duration under the label `class`.

- **Rationale**: Cache-hit ratio and signing-error rate are the operational signals that reveal a
  degrading signing backend or a cache-key regression before callers notice.
- **Actors**: `cpt-cf-token-issuer-actor-operator`

## 6. Non-Functional Requirements

> **Global baselines**: Repository-wide NFRs are defined in
> [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational
> [guidelines/](../../../../guidelines/). This gear has no parent gear PRD. Only gear-specific NFRs
> are recorded below.

### 6.1 Gear-Specific NFRs

#### No private key material in process

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-nfr-no-key-custody`

The gear **MUST NOT** hold, cache, or log private signing key material. Every signature **MUST** be
produced by the signing plugin, and the gear **MUST** read only public key versions.

- **Threshold**: Zero private key bytes resident in the issuer process at any time.
- **Rationale**: Compromising the issuer must not yield an offline forgery capability. Signing stays
  behind the plugin boundary so key custody remains with the backend the deployment chose.
- **Architecture Allocation**: See DESIGN.md § 1.2 NFR Allocation.

#### Fail closed on every ambiguity

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-nfr-fail-closed`

Every unresolved condition on an issuance path — unwarmed JWKS, unresolvable signing plugin, absent
peer certificate, unavailable adapter registry, unstable key version — **MUST** result in refusal, not
in a token.

- **Threshold**: No code path returns a token when any gate input is unavailable or indeterminate.
- **Rationale**: A permissive default in a credential issuer converts a transient dependency outage
  into an authority escalation.
- **Architecture Allocation**: See DESIGN.md § 1.2 NFR Allocation.

#### Cross-class forgery resistance

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-nfr-class-isolation`

A token of one class **MUST NOT** be verifiable as another, enforced by disjoint signing keys and
issuer identifiers rather than by claim inspection alone.

- **Threshold**: Each class's JWKS contains only that class's key versions; no `kid` is shared.
- **Rationale**: A `typ`-only defence fails against a verifier that neglects the check. Key
  disjointness makes the confusion cryptographically unavailable.
- **Architecture Allocation**: See DESIGN.md § 1.2 NFR Allocation.

#### Bounded signing amplification

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-nfr-bounded-signing`

A capability mint **MUST** cost at most two signing calls on a cache miss and zero on a hit; the
key-version stabilization retry **MUST** be bounded and terminate in a retryable failure rather than
loop.

- **Threshold**: ≤ 2 signing calls per cache miss under a stable key version; bounded retry count when
  rotation races the mint.
- **Rationale**: The double sign is inherent to putting the true key version inside the signed header.
  Leaving the retry unbounded would turn a rotation storm into an unbounded load amplifier against the
  signing backend.
- **Architecture Allocation**: See DESIGN.md § 1.2 NFR Allocation.

#### Non-enumerable authorization failures

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-nfr-opaque-denials`

Peer, adapter-eligibility, down-scope, and loop-guard refusals **MUST** be externally
indistinguishable from one another.

- **Threshold**: All such refusals share one status and one reason string; no variant-specific detail
  is surfaced.
- **Rationale**: Distinguishable denials let a caller enumerate registered adapters, their active
  state, and their OBO grants.
- **Architecture Allocation**: See DESIGN.md § 1.2 NFR Allocation.

#### Idempotent retry under skew

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-nfr-idempotent-retry`

A repeated re-mint with unchanged inputs **MUST** yield the byte-identical token for the full window
during which the presented capability token is still accepted, including the clock-skew tail.

- **Threshold**: Byte equality across retries until capability `exp + clock_skew_secs`.
- **Rationale**: An adapter retrying a callback must not accumulate distinct live credentials. Scoping
  the guarantee to bare `exp` would break it precisely in the skew window, where retries concentrate.
- **Architecture Allocation**: See DESIGN.md § 1.2 NFR Allocation.

### 6.2 NFR Exclusions

- **Durability / persistence**: Not applicable. All state is in-memory and reconstructible — caches
  are optimizations and the JWKS is rebuilt from the signing backend. Nothing needs to survive a
  restart.
- **Database performance baselines**: Not applicable; the gear declares no `db` capability and owns no
  tables.
- **Multi-tenant data isolation at rest**: Not applicable; no data is stored. Tenant context travels
  in claims and in the `SecurityContext`.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### `TokenIssuerClientV1`

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-interface-client`

- **Type**: Rust trait, registered in `ClientHub`
- **Stability**: stable
- **Description**: Consumer-facing minting API — `mint_capability` returns a compact `cap+jwt`;
  `mint_grant` returns a `GrantToken` carrying the compact `grant+jwt` and its absolute expiry.
- **Breaking Change Policy**: Major version bump; the `V1` suffix pins this shape, and a
  successor trait would be introduced alongside it.

#### `SigningClientV1`

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-interface-signing-port`

- **Type**: Rust trait (port implemented by plugins)
- **Stability**: stable
- **Description**: The signing port — `sign` returns a signature plus the key version that produced
  it; `public_keys` enumerates current public key versions for JWKS construction. Keys are
  platform-scoped; tenant context travels via `SecurityContext`.
- **Breaking Change Policy**: Major version bump; every registered signing plugin implements this
  trait, so a change breaks all backends simultaneously.

#### Token models and claim sets

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-interface-models`

- **Type**: Rust structs and enums
- **Stability**: stable
- **Description**: `MintCapabilityRequest`, `MintGrantRequest`, `CapabilityClaims`, `GrantClaims`,
  `GrantToken`, `SigningKeyRef`, `SigAlg`, `SignatureResult`, `PublicKeyVersion`, plus
  `TokenIssuerError` and `SigningError`.
- **Breaking Change Policy**: Major version bump for any change to a serialized claim set, since
  minted tokens are verified by parties outside this repository.

#### `SigningPluginSpecV1` GTS type

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-interface-plugin-gts`

- **Type**: GTS type declaration
- **Stability**: stable
- **Description**: `cf.toolkit.plugins.plugin.v1~cf.core.token_issuer.signing_plugin.v1~` — the type
  chain a signing plugin registers an instance under so the issuer can discover it by vendor.
- **Breaking Change Policy**: New type version; the identifier is a wire contract with the registry.

### 7.2 External Integration Contracts

#### Published JWKS and discovery documents

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-contract-jwks`

- **Direction**: provided by library
- **Protocol/Format**: HTTP `GET`, unauthenticated, JSON — a JWKS of EC P-256 keys and an OIDC-style
  discovery document, per token class
- **Compatibility**: Additive only. Key versions come and go as the backend rotates; the document
  shape and the paths are stable, and the paths are deliberately unversioned identifier surfaces.

#### Offline grant-token verification

- [ ] `p1` - **ID**: `cpt-cf-token-issuer-contract-grant-verification`

- **Direction**: required from client (adapters)
- **Protocol/Format**: Compact ES256 JWT, `typ = grant+jwt`
- **Compatibility**: The adapter enforces `aud`, `resource_id`, `resource_name`, `resource_type`, and
  the granted operation set itself. `project_id` is attribution only and **MUST NOT** be treated as an
  authorization input. Claim additions are backward compatible; the adapter must ignore unknown
  claims.

#### OBO re-mint over mTLS

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-contract-obo-remint`

- **Direction**: provided by library
- **Protocol/Format**: HTTP `POST` with `Authorization: Bearer {cap+jwt}` over mTLS; JSON body
  optionally requesting a scope subset
- **Compatibility**: Available only when `obo.enabled`. Requires the deployment's mTLS layer to supply
  the verified client-certificate subject; absent it, every request is refused.

## 8. Use Cases

#### Mint and verify a capability token offline

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-usecase-mint-verify`

**Actor**: `cpt-cf-token-issuer-actor-consumer-gear`, `cpt-cf-token-issuer-actor-verifier`

**Preconditions**:
- The gear is ready; the capability JWKS is warm.

**Main Flow**:
1. The consumer gear calls `mint_capability` with a verified `SecurityContext` and a mint request.
2. The gear assembles the claims and signs them through the signing plugin, producing a `kid` naming
   the signing key version.
3. The consumer presents the token to a verifier.
4. The verifier fetches the capability JWKS, resolves the `kid`, and validates the signature.

**Postconditions**:
- The verifier accepted the token without contacting the issuer.

**Alternative Flows**:
- **Signature tampered**: Verification fails at the verifier; the issuer is not involved.

#### Reuse a cached capability token

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-usecase-cache-reuse`

**Actor**: `cpt-cf-token-issuer-actor-consumer-gear`

**Preconditions**:
- A token for this exact caller context was minted recently and its remaining lifetime exceeds the
  reuse floor.

**Main Flow**:
1. The consumer gear calls `mint_capability` with the same context.
2. The gear computes the cache key from the canonicalized claims and finds a live entry.
3. The cached token is returned; no signing call is made.

**Postconditions**:
- A cache-hit metric is recorded; signing volume is unchanged.

**Alternative Flows**:
- **Remaining lifetime at or below the floor**: A fresh token is minted and replaces the entry.
- **Differing operation or resource type**: A distinct cache entry is created and minted.

#### Survive a signing-key rotation mid-mint

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-usecase-rotation`

**Actor**: `cpt-cf-token-issuer-actor-signing-plugin`

**Preconditions**:
- The signing backend rotates the key between the version-learning signature and the final one.

**Main Flow**:
1. The gear signs provisionally and learns the key version.
2. It assembles the final header with `kid = {key}-v{version}` and signs again.
3. The version reported by the final signature does not match the header, so the gear retries.
4. Once the version is stable, the token is returned; the JWKS is rebuilt so the new `kid` is
   publishable.

**Postconditions**:
- The returned token's `kid` names the key version that actually signed it, and that key is present
  in the published JWKS.

**Alternative Flows**:
- **Version never stabilizes**: The mint fails with a retryable error rather than returning an
  unverifiable token.
- **`kid` still unpublishable after rebuild**: The capability mint is refused before the token is
  cached.

#### Re-mint a down-scoped OBO token for an adapter callback

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-usecase-obo-remint`

**Actor**: `cpt-cf-token-issuer-actor-adapter`

**Preconditions**:
- `obo.enabled` is set; mTLS is terminated and the peer certificate subject is available; the adapter
  is registered, active, and OBO-granted.

**Main Flow**:
1. The adapter POSTs to the re-mint endpoint presenting its capability token, optionally requesting a
   scope subset.
2. The gear confirms the presented token is not itself an OBO token.
3. It verifies the token against the capability JWKS.
4. It resolves the peer's adapter GTS ID from the certificate and confirms it equals the token's
   audience.
5. It confirms the adapter is active and OBO-granted.
6. It intersects the adapter allowlist with the token's scopes, narrowing to the requested subset.
7. It mints and returns an OBO token carrying the narrowed scope set and its own lifetime.

**Postconditions**:
- The adapter holds a credential strictly narrower than the one it presented.

**Alternative Flows**:
- **Retry with identical inputs**: The byte-identical token is returned, including within the
  clock-skew tail past the capability token's expiry.
- **Audience does not match the peer**: Refused with an opaque 403.
- **Presented token is an OBO token**: Refused with an opaque 403.
- **Intersection empty, or request not a subset**: Refused with an opaque 403.
- **`obo.enabled` unset**: Refused with 404; the routes are not even registered.

#### Boot before the signing backend is available

- [ ] `p2` - **ID**: `cpt-cf-token-issuer-usecase-delayed-backend`

**Actor**: `cpt-cf-token-issuer-actor-operator`

**Preconditions**:
- The gear starts before the signing plugin has registered.

**Main Flow**:
1. The gear attempts to warm the JWKS and fails.
2. It stays not-ready, serving 503, and retries with capped exponential backoff.
3. The signing plugin registers; a subsequent warm succeeds.
4. The gear signals readiness.

**Postconditions**:
- No empty or invalid JWKS was ever published.

**Alternative Flows**:
- **Shutdown during retry**: The gear abandons the retry promptly and exits cleanly without ever
  signalling readiness.

## 9. Acceptance Criteria

- [ ] A capability token minted by the gear verifies against the served capability JWKS, and the same
  token with a tampered signature does not.
- [ ] No token of one class verifies against another class's JWKS.
- [ ] A repeated mint for an identical caller context performs no signing call while the cached token
  remains above the reuse floor.
- [ ] Every OBO refusal path — disabled, loop guard, provenance, peer mismatch, unknown peer, inactive
  adapter, ungranted adapter, empty intersection, over-broad request — is externally
  indistinguishable from the others except for the documented 401 and 404 cases.
- [ ] A repeated OBO re-mint with unchanged inputs returns the byte-identical token, including within
  the clock-skew window past the presented token's expiry.
- [ ] With no signing backend available, the gear serves 503 and never publishes a JWKS.
- [ ] Every configuration invariant violation prevents startup, and an unknown configuration key is
  rejected rather than ignored.
- [ ] No private key material appears in the issuer process or in its logs.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `types_registry` gear | Resolves the signing-plugin instance by GTS type and vendor. Declared as a gear dependency. | p1 |
| Signing plugin (`SigningClientV1`) | Performs every signature and enumerates public key versions. Without it the gear cannot become ready. | p1 |
| ToolKit runtime | Gear lifecycle, `ClientHub`, REST registration via `OperationBuilder`, `SecurityContext` propagation. | p1 |
| `toolkit-canonical-errors` | Canonical error model behind the HTTP status mapping. | p1 |
| External mTLS terminator | Supplies the verified client-certificate subject for OBO peer binding. | p2 |
| RMS adapter registry | Adapter status, OBO grant, and scope allowlist for the OBO path. Seam present, fails closed until exposed. | p2 |

## 11. Assumptions

- The caller's identity in the `SecurityContext` has already been authenticated upstream; the gear
  does not re-authenticate it.
- Verifiers fetch and cache the published JWKS themselves and tolerate key sets changing as the
  backend rotates.
- Adapters enforce the resource and operation claims in a grant token; the gear cannot compel this.
- Clock disagreement between issuer and verifiers stays within the configured `clock_skew_secs`.
- The signing plugin returns the key version that actually produced each signature, truthfully.
- `issuer_base_url` is externally reachable at the value configured, and stable for the lifetime of
  issued tokens.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Signing backend unavailable or slow | No tokens can be minted; dependent flows stall | Readiness gate keeps the gear out of rotation; signing failures map to 503 so callers retry; `token_issuer_sign_errors_total` surfaces the condition |
| Key rotation races a mint | A token could carry a `kid` naming the wrong version and be unverifiable | Two-phase signing with bounded retry until the version stabilizes; JWKS rebuilt on an unseen `kid`; capability mint refused if the `kid` remains unpublishable |
| `issuer_base_url` misconfigured | Verifiers resolve keys from a location that serves nothing; every token is rejected | Non-blank validation at startup; issuer identifiers and discovery URLs derived from the single configured value so they cannot disagree |
| OBO surface enabled without mTLS wired | Every re-mint is refused, appearing as an outage | Peer resolution fails closed and the surface is off by default; the coupling is documented in the configuration reference |
| Reuse floor set too close to the TTL | Tokens handed out shortly before expiry, causing downstream rejections | Startup invariant `clock_skew_secs <= cap_reuse_floor_secs < cap_ttl_secs` |
| Cache key regression collapsing distinct contexts | A caller receives a token asserting authority it did not request | Cache key derived from the canonicalized claim set, pinned by dedicated tests |
| Adapter treats `project_id` as an authorization input | Authorization decided on an attribution-only field | Documented as attribution-only in the contract and in the claim documentation |
| In-memory caches lost on restart | A burst of signing calls after every restart | Caches are pure optimizations; correctness is unaffected, and the signing amplification stays bounded per miss |

## 13. Open Questions

- Should the reuse cache and the OBO idempotency cache be bounded in entry count? Both currently grow
  with the number of distinct live contexts, and neither has an eviction policy beyond expiry.
- When the RMS adapter registry is exposed, should adapter facts be cached, and with what invalidation
  — a stale allowlist would widen or narrow OBO grants incorrectly.
- Should grant tokens support a reuse cache for identical resource-and-operation requests, or is
  per-mint uniqueness a requirement adapters may come to depend on?
- Is `obo_ttl_secs`' 60-second ceiling a deliberate policy limit or an artifact of the current
  deployment, and should it be configurable?
- Should the gear expose a way for operators to force a JWKS rebuild without a restart, for a rotation
  that the mint path has not yet observed?
- `transit_mount` is accepted and validated as configuration but is never read by this gear — the
  signing plugin owns the mount path. Should it be removed from this gear's config, or wired through to
  the plugin so the value has an effect?
- The OBO re-mint path refreshes its JWKS on an unseen `kid` but, unlike the capability and grant
  paths, does not refuse an unpublishable one. Should it be made symmetric before the surface is
  enabled in any deployment?
- `validate_grant_request` applies the field contract to `audience`, `resource_name`, `resource_type`,
  and each operation id, but only the empty-operations branch has a test. The length and charset
  branches on the grant path are unverified — should they be covered before the grant class is
  relied on more widely?

## 14. Traceability

Links to related specification artifacts.

- **Design**: [DESIGN.md](./DESIGN.md)
