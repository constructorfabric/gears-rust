---
description: "Product requirements for the api-key-management gear: self-service API keys bound to the calling user, with expiration, revocation, administrative withdrawal, and verification for the AuthN resolver."
---

# PRD — API Key Management

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
  - [5.1 API Key Lifecycle](#51-api-key-lifecycle)
  - [5.2 Verification](#52-verification)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Issue and Use an API Key](#issue-and-use-an-api-key)
  - [Revoke a Leaked Key](#revoke-a-leaked-key)
  - [Offboard a User](#offboard-a-user)
  - [Owner Deleted](#owner-deleted)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

API Key Management is a platform gear that lets an authenticated user issue long-lived API keys for themselves and later use such a key as a bearer credential instead of an IdP-issued token. The gear issues keys, lists and describes the caller's keys, revokes them, lets tenant administrators withdraw keys of users they administer, and verifies a presented key on behalf of the AuthN resolver so the platform can build a `SecurityContext` for the key's owner.

Key features of the first version: issuance for the current user with a mandatory expiration, one-time disclosure of the API key secret, a recognizable configurable API key prefix, listing with a human-readable name and a partial API key hint, irreversible revocation with a recorded reason, administrative revocation, a per-owner key limit, and verification that also confirms the owning user still exists in an active tenant.

### 1.2 Background / Problem Statement

Today the only bearer credentials the platform accepts are JWTs from the configured OIDC IdP. Scripts, CI jobs, and integrations need a credential that does not require an interactive login or a client-credentials registration in the IdP for every use case, and that the user can create and withdraw without operator involvement.

An API key differs from an OIDC access token in one important way: an access token lives minutes and the IdP stops issuing tokens to a deleted user, so access is withdrawn implicitly. An API key lives for months. The gear therefore has to provide the guarantees the IdP gives implicitly: bounded lifetime, explicit revocation, and withdrawal of access when the owning user disappears.

API key management is an established capability in the industry, offered both as dedicated services (for example, Ory Talos and Unkey) and as built-in features of developer platforms (for example, GitHub personal access tokens and Stripe API keys). Such systems commonly add imported keys, derived short-lived tokens, per-key scopes, rate limits, and IP restrictions; this gear deliberately covers only the minimal subset the platform needs now: keys issued for the calling user.

### 1.3 Goals (Business Outcomes)

- A user can obtain a working API key for automation in one self-service call, without operator involvement.
- A revoked or expired key, or a key whose owning user was deleted, stops authenticating within a bounded, configured time.
- A tenant administrator can withdraw all API keys of a user without that user's participation.
- A leaked database snapshot does not reveal usable API keys.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| API key | A credential record issued by this gear to a user: identity, owner, name, expiration, status, and the hash of its API key secret. |
| API Key Secret | The full credential string presented by the client, including the API key prefix, API key format version, and random part. Disclosed only once, in the issuance response. |
| API Key Prefix | A configurable leading label of every API key secret that makes keys recognizable and lets the AuthN resolver plugin route them to this gear. |
| API Key Format Version | A marker inside every API key secret that identifies its layout, so later releases can change the layout without invalidating issued keys. |
| API Key Hint | A non-secret display form of an API key secret (its API key prefix plus its last characters) used to tell keys apart in listings. |
| Owner | The user (subject ID, subject type, and home tenant) that issued the key and on whose behalf the key authenticates. |
| Verification | Checking a presented API key secret and returning the owner's identity when the key is valid. |
| API Key Lifecycle | A key is issued as `active`, becomes `revoked` through irreversible revocation or `expired` when its lifetime ends, and is removed after retention. Revocation or expiration, whichever occurs first, starts retention; a revoked key remains `revoked` after its expiration time. |

## 2. Actors

### 2.1 Human Actors

#### Key Owner

**ID**: `cpt-cf-api-key-management-actor-key-owner`

- **Role**: An authenticated user who issues, lists, inspects, and revokes API keys for themselves, and uses a key from scripts or integrations.
- **Needs**: Create a key with a name and expiration, see which keys exist and when they expire, revoke a key that is no longer needed or was leaked.

#### Tenant Administrator

**ID**: `cpt-cf-api-key-management-actor-tenant-admin`

- **Role**: A user with administrative rights over a tenant who has to withdraw keys of other users in that tenant, for example on offboarding, user disablement, or a suspected leak.
- **Needs**: List and revoke any key, or all keys of a given user, within the tenants they administer, including keys of users who no longer exist.

#### Platform Operator

**ID**: `cpt-cf-api-key-management-actor-operator`

- **Role**: The person who deploys and configures the gear and the AuthN resolver plugin: the API key prefix in both places, lifetime policy, key limit, and the gear's service authorization in Account Management; investigates verification failures and dependency outages.
- **Needs**: Configuration errors detected at startup, and operational signals that explain why verifications fail.

### 2.2 System Actors

#### AuthN Resolver Plugin

**ID**: `cpt-cf-api-key-management-actor-authn-plugin`

- **Role**: The AuthN resolver plugin that receives a bearer credential, recognizes an API key by its API key prefix, asks this gear to verify it, and turns the result into a `SecurityContext`. Credentials without the API key prefix and client-credentials exchange are handled by the platform's existing authentication path.

#### Account Management

**ID**: `cpt-cf-api-key-management-actor-account-management`

- **Role**: The source of truth for users and tenants. This gear asks it whether a key owner still exists in the owner's home tenant and whether that tenant is active.

#### AuthZ Resolver

**ID**: `cpt-cf-api-key-management-actor-authz-resolver`

- **Role**: The policy decision point that authorizes self-service and administrative key operations, and authorizes requests made with an API key exactly as it would for the owner.

## 3. Operational Concept & Environment

Runtime, deployment, and integration patterns follow the repository baselines in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/README.md](../../../../guidelines/README.md). Authentication and authorization follow the platform AuthN/AuthZ architecture in [docs/arch/authorization/DESIGN.md](../../../../docs/arch/authorization/DESIGN.md).

### 3.1 Gear-Specific Environment Constraints

- The AuthN resolver selects a single plugin per deployment, so API key authentication is delivered by a plugin that handles API keys itself and preserves the platform's existing OIDC bearer authentication and client-credentials exchange for everything else.
- Verification runs on the request authentication path, before any user `SecurityContext` exists; the gear calls Account Management under its own service identity, which requires a deployment-time grant to read users and tenants in every tenant whose users own keys.
- Supported deployments apply platform admission controls to unauthenticated verification and to issuance traffic, and bound the volume of failure diagnostics; per-key rate limits are out of scope for P1.

## 4. Scope

### 4.1 In Scope

- Issuing an API key for the calling user, with a name and an expiration.
- One-time disclosure of the API key secret; storage of a non-reversible hash only.
- Listing and inspecting the caller's own keys, with name, API key hint, timestamps, status, and revocation reason.
- Revoking a key by its owner; listing and revoking keys of users by a tenant administrator.
- A per-owner limit on active keys.
- Verifying a presented key for the AuthN resolver plugin and returning the owner's identity.
- Withdrawing access when the owning user no longer exists in Account Management or the owner's home tenant is not active.
- A configurable API key prefix, shared by the gear and the AuthN resolver plugin.
- Structured security events for key issuance and revocation.
- Time-bounded retention of revoked and expired key records.

### 4.2 Out of Scope

Implementation of the AuthN resolver plugin, construction and propagation of `SecurityContext`, and downstream credential forwarding or exchange belong to the AuthN resolver. This PRD defines the integration contracts needed by API Key Management, but does not specify those components as deliverables of this gear.

**Deferred to P2** (additional P2 capabilities named here without detailed requirements; other P2 requirements are marked `p2` in sections 5 and 6):

- Detecting users that are disabled but not deleted in the IdP: Account Management does not expose this state yet. In P1 a tenant administrator revokes such a user's keys (`cpt-cf-api-key-management-fr-admin-revoke`).
- Per-key scopes or permissions: in P1 every key authenticates with unrestricted token scopes (`["*"]`); authorization is still decided by the PDP.
- Key rotation as a dedicated operation that copies the old key's attributes to the new key: in P1 a client issues a new key, deploys it, and then revokes the old one with the reason `superseded`; this sequence requires a free active-key slot. At the per-owner limit, the owner must first revoke an existing key to free a slot; uninterrupted replacement at the limit is not guaranteed in P1.
- Idempotent issuance with a client-supplied request key (Google AIP-155): in P1 a retried issuance after a lost response creates a second key; the owner finds the unused one by name and creation time and revokes it.
- An integrity checksum inside the API key secret for offline rejection of mistyped or fabricated keys, introduced as a new API key format version (`cpt-cf-api-key-management-fr-key-format-version`).
- Per-key rate limits.
- Per-key IP allowlists.

**Not planned**:

- Importing externally issued keys.
- Derived short-lived tokens (JWT, macaroon) issued by this gear. Exchanging an API key for an IdP access token, if ever supported, belongs to the AuthN resolver (`cpt-cf-api-key-management-contract-authn-context`).
- Public (client-safe) keys.
- Self-revocation by presenting the API key secret without authentication (secret-scanning integrations).
- Keys that never expire.
- Migration of keys between API key prefixes.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code coverage unless otherwise specified.

### 5.1 API Key Lifecycle

#### Issue API Key for the Caller

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-issue-key`

The system **MUST** issue a new API key owned by the calling user, recording the caller's subject ID, subject type, and home tenant as the key's owner, together with a caller-supplied name and the key's expiration time. The response **MUST** contain the API key secret and the key's metadata. Every key **MUST** receive a freshly generated key ID and API key secret. The system **MUST NOT** intentionally reuse the key ID or API key secret of a removed key and **MUST** reject collisions with retained keys.

- **Rationale**: Core self-service capability; the owner identity is exactly what a `SecurityContext` needs later.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`

#### Issuer Eligibility

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-issuer-eligibility`

The system **MUST** reject issuance for anonymous callers, for callers whose subject type is absent or is not a user subject type, and for callers that Account Management does not report as an existing user in their home tenant.

- **Rationale**: Verification checks the owner through the Account Management user lookup; a key owned by any other kind of subject could never pass that check or would be interpreted incorrectly.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-account-management`

#### Access Token Required for Key Management

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-management-auth`

The system **MUST** accept caller-initiated key management operations only from users authenticated with an IdP-issued access token whose authentication is positively established as not originating from an API key. This applies to issuance, listing, single-key retrieval, owner revocation, administrative listing and revocation, and bulk revocation. An API-key-origin context means authentication using an API key or any downstream credential derived from it, including an exchanged access token. Management operations from such a context, or from a context whose authentication origin cannot be established, **MUST** be rejected, regardless of the caller's ownership or administrative permissions and whether the target is the authenticating key or another key.

This restriction **MUST** hold for REST and SDK operations, including direct and remote invocation. An eligible access token does not bypass authorization or the additional issuance requirements. Verification by authorized platform authentication services and system-initiated revocation and retention are outside caller-initiated key management and retain their own requirements.

- **Rationale**: A leaked API key must not expose key inventory or allow an attacker to create or revoke keys, including the leaked key itself.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-tenant-admin`, `cpt-cf-api-key-management-actor-authn-plugin`

#### No Key Issuance from Restricted or Key-Based Sessions

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-no-escalation`

In addition to `cpt-cf-api-key-management-fr-management-auth`, the system **MUST** accept issuance only from a caller whose token scopes are unrestricted (`["*"]`). Issuance with restricted token scopes **MUST** be rejected for direct and remote invocation.

- **Rationale**: Every key carries unrestricted token scopes. Without this rule a restricted token could be turned into an unrestricted key, and a leaked key could mint replacement keys that survive its own revocation.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-authn-plugin`

#### Mandatory Bounded Expiration

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-expiration`

The system **MUST** give every key an expiration time, measured from issuance. When the caller does not request a lifetime, the configured default lifetime applies. The system **MUST** reject a requested lifetime that is not positive or is longer than the configured maximum, without creating a key. The configuration **MUST** satisfy `0 < default lifetime ≤ maximum lifetime`; otherwise the gear **MUST** fail to start. Changing the lifetime policy **MUST NOT** change the expiration of issued keys. Expiration is an absolute cutoff: from its expiration time on, a key **MUST NOT** authenticate, and a reused verification result **MUST NOT** extend it.

- **Rationale**: Bounds the damage of a forgotten or leaked key.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-operator`

#### One-Time API Key Secret Disclosure

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-secret-once`

The system **MUST** return the API key secret only in the issuance response. Every later operation **MUST NOT** return the API key secret or any value from which it can be recovered. The system **MUST** store only a non-reversible hash of the API key secret.

- **Rationale**: A database or API leak must not disclose usable keys.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`

#### Recognizable API Key Prefix

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-key-prefix`

Every issued key **MUST** start with the configured API key prefix. The system **MUST** treat a presented credential without the configured API key prefix as invalid. The gear **MUST** fail to start with an empty API key prefix or with an API key prefix that can be the beginning of another supported credential format, such as a JWT.

Changing the configured API key prefix is a breaking operational change: once the new prefix is effective in the gear and the AuthN resolver plugin, keys issued under a previous prefix **MUST** fail authentication, including unexpired and unrevoked ones. A prefix change does not revoke keys and does not change their status; if a previous prefix is configured again, its unexpired and unrevoked keys become eligible for verification again. Permanent withdrawal requires revocation.

- **Rationale**: Lets the AuthN resolver plugin route credentials, and lets people and secret scanners recognize leaked keys. Supporting several prefixes at once is not worth its complexity in P1.
- **Actors**: `cpt-cf-api-key-management-actor-authn-plugin`, `cpt-cf-api-key-management-actor-operator`

#### Versioned API Key Format

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-key-format-version`

Every issued API key secret **MUST** carry a marker of its API key format version. After a new API key format version is introduced, the system **MUST** keep verifying unexpired, unrevoked keys of every earlier released format version that carry the configured API key prefix.

- **Rationale**: Lets later releases change the API key secret format, for example by adding an integrity checksum, without invalidating keys already in use.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-authn-plugin`

#### List Own Keys

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-list-keys`

The system **MUST** list the caller's retained keys with pagination, returning for each key its ID, name, API key hint, status, creation time, expiration time, and, for revoked keys, the revocation time and revocation reason. The listing **MUST** contain only keys owned by the caller. The status **MUST** be `revoked` for a revoked key, also after its expiration time; otherwise `expired` from the expiration time on; otherwise `active`.

- **Rationale**: The owner needs to find a key to revoke and to see which keys are about to expire.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`

#### Get Own Key

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-get-key`

The system **MUST** return the metadata of a single key by its ID, with the same fields as the listing, when the caller owns the key, and **MUST** respond with the same not-found outcome when the key does not exist, has been purged, or is owned by someone else.

- **Rationale**: Detail view and existence check without disclosing other users' keys.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`

#### Revoke Own Key

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-revoke-key`

The system **MUST** let the owner revoke a key by its ID with a revocation reason the owner is allowed to set (`cpt-cf-api-key-management-fr-revocation-reason`). After authentication and authorization succeed, the reason **MUST** be validated before deciding the revocation outcome. Revocation **MUST** be irreversible and idempotent:

- revoking an already revoked key succeeds and keeps the original revocation time and reason;
- revoking an expired key that was never revoked succeeds without change and records no reason;
- revoking a key that does not exist, has been purged, or is owned by someone else returns the not-found outcome of `cpt-cf-api-key-management-fr-get-key`.

A revoked key **MUST NOT** authenticate after the revocation propagation bound (`cpt-cf-api-key-management-nfr-revocation-propagation`).

- **Rationale**: Withdrawing a leaked or unneeded key is the main safety control.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`

#### Revocation Reason

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-revocation-reason`

The system **MUST** record a revocation reason for every revoked key and **MUST** reject a revocation request without a reason or with a reason the actor is not allowed to set. The reasons are a subset of the CRL reason codes defined in RFC 5280 (section 5.3.1), reduced to those meaningful for API keys:

| Reason | Set by | RFC 5280 reason code |
|--------|--------|----------------------|
| `key_compromise` | owner, tenant administrator | `keyCompromise` |
| `superseded` | owner | `superseded` |
| `no_longer_needed` | owner | `cessationOfOperation` |
| `privilege_withdrawn` | tenant administrator | `privilegeWithdrawn` |
| `owner_removed` | the system only | `affiliationChanged` |

- **Rationale**: Incident response must tell a compromised key from routine cleanup and a manual revocation from an automatic one; reasons cannot be reconstructed for keys revoked before the field exists. Reusing RFC 5280 semantics avoids inventing a new taxonomy.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-tenant-admin`, `cpt-cf-api-key-management-actor-account-management`

#### Administrative Listing and Revocation

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-admin-revoke`

The system **MUST** let a tenant administrator list and revoke retained keys whose recorded owner home tenant they are currently authorized to administer, including revoking all keys of a given user in one operation, with a revocation reason the administrator is allowed to set (`cpt-cf-api-key-management-fr-revocation-reason`). Listing and revocation **MUST** work by the recorded owner and tenant identity without requiring the owner to still exist. Revocation follows the idempotency and propagation rules of `cpt-cf-api-key-management-fr-revoke-key`.

Bulk revocation **MUST** cover all active keys of the specified owner at a single cutoff during the operation. Keys issued after that cutoff are outside its guarantee. The operation **MUST NOT** itself disable the owner or prevent subsequent issuance; offboarding must separately remove the owner's ability to issue keys.

- **Rationale**: Offboarding, user disablement, and incident response must not depend on the key owner; it is the P1 mitigation for disabled users that Account Management cannot report.
- **Actors**: `cpt-cf-api-key-management-actor-tenant-admin`

#### Per-Owner Key Limit

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-key-limit`

The system **MUST** reject issuance when the owner already has the configured maximum number of active keys, also under concurrent issuance requests. Revoked and expired keys **MUST NOT** count toward the limit. Issuing a replacement key is subject to the same limit; P1 provides no additional slot for replacement.

- **Rationale**: Bounds the number of simultaneously valid credentials of a single user.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-operator`

#### Retention of Revoked and Expired Keys

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-retention`

The system **MUST** retain the metadata and stored hash of a key for 90 days from revocation or expiration, whichever occurs first, and **MUST** remove them within 24 hours after that period ends while the gear is operational; when downtime prevents this, overdue removals **MUST** complete within 24 hours of restored operation. Repeated requests and later lifecycle timestamps **MUST NOT** restart the retention period, and retention **MUST NOT** extend credential validity. Removal **MUST NOT** depend on the owner still existing. A removed key **MUST** be omitted from listings, return not-found on lookup, and fail verification. Security events follow their own platform retention policy and are not removed with the key.

- **Rationale**: Keeps revoked and expired keys visible for incident response and offboarding for a bounded time, without growing storage forever.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-tenant-admin`

#### Last Used Time

- [ ] `p2` - **ID**: `cpt-cf-api-key-management-fr-last-used`

The system **MUST** report, for each key, the approximate time it last authenticated successfully, with a staleness of at most 5 minutes.

- **Rationale**: Lets owners and administrators find and remove unused keys.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-tenant-admin`

#### Developer and Operator Guidance

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-guidance`

The gear **MUST** provide developer and operator guidance covering one-time API key secret capture, recovery from a lost issuance response, manual replacement before expiration, revocation, the consequences of an API key prefix change, the required service authorization, and diagnosis of the "verification unavailable" outcome.

- **Rationale**: Mandatory expiration, one-time disclosure, and two-place prefix configuration have non-obvious consequences for key owners and operators.
- **Actors**: `cpt-cf-api-key-management-actor-key-owner`, `cpt-cf-api-key-management-actor-operator`

### 5.2 Verification

#### Verify Presented Key

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-verify-key`

The system **MUST** verify a presented API key secret and, when the key exists, is neither revoked nor expired, and its owner passes `cpt-cf-api-key-management-fr-owner-exists`, return the key ID, the owner's subject ID, subject type, home tenant, the token scopes `["*"]`, and the key's expiration time. The system **MUST** reject a credential that exceeds the supported maximum length or does not match a supported key format with the "invalid credential" outcome.

- **Rationale**: Gives the AuthN resolver plugin everything it needs to build a `SecurityContext` for the owner and to attribute the request to a specific key.
- **Actors**: `cpt-cf-api-key-management-actor-authn-plugin`

#### Uniform Verification Failure

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-uniform-failure`

The system **MUST** report every credential-related verification failure (malformed credential, wrong API key prefix, unknown or purged key, wrong API key secret, revoked, expired, owner no longer valid) as one indistinguishable "invalid credential" outcome. When the system cannot establish validity because of a dependency failure, an authorization or configuration failure of its service identity, an unsupported dependency capability, or an internal error, it **MUST** report a distinct "verification unavailable" outcome; this outcome **MUST NOT** grant access. Failure categories **MUST** be observable in aggregate metrics; the specific failure reason and, when the key is known, its key ID **MUST** appear only in the gear's diagnostic logs.

- **Rationale**: Callers must not learn whether a key exists or why it was rejected, while an infrastructure outage must stay distinguishable for retries and incident detection; the AuthN resolver SDK already distinguishes invalid credentials from service unavailability.
- **Actors**: `cpt-cf-api-key-management-actor-authn-plugin`, `cpt-cf-api-key-management-actor-operator`

#### Owner Must Still Exist

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-owner-exists`

The system **MUST** fail verification of a key unless Account Management reports the owning user as existing in the owner's home tenant. A positive result is reused for no longer than the owner check interval (`cpt-cf-api-key-management-nfr-owner-check-interval`). When no reusable positive result exists and Account Management reports the user as absent, denies access, or is unavailable, verification **MUST** fail. The system **MUST** distinguish authoritative absence of the user from denied access, an inaccessible tenant, and dependency failure in its logs and metrics.

- **Rationale**: Replaces the implicit withdrawal an OIDC IdP provides by refusing tokens to deleted users; the reuse interval keeps the guarantee comparable to an OIDC access token lifetime.
- **Actors**: `cpt-cf-api-key-management-actor-authn-plugin`, `cpt-cf-api-key-management-actor-account-management`

#### Owner Tenant Must Be Active

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-fr-owner-tenant-active`

The system **MUST** fail verification of a key whose owner's home tenant is not active in Account Management, with the same reuse and failure rules as `cpt-cf-api-key-management-fr-owner-exists`.

In P1, owner absence or tenant inactivity blocks authentication without automatically revoking the key. If Account Management again reports the same recorded owner identity in an active home tenant, an unexpired, unrevoked key **MUST** become eligible for verification again. This includes restoration of the tenant or reappearance of a user with the same identity. Permanent withdrawal requires explicit revocation; automatic revocation of deleted owners is a P2 requirement (`cpt-cf-api-key-management-fr-revoke-orphaned`).

- **Rationale**: A suspended or deleted tenant must not keep long-lived access through API keys. Account Management reports users only in active tenants, so this follows from the owner check; it is stricter than the current OIDC path, which does not check tenant status.
- **Actors**: `cpt-cf-api-key-management-actor-authn-plugin`, `cpt-cf-api-key-management-actor-account-management`

#### Revoke Keys of Deleted Owners

- [ ] `p2` - **ID**: `cpt-cf-api-key-management-fr-revoke-orphaned`

When Account Management authoritatively reports that the owning user no longer exists, the system **MUST** mark the key as revoked with the reason `owner_removed`. Denied access, an inaccessible tenant, or a dependency failure **MUST NOT** trigger this revocation.

- **Rationale**: Keeps listings and administrative views truthful and makes the withdrawal permanent even if a user with the same ID reappears.
- **Actors**: `cpt-cf-api-key-management-actor-account-management`

## 6. Non-Functional Requirements

Project-wide NFR baselines are defined in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/README.md](../../../../guidelines/README.md); security baselines in [docs/security/SECURITY.md](../../../../docs/security/SECURITY.md). Only gear-specific NFRs are listed below.

### 6.1 Gear-Specific NFRs

#### API Key Secret Strength

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-secret-strength`

The system **MUST** generate the random part of every API key secret from a cryptographically secure source with at least 256 bits of entropy.

- **Threshold**: ≥ 256 bits of entropy per API key secret.
- **Rationale**: Makes guessing infeasible and makes a fast non-reversible hash sufficient for storage.

#### No API Key Secret Exposure

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-no-secret-exposure`

The system **MUST NOT** write an API key secret, or the bearer credential carrying it, to logs, traces, metrics, security events, error responses, or persistent storage other than as a non-reversible hash.

- **Threshold**: Zero occurrences of an API key secret in any of the listed sinks in automated tests that issue and verify keys.
- **Rationale**: API keys are long-lived; any leak path is high impact.

#### Verification Latency

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-verify-latency`

The system **MUST** verify a key within 10 ms at p95 when a reusable owner check result exists, measured inside the gear excluding network transport. A verification that needs a fresh owner check **MUST** complete or fail with the "verification unavailable" outcome within a configured timeout.

- **Threshold**: p95 ≤ 10 ms with a reusable owner check; fresh-check timeout configurable, default ≤ 2 s.
- **Rationale**: Verification is on every API-key-authenticated request; a slow IdP must not hold requests indefinitely.

#### Revocation Propagation

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-revocation-propagation`

On every gear instance, every verification that starts at or after the moment of a successful revoke response plus the configured propagation bound **MUST** reject the revoked key.

These bounds, like the expiration cutoff and the owner check interval, apply to new authentication decisions, including decisions based on reused or derived credentials; termination of already authorized in-flight operations and streams follows the platform policy.

- **Threshold**: Propagation bound configurable, default ≤ 60 s.
- **Rationale**: Revocation is the primary response to a leaked key.

#### Owner Check Interval

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-owner-check-interval`

A positive owner check result **MUST** be no older than the configured owner check interval at the moment it is used to accept a key. Reusing the result **MUST NOT** restart its age.

- **Threshold**: Interval configurable, default ≤ 300 s.
- **Rationale**: Keeps the delay between user deletion or tenant suspension and loss of API key access comparable to an OIDC access token lifetime.

#### Lifecycle Security Events

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-security-events`

The system **MUST** emit a structured security event for each committed issuance and each committed revocation, including each key revoked by a bulk administrative revocation. The event **MUST** carry the event time, operation, key ID, owner and home tenant, actor (the system for system-initiated revocations), and revocation reason where applicable. A repeated revocation that changes no state **MUST NOT** be reported as a new revocation. Events use the platform's existing structured logging; P1 does not guarantee durable or exactly-once delivery.

- **Threshold**: One event per committed issuance and per committed revocation in automated tests.
- **Rationale**: Incident investigation must be able to answer who created or withdrew a key and when, even before the platform audit capability exists.

#### Durable Lifecycle Audit

- [ ] `p2` - **ID**: `cpt-cf-api-key-management-nfr-audit`

The system **MUST** deliver the lifecycle security events of `cpt-cf-api-key-management-nfr-security-events` to the platform audit capability with its durability, immutability, retention, and outage guarantees.

- **Threshold**: Every committed issuance and revocation is present in the platform audit store.
- **Rationale**: [docs/security/SECURITY.md](../../../../docs/security/SECURITY.md) requires credential lifecycle audit; this waits for the platform audit gear, which does not exist yet.

#### Operational Signals

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-nfr-operational-signals`

The system **MUST** expose metrics for verification outcomes by failure reason, verification latency, owner check reuse, and Account Management errors, distinguishing authorization misconfiguration of the gear's service identity from dependency unavailability. Metric labels **MUST NOT** contain key IDs, owner IDs, or API key secrets.

- **Threshold**: Each listed signal is observable in automated tests.
- **Rationale**: The operator must be able to tell a misconfigured deployment from an outage and from an attack.

### 6.2 NFR Exclusions

- Horizontal scalability beyond project defaults: key management traffic is low-volume; verification load follows the platform default sizing.
- Offline verification without the gear: not required, because every key verification is stateful by design.
- User interface, accessibility, and localization: not applicable; the gear exposes only APIs.
- Safety-critical operation: not applicable; the gear does not control physical processes.
- Specific regulatory certifications: none are claimed for this gear; data handling follows [docs/security/SECURITY.md](../../../../docs/security/SECURITY.md).
- Reconciliation after restoring a database backup, so that restored records cannot re-enable revoked keys: deferred until production installations exist.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Self-Service and Administrative REST API

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-interface-rest`

- **Type**: REST API
- **Stability**: unstable
- **Description**: Issue, list, get, and revoke API keys of the calling user; administrative listing and revocation of keys of users in administered tenants.
- **Breaking Change Policy**: Versioned path; breaking changes require a new major version while the API is unstable.

#### SDK Client

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-interface-sdk`

- **Type**: Rust gear client trait in the gear's SDK crate
- **Stability**: unstable
- **Description**: The same lifecycle operations as the REST API, plus key verification for in-process and remote platform authentication services.
- **Breaking Change Policy**: Major version bump of the SDK crate.

### 7.2 External Integration Contracts

Contracts marked "required from" state dependencies on other gears. Their implementation belongs to those gears; API Key Management provides the verification contract below.

#### Key Verification for the AuthN Resolver

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-contract-verification`

- **Direction**: provided by the gear
- **Protocol/Format**: SDK client call, available only to authorized platform authentication services and never exposed through the public REST API. It does not require an authenticated key-owner context; remote invocation authenticates and authorizes the calling service independently of the presented API key.
- **Compatibility**: Input is the full bearer credential. Output is the key ID, owner identity, token scopes, and key expiration (`cpt-cf-api-key-management-fr-verify-key`), the "invalid credential" outcome, or the "verification unavailable" outcome. Verification does not issue IdP access tokens and does not select the downstream credential. New output fields are added backward-compatibly.

#### API Key Routing in the AuthN Resolver Plugin

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-contract-authn-routing`

- **Direction**: required from the AuthN resolver plugin
- **Protocol/Format**: The plugin is configured with the same API key prefix as the gear, rejects an empty or ambiguous prefix at startup, sends every credential that starts with it to the verification contract, and passes every other credential to the platform's existing OIDC authentication unchanged. A credential routed as an API key that fails verification **MUST NOT** be retried through another authentication path. The plugin preserves the existing client-credentials exchange behavior.
- **Compatibility**: A prefix mismatch between plugin and gear configuration makes API keys fail authentication; it never makes an invalid credential succeed. Existing OIDC bearer authentication and client-credentials exchange keep their current behavior and failure semantics.

#### API-Key-Origin Security Context

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-contract-authn-context`

- **Direction**: required from the AuthN resolver
- **Protocol/Format**: The AuthN resolver builds the owner's `SecurityContext` from the verification result and owns the choice of the downstream bearer credential: forwarding the API key itself, or, if it ever supports one, exchanging it for an IdP access token. It **MUST** keep an API-key-origin context distinguishable through every supported downstream call and token exchange, so that it cannot authorize any key management operation (`cpt-cf-api-key-management-fr-management-auth`). It **MUST NOT** extend access beyond the key's expiration, the revocation propagation bound, or the owner check interval. It **MUST** support only forwarding or exchange modes that the receiving authentication and authorization services accept; an unsupported or failed exchange **MUST NOT** grant access or silently switch to another mode.
- **Compatibility**: Defined by the AuthN resolver; tracked as an open question owned by the AuthN resolver.

#### Owner Status from Account Management

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-contract-account-management`

- **Direction**: required from Account Management
- **Protocol/Format**: Account Management SDK user lookup by home tenant and user ID, which succeeds only for existing users in active tenants, callable under the gear's service identity. The deployment grants that identity read access to users and tenants in every tenant whose users own keys. The AuthN subject ID of a user equals the user ID in Account Management.
- **Compatibility**: Follows the Account Management SDK versioning.

## 8. Use Cases

All caller-initiated management steps below require an eligible IdP-issued access token under `cpt-cf-api-key-management-fr-management-auth`, including owner and administrator revocation. Using a key to authenticate an ordinary platform request and verifying it through AuthN are not management operations.

### Issue and Use an API Key

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-usecase-issue-and-use`

**Actor**: `cpt-cf-api-key-management-actor-key-owner`

**Preconditions**:
- The owner is a user authenticated with an eligible IdP-issued access token with unrestricted token scopes.

**Main Flow**:
1. The owner requests a new key with a name and a lifetime within the configured maximum.
2. The system issues the key and returns the API key secret once, together with the key's metadata.
3. The owner stores the API key secret in a CI job and calls a platform API with it as the bearer credential.
4. The AuthN resolver plugin recognizes the API key prefix and asks the gear to verify the key.
5. The gear returns the key ID and the owner's identity; the request proceeds with the owner's `SecurityContext`.

**Postconditions**:
- The key is listed among the owner's keys with status `active`.

**Alternative Flows**:
- **Lifetime above maximum**: issuance is rejected and no key is created.
- **Owner already has the maximum number of active keys**: issuance is rejected.
- **Caller authenticated with an API key**: issuance is rejected.

### Revoke a Leaked Key

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-usecase-revoke`

**Actor**: `cpt-cf-api-key-management-actor-key-owner`

**Preconditions**:
- The owner has an active key.

**Main Flow**:
1. The owner lists their keys and identifies the leaked one by name and API key hint.
2. The owner revokes it with the reason `key_compromise`.
3. Within the revocation propagation bound, requests with the key fail authentication.

**Postconditions**:
- The key is listed with status `revoked` and reason `key_compromise`.

### Offboard a User

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-usecase-offboard`

**Actor**: `cpt-cf-api-key-management-actor-tenant-admin`

**Preconditions**:
- A user of a tenant the administrator administers has active keys and is being disabled or removed.

**Main Flow**:
1. As part of offboarding outside this gear, the administrator removes the user's ability to issue new keys.
2. The administrator lists the user's keys.
3. The administrator revokes all keys of the user in one operation with the reason `privilege_withdrawn`.
4. Within the revocation propagation bound, requests with any of these keys fail authentication.

**Postconditions**:
- Every key that was active at the bulk revocation cutoff is revoked with the reason `privilege_withdrawn` and stops authenticating within the propagation bound. Previously revoked keys keep their original reason and time; expired keys remain expired. A security event exists for each key whose state changed to revoked.

### Owner Deleted

- [ ] `p1` - **ID**: `cpt-cf-api-key-management-usecase-owner-deleted`

**Actor**: `cpt-cf-api-key-management-actor-account-management`

**Preconditions**:
- A user with an active key is deleted in Account Management.

**Main Flow**:
1. A request arrives with the user's key after the owner check interval has elapsed.
2. The gear asks Account Management for the user and learns that it does not exist.
3. Verification fails with the uniform "invalid credential" outcome.

**Postconditions**:
- The key does not authenticate while the owner is absent. In P1 it becomes eligible for verification again if the same owner identity returns in an active tenant before the key expires, provided the key has not been explicitly revoked. With `cpt-cf-api-key-management-fr-revoke-orphaned` (P2), authoritative owner absence also causes permanent revocation.

## 9. Acceptance Criteria

Criteria involving platform authentication or downstream credentials verify integration with a compatible AuthN resolver against section 7.2. They do not add implementation of the resolver or its plugin to this gear's scope.

- [ ] An authenticated user issues a key, authenticates a platform request with it, and the resulting `SecurityContext` carries the user's subject ID, subject type, home tenant, and token scopes `["*"]`.
- [ ] The API key secret appears in the issuance response and nowhere else: not in listings, single-key responses, logs, security events, or storage.
- [ ] A key stops authenticating at its expiration time, after revocation within the propagation bound on every instance, and after its owner is deleted or its owner's tenant is suspended within the owner check interval.
- [ ] A user cannot see, inspect, or revoke another user's key; a tenant administrator can list and revoke all keys of a user in an administered tenant, also after the user was deleted.
- [ ] Issue, list, get, revoke, administrative list/revoke, and bulk revoke reject API-key-origin credentials, including exchanged access tokens, and unknown authentication origin through REST and direct or remote SDK invocation. The rejection applies even to owners and tenant administrators, for both the authenticating key and other keys.
- [ ] Management requires an eligible IdP-issued user access token and authorization; issuance additionally rejects restricted token scopes and callers not reported as existing users by Account Management.
- [ ] Issuance is rejected when the owner already has the maximum number of active keys, also under concurrent requests and when the new key is intended as a replacement.
- [ ] Bulk administrative revocation covers all keys active at its cutoff; it does not prevent subsequent issuance.
- [ ] In P1, an unexpired, unrevoked key becomes eligible for verification again when Account Management again reports its recorded owner identity in an active home tenant; an explicitly revoked key remains invalid.
- [ ] When Account Management is unavailable and no reusable owner check exists, verification returns "verification unavailable" and the request is not authenticated.
- [ ] Credentials without the API key prefix and client-credentials exchange keep working through the existing OIDC path unchanged; a credential with the API key prefix that fails verification is not retried through OIDC.
- [ ] Once a changed API key prefix is effective in the gear and the plugin, keys issued under the previous prefix fail authentication; the gear refuses to start with an empty or ambiguous prefix or an invalid lifetime policy.
- [ ] A security event is emitted for every committed issuance and revocation, and none for a repeated revocation.
- [ ] While the gear is operational, a key is removed no later than 90 days plus 24 hours from revocation or expiration, whichever occurs first, after which it is absent from listings, not found on lookup, and fails verification.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| AuthN resolver | Hosts the plugin that routes API keys to this gear, builds the `SecurityContext`, and preserves the API-key origin downstream. | p1 |
| Account Management | Answers whether a key owner exists in an active home tenant; requires deployment-provisioned authorization for the gear's service identity. | p1 |
| AuthZ resolver | Authorizes self-service and administrative operations and requests made with API keys. | p1 |
| Platform database | Stores key metadata and API key secret hashes. | p1 |
| Platform audit gear | Durable audit delivery of lifecycle security events; not implemented yet. | p2 |

## 11. Assumptions

- Account Management user lookup reaches the IdP, so its latency and availability are those of the IdP plugin.
- For operations that permit API key authentication, platform authorization (PDP) decisions are the same as for the owner's OIDC-authenticated requests and reflect the current policy, not the policy at issuance time. Key management separately requires an eligible access token (`cpt-cf-api-key-management-fr-management-auth`).

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| A user disabled but not deleted in the IdP keeps API key access. | Access survives a disablement that blocks OIDC login. | A tenant administrator revokes the user's keys (`cpt-cf-api-key-management-fr-admin-revoke`); automatic detection is deferred to P2 and requires Account Management to expose user enablement. |
| Account Management or the IdP is unavailable. | API-key-authenticated requests fail once reusable owner checks expire. | Fail closed by design with a distinct "verification unavailable" outcome; owner check reuse smooths short outages. |
| The gear's service identity lacks read access in some tenant. | Keys of users in that tenant never authenticate. | Runtime signals distinguish authorization misconfiguration (`cpt-cf-api-key-management-nfr-operational-signals`). |
| API key prefix configured differently in the gear and the plugin. | API keys stop authenticating. | Fail-closed contract (`cpt-cf-api-key-management-contract-authn-routing`); prefix validation at startup. |
| An exchanged downstream token loses the API-key origin. | A leaked key can mint replacement keys through a downstream call. | The AuthN resolver keeps the origin distinguishable (`cpt-cf-api-key-management-contract-authn-context`); all key management requires an eligible access token with a positively established non-API-key origin. |
| Unrestricted token scopes on every key. | A leaked key grants the owner's permissions on APIs that accept API keys; key management remains excluded. | Mandatory expiration, revocation, administrative withdrawal, no management from key-based sessions and no issuance from restricted sessions; per-key scopes are deferred to P2. |
| Security events are lost before the platform audit gear exists. | Incomplete lifecycle evidence for an incident. | Accepted P1 limitation; durable delivery in P2 (`cpt-cf-api-key-management-nfr-audit`). |

## 13. Open Questions

- Are service accounts allowed to own API keys in a later version, and how is their existence checked, given that the Account Management user lookup does not cover them?
- What freshness guarantee does Account Management provide for user deletion and tenant status, and from which observation time is the owner check interval measured? The withdrawal bound in section 9 depends on this contract; a fresh lookup must not silently renew stale source data. Owner: API Key Management and Account Management maintainers; resolve before DESIGN sign-off.
- What are the default and maximum key lifetimes and the default per-owner key limit (proposed: 90 days default, 1 year maximum, 10 active keys)? Owner: gear maintainers; to be decided before DESIGN sign-off.
- Which downstream bearer credential does the AuthN resolver carry for an API-key-authenticated request: the API key itself, or an exchanged IdP access token? Which IdPs and versions support the exchange, how is the API-key origin preserved across hops, and how is the exchanged token's lifetime bounded by the key's expiration, revocation, and owner check? Owner: AuthN resolver maintainers; until resolved, only forwarding to services that accept the API key is supported.

## 14. Traceability

- **Design**: `DESIGN.md` (planned)
- **ADRs**: `ADR/` (planned)
- **Features**: `features/` (planned)
