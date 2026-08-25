# Feature: Service Accounts

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Provision Service Account](#provision-service-account)
  - [List Service Accounts](#list-service-accounts)
  - [Rotate Service Account Secret](#rotate-service-account-secret)
  - [Revoke Service Account](#revoke-service-account)
  - [Reconcile an Ambiguous Provision](#reconcile-an-ambiguous-provision)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Service Account Contract Invocation](#service-account-contract-invocation)
  - [Adapter Text Discard](#adapter-text-discard)
  - [Revoke Idempotency Guard](#revoke-idempotency-guard)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Service Account Contract Trait Surface](#service-account-contract-trait-surface)
  - [One-Time Secret Disclosure](#one-time-secret-disclosure)
  - [No Adapter Text on Wire or in Logs](#no-adapter-text-on-wire-or-in-logs)
  - [Ambiguous Outcome Signaling](#ambiguous-outcome-signaling)
  - [Revoke Idempotency and Non-Disclosure](#revoke-idempotency-and-non-disclosure)
  - [Independent Management Permissions](#independent-management-permissions)
  - [Authenticated Tenant-Scoped Invocation](#authenticated-tenant-scoped-invocation)
  - [No Local Account or Credential Storage](#no-local-account-or-credential-storage)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-service-accounts`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-service-accounts`

## 1. Feature Context

### 1.1 Overview

Owns the machine-identity half of the `IdpPluginClient` contract — tenant-scoped provision, list, rotate-secret, and revoke of confidential OAuth 2.0 `client_credentials` clients — plus the four tenant-scoped operations exposed through both REST (`POST` / `GET /tenants/{tenant_id}/service-accounts`, `POST …/{client_id}/rotate-secret`, `DELETE …/{client_id}`) and the inter-gear `AccountManagementClient`. A service account is a tenant-owned identity for a workload that must authenticate without human credentials; issued tokens carry the owning `tenant_id` and a service-subject `user_type`.

The feature registers `gts.cf.core.am.service_account.v1~` as a managed resource distinct from `gts.cf.core.am.user.v1~`, together with four independently grantable permissions. Concrete provider adapters conform to the contract but ship outside this gear, exactly as for the user half.

### 1.2 Purpose

Platform workloads need identities for service-to-service access, automation, and unattended jobs. Reusing human accounts weakens accountability, ties workload availability to a person's lifecycle, and makes credential rotation a manual coordination problem; administering the identity provider directly gives every consumer a different integration surface and error model.

This feature answers that the same way AM answers user provisioning: it is the tenant-scoped authorization and delegation boundary, and the `IdP` stays the source of truth. It holds no account table and no credential store — every read and write is a live pass-through per `cpt-cf-account-management-constraint-no-user-storage`, so a restart loses nothing.

Two obligations distinguish it from the user surface, and both come from the fact that what crosses the boundary here is a **live credential** rather than a profile:

- **The secret is disclosed exactly once**, by provision and rotate, in a response no intermediary may cache. There is no read-back path anywhere in the contract; recovery from a lost secret is a rotation.
- **No provider-supplied text reaches a caller or a log.** The user half forwards a digest of the provider `detail` for operator correlation; here even that is dropped, because a vendor error string on a credential-minting call is the single most likely place for a secret to appear, and a credential (`secret=abc123`) is ordinary ASCII no filter can distinguish from operator prose. AM answers each failure category with a fixed message it owns and records only the category, the discarded text's length, and whether a field was attributed.

**Requirements**: `cpt-cf-account-management-fr-service-account-provision`, `cpt-cf-account-management-fr-service-account-list`, `cpt-cf-account-management-fr-service-account-rotate`, `cpt-cf-account-management-fr-service-account-revoke`, `cpt-cf-account-management-fr-service-account-secret-confidentiality`

`cpt-cf-account-management-nfr-authentication-context` and `cpt-cf-account-management-nfr-data-classification` are cited normatively below but **owned** by `feature-idp-user-operations-contract` and `feature-errors-observability` respectively — this feature satisfies them, it does not define them.

**Principles**: `cpt-cf-account-management-principle-idp-agnostic`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Authenticated caller of every service-account endpoint; acts within an authorized tenant scope per platform AuthN/AuthZ contracts. A platform administrator performs the same workflow across broader scopes when policy grants it — not a separate workflow. |
| `cpt-cf-account-management-actor-machine-workload` | The workload the account exists for. Never calls AM: it uses the issued credentials against the `IdP`'s token endpoint through the `client_credentials` grant, and stops being able to authenticate once the credential is superseded or revoked. |
| `cpt-cf-account-management-actor-idp` | External system reached through `IdpPluginClient` via `ClientHub` plugin resolution; owns authoritative account state, enforces name syntax / scope allowlist / per-tenant quota, and classifies its own outcomes into the contract's failure categories. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.5 IdP Tenant & User Operations Contract (`fr-service-account-provision`, `fr-service-account-list`, `fr-service-account-rotate`, `fr-service-account-revoke`, `fr-service-account-secret-confidentiality`); §6.2 Authentication Context (`nfr-authentication-context`); §6.9 Data Classification; §7.1 Service Account API.
- **Design**: [DESIGN.md](../DESIGN.md) §2.1 IdP-Agnostic Principle (`principle-idp-agnostic`); §3.2 Component Model (`ServiceAccountService`); §3.3 Service Account REST API (`interface-service-account-rest`) and `AccountManagementClient` SDK Trait (`interface-sdk-client`); §3.8 Error Codes Reference (`Aborted` / `AMBIGUOUS_OUTCOME`, `Unimplemented`, `ServiceUnavailable`); [ADR-0001](../ADR/0001-cpt-cf-account-management-adr-idp-contract-separation.md) (`adr-idp-contract-separation`).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.9 Service Accounts.
- **Dependencies**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` — owns tenant existence and the `AccessScope`-clamped tenant resolution this feature runs before every provider call, and owns the companion `deprovision_tenant` hook through which a tenant's accounts are removed at offboarding.
  - `cpt-cf-account-management-feature-idp-user-operations-contract` — sibling half of the same `IdpPluginClient` trait; shares the plugin resolution, the resolved `TenantContext` shape (including the plugin-private `metadata` replay), and the tenant guard.
  - `cpt-cf-account-management-feature-errors-observability` — owns the canonical envelope this feature's categories are rendered through.

## 2. Actor Flows (CDSL)

### Provision Service Account

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-service-accounts-provision`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin POSTs a name and optional scopes to `/tenants/{tenant_id}/service-accounts`; `ServiceAccountService` authorizes the `create` action, resolves the target tenant to an `active` one, invokes `IdpPluginClient::provision_service_account` with the resolved tenant context, and returns 201 Created carrying the client id, the one-time plaintext secret, the token endpoint, and the subject id — with `Location` addressing the new account and `Cache-Control: no-store` on the response.

**Error Scenarios**:

- Caller not authorized for `{tenant_id}` — `permission_denied`; no tenant read, no provider call.
- `{tenant_id}` does not resolve to an `active` tenant, or lies outside the caller's subtree — `not_found` / `validation`; no provider call. An out-of-subtree tenant is reported as absent, so tenant topology is not disclosed.
- Request exceeds an AM boundary cap (empty / oversized name, too many scopes, oversized scope) — `validation`; no provider call.
- Provider rejects the request with nothing retained (name already live in the tenant, name syntax, scope outside the allowlist, quota) — `invalid_argument` attributed to `request`, carrying AM's fixed message.
- Provider outcome is ambiguous — `aborted` (409) with `reason = AMBIGUOUS_OUTCOME`; see `flow-service-accounts-reconcile`.
- Provider fails cleanly — `service_unavailable`; nothing retained, so the same request may be retried.
- Deployment's adapter ships no service-account support — `unimplemented` (501); never a simulated success.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` via platform AuthN middleware per `nfr-authentication-context` - `inst-flow-sa-provision-validate-caller`
2. [ ] - `p1` - Authorize the `create` action on `{tenant_id}` through `PolicyEnforcer`, fail-closed - `inst-flow-sa-provision-authorize`
3. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant under the compiled `AccessScope`; forward any not-found / non-active outcome as the envelope-mapped error - `inst-flow-sa-provision-resolve-tenant`
4. [ ] - `p1` - Bound the caller's inputs (trimmed non-empty name within the AM cap; scope count and per-scope length within their caps) — syntax and allowlist membership remain the provider's to judge - `inst-flow-sa-provision-bound-inputs`
5. [ ] - `p1` - Invoke `algo-service-accounts-contract-invocation` with operation `provision_service_account(tenant_context, name, scopes)` - `inst-flow-sa-provision-invoke-contract`
6. [ ] - `p1` - **IF** the contract returned any failure category **RETURN** the mapped error through the `feature-errors-observability` envelope, carrying AM's fixed message per `algo-service-accounts-adapter-text-discard`; no AM state is mutated (AM owns none) - `inst-flow-sa-provision-failure-return`
7. [ ] - `p1` - Emit the `service_account_provisioned` audit line carrying `tenant_id`, `client_id`, the submitted `name`, and the actor — never the credential - `inst-flow-sa-provision-audit`
8. [ ] - `p1` - **RETURN** 201 Created with the credentials, a `Location` header addressing the account, and `Cache-Control: no-store` - `inst-flow-sa-provision-success-return`

### List Service Accounts

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-service-accounts-list`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin GETs `/tenants/{tenant_id}/service-accounts`; the service authorizes `list`, resolves the tenant, invokes `IdpPluginClient::list_service_accounts`, and returns 200 OK with every account the provider reports for that tenant — each carrying its client id, the caller-supplied `name` reported verbatim, its enabled state, and its attached scopes. The listing is unpaginated and carries no secret.
- A tenant with no accounts returns 200 OK with an empty collection, never a 404.

**Error Scenarios**:

- Caller not authorized, tenant absent / non-active / out-of-subtree, provider unavailable, or adapter without support — as in `flow-service-accounts-provision`. No stale or partial inventory is served during a provider outage.
- Because listing is a non-retaining read, a provider-reported `Ambiguous` outcome is returned as the clean-failure category (`service_unavailable`, 503): retrying the read is safe, and returning the mutation-oriented `AMBIGUOUS_OUTCOME` steer would incorrectly tell the caller to reconcile by issuing the same list operation that just failed.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` - `inst-flow-sa-list-validate-caller`
2. [ ] - `p1` - Authorize the `list` action on `{tenant_id}`, fail-closed - `inst-flow-sa-list-authorize`
3. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant under the compiled `AccessScope` - `inst-flow-sa-list-resolve-tenant`
4. [ ] - `p1` - Invoke `algo-service-accounts-contract-invocation` with operation `list_service_accounts(tenant_context)` - `inst-flow-sa-list-invoke-contract`
5. [ ] - `p1` - **IF** the contract returned any failure category **RETURN** the mapped error; for this non-retaining read only, remap `Ambiguous` to the clean-failure `service_unavailable` category because retrying cannot duplicate state; NO stale inventory is served per `principle-idp-agnostic` - `inst-flow-sa-list-failure-return`
6. [ ] - `p1` - **RETURN** 200 OK with the collection, each entry reporting the caller-supplied `name` unchanged so a submitted name maps back to an opaque client id - `inst-flow-sa-list-success-return`

### Rotate Service Account Secret

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-service-accounts-rotate`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin POSTs `/tenants/{tenant_id}/service-accounts/{client_id}/rotate-secret`; the service authorizes the distinct `rotate_secret` action, resolves the tenant, invokes `IdpPluginClient::rotate_service_account_secret`, and returns 200 OK with a new one-time secret under `Cache-Control: no-store`. The account's identity — client id and subject id — is unchanged; the previous secret stops working.

**Error Scenarios**:

- `{client_id}` does not resolve within `{tenant_id}` — `not_found` (404) carrying the addressed client id. Unlike revoke, absence is **NOT** folded into success: a rotation that found nothing minted no credential the caller could use. An account owned by a different tenant is reported identically, so rotation is not a probe for other tenants' accounts.
- Ambiguous, clean-failure, unsupported, unauthorized, and tenant-guard outcomes — as in `flow-service-accounts-provision`.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` - `inst-flow-sa-rotate-validate-caller`
2. [ ] - `p1` - Authorize the `rotate_secret` action on `{tenant_id}`, fail-closed - `inst-flow-sa-rotate-authorize`
3. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant under the compiled `AccessScope` - `inst-flow-sa-rotate-resolve-tenant`
4. [ ] - `p1` - Invoke `algo-service-accounts-contract-invocation` with operation `rotate_service_account_secret(tenant_context, client_id)` - `inst-flow-sa-rotate-invoke-contract`
5. [ ] - `p1` - **IF** the contract reported the account absent **RETURN** `not_found` (404) with the addressed `client_id` as the resource — the caller's own path input, so echoing it discloses nothing the caller did not supply - `inst-flow-sa-rotate-not-found-return`
6. [ ] - `p1` - **IF** the contract returned any other failure category **RETURN** the mapped error per `algo-service-accounts-adapter-text-discard` - `inst-flow-sa-rotate-failure-return`
7. [ ] - `p1` - Emit the `service_account_secret_rotated` audit line carrying `tenant_id`, `client_id`, and the actor — never the credential - `inst-flow-sa-rotate-audit`
8. [ ] - `p1` - **RETURN** 200 OK with the new credentials under `Cache-Control: no-store` - `inst-flow-sa-rotate-success-return`

### Revoke Service Account

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-service-accounts-revoke`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin DELETEs `/tenants/{tenant_id}/service-accounts/{client_id}`; the service authorizes `revoke`, resolves the tenant, invokes `IdpPluginClient::revoke_service_account`, and returns 204 No Content. The response carries no account state and no credential material.
- The provider reports the account already absent — folded into the same 204 per `algo-service-accounts-revoke-idempotency-guard`, so a retried DELETE stays safe.

**Error Scenarios**:

- Caller not authorized, tenant absent / non-active / out-of-subtree, provider clean failure or ambiguity, or adapter without support — as in `flow-service-accounts-provision`. A clean failure is **not** folded into success: only a reported absence is.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` - `inst-flow-sa-revoke-validate-caller`
2. [ ] - `p1` - Authorize the `revoke` action on `{tenant_id}`, fail-closed - `inst-flow-sa-revoke-authorize`
3. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant under the compiled `AccessScope` - `inst-flow-sa-revoke-resolve-tenant`
4. [ ] - `p1` - Invoke `algo-service-accounts-contract-invocation` with operation `revoke_service_account(tenant_context, client_id)` - `inst-flow-sa-revoke-invoke-contract`
5. [ ] - `p1` - Apply `algo-service-accounts-revoke-idempotency-guard` to the provider outcome - `inst-flow-sa-revoke-idempotency-check`
6. [ ] - `p1` - **IF** the guard reported a non-absence failure **RETURN** the mapped error per `algo-service-accounts-adapter-text-discard` - `inst-flow-sa-revoke-failure-return`
7. [ ] - `p1` - Emit the `service_account_revoked` audit line carrying `tenant_id`, `client_id`, and the actor - `inst-flow-sa-revoke-audit`
8. [ ] - `p1` - **RETURN** 204 No Content, whether the account was removed on this call or reported already absent - `inst-flow-sa-revoke-success-return`

### Reconcile an Ambiguous Provision

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-service-accounts-reconcile`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

This is the caller-side recovery flow the 409 `AMBIGUOUS_OUTCOME` response exists to enable. AM performs no step of it automatically — there is no idempotency key, no retry worker, and no reconciliation state (see §7).

**Deliberately carries no `@cpt-*` code markers.** Every other flow and process in this document is anchored to the implementation, but this one is executed by the caller: the AM-side halves it composes are already marked under `flow-service-accounts-list`, `-rotate`, `-revoke`, and `-provision`. Its steps are here so the recovery procedure the 409 obliges a caller to follow is specified somewhere normative, not because AM runs them.

**Success Scenarios**:

- After a 409 `AMBIGUOUS_OUTCOME` on provision, the caller lists the tenant, finds the entry whose `name` equals the one it submitted, and resolves the uncertainty by that entry's `client_id`: rotate-secret to obtain usable credentials without deleting, or revoke followed by a fresh provision.
- The name is absent from the listing, proving the provision did not land; a plain retry is then safe.

**Error Scenarios**:

- Two live entries carry the submitted name — the provider is violating the uniqueness obligation of `dod-service-accounts-contract-trait-surface`. The caller **MUST NOT** guess which to act on; this is an operator escalation, not a recoverable state.

**Steps**:

1. [ ] - `p1` - **IF** provision returned `aborted` with `reason = AMBIGUOUS_OUTCOME`, do NOT retry the same request — a retry against a landed provision returns `invalid_argument` ("name already live") and tells the caller nothing about the credential - `inst-flow-sa-reconcile-no-blind-retry`
2. [ ] - `p1` - Invoke `flow-service-accounts-list` for the tenant and match entries on the submitted `name`, never on a derived client-id format - `inst-flow-sa-reconcile-correlate`
3. [ ] - `p1` - **IF** exactly one entry matches, resolve it by its `client_id` — `flow-service-accounts-rotate` to obtain usable credentials, or `flow-service-accounts-revoke` then `flow-service-accounts-provision` - `inst-flow-sa-reconcile-resolve-match`
4. [ ] - `p1` - **IF** no entry matches, re-issue `flow-service-accounts-provision` - `inst-flow-sa-reconcile-retry-safe`
5. [ ] - `p1` - **IF** more than one entry matches, escalate: the provider broke the `(tenant_id, name)` uniqueness obligation and no automated choice is safe - `inst-flow-sa-reconcile-escalate`

## 3. Processes / Business Logic (CDSL)

### Service Account Contract Invocation

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-service-accounts-contract-invocation`

**Input**: Operation name (`provision_service_account` / `list_service_accounts` / `rotate_service_account_secret` / `revoke_service_account`), the resolved `TenantContext`, and the operation-specific payload.

**Output**: Contract-level outcome: success with the provider-returned credentials or summaries, or one of the five failure categories (`InvalidInput`, `NotFound`, `CleanFailure`, `Ambiguous`, `UnsupportedOperation`).

**Steps**:

> The contract surface is `IdpPluginClient` — the same trait and the same `ClientHub` resolution the user half uses, so a deployment registers one adapter and gets both. Every method ships a default implementation returning `UnsupportedOperation`, which is what makes a tenant-only or user-only adapter legal; a provider that *can* perform a mutating operation **MUST NOT** silently no-op.
>
> Two ownership facts the steps below depend on. **Timeouts** are governed by platform configuration and are observable; this feature owns no per-operation retry, backoff, rate-limiting, or circuit-breaking policy — a provider that exhausts its own budget reports a failure category and AM surfaces it, exactly as on the user half. **Every call runs outside any database transaction.** That is the premise the ambiguous-outcome design rests on: AM commits nothing alongside the provider call, so there is no local state to roll back and nothing to reconcile except what the provider may have retained. A provider call inside a transaction would make `Ambiguous` a distributed-commit problem rather than a caller-side reconciliation one.

1. [ ] - `p1` - Resolve the active `IdpPluginClient` instance via `ClientHub` plugin registration - `inst-algo-sa-contract-invocation-resolve-plugin`
2. [ ] - `p1` - Package the operation payload with the resolved `TenantContext` — `(tenant_id, tenant_name, tenant_type, metadata)`, where `metadata` is the plugin-private blob AM replays from `tenant_idp_metadata` so the adapter can route to its own vendor-side realm or organization - `inst-algo-sa-contract-invocation-package-request`
3. [ ] - `p1` - Invoke the resolved operation exactly once per logical request; retry, backoff, and rate-limiting policy belong to the adapter - `inst-algo-sa-contract-invocation-invoke`
4. [ ] - `p1` - **IF** the provider reported `InvalidInput` **RETURN** `(reject, invalid_argument)` attributed to `request` as a whole - `inst-algo-sa-contract-invocation-invalid-input-return`
5. [ ] - `p1` - **IF** the provider reported `NotFound` **RETURN** `(reject, not_found)`, leaving the caller's flow to decide whether absence is a failure (rotate) or a success (revoke) - `inst-algo-sa-contract-invocation-not-found-return`
6. [ ] - `p1` - **IF** the provider reported `CleanFailure` **RETURN** `(reject, service_unavailable)` — nothing was retained, so retrying the same request is safe - `inst-algo-sa-contract-invocation-clean-failure-return`
7. [ ] - `p1` - **IF** the provider reported `Ambiguous` for a retaining or mutating operation **RETURN** `(reject, aborted, reason=AMBIGUOUS_OUTCOME)` — never success, and never the retry-same signal a `service_unavailable` carries; the non-retaining `list` flow explicitly remaps this category to `service_unavailable` because replaying a read is safe - `inst-algo-sa-contract-invocation-ambiguous-return`
8. [ ] - `p1` - **IF** the provider reported `UnsupportedOperation` **RETURN** `(reject, unimplemented)` — the deployment's adapter implements no machine-identity management - `inst-algo-sa-contract-invocation-unsupported-return`
9. [ ] - `p1` - **ELSE** **RETURN** success with the provider-returned credentials or summaries - `inst-algo-sa-contract-invocation-success-return`

### Adapter Text Discard

- [x] `p1` - **ID**: `cpt-cf-account-management-algo-service-accounts-adapter-text-discard`

**Input**: A provider-returned failure carrying an untrusted `detail` string and, for `InvalidInput`, an optionally attributed `field` name.

**Output**: A domain error whose human-readable text is one of AM's fixed, gear-owned messages, plus a log record carrying only safe metadata.

**Steps**:

> This is the one place the machine-identity boundary is deliberately stricter than the user half, which forwards an FNV digest of the provider `detail` for operator correlation. Filtering was tried and removed: a credential such as `secret=abc123` is ordinary ASCII graphic text, so no character filter, length cap, or control-character strip can separate it from operator prose — each only launders or bounds a leak. The consequence for implementors is stated normatively in the SPI contract: **an adapter MUST log its own diagnostics in-process**, where it alone knows what is safe to emit.

1. [x] - `p1` - Select the fixed message for the known failure category — invalid-input, upstream-unavailable, ambiguous-reconcile, or unsupported — and discard the provider's `detail` entirely; an unknown future category uses the internal mapping-gap answer from step 5 - `inst-algo-sa-text-discard-select-fixed-message`
2. [x] - `p1` - Discard any adapter-attributed `field`; attribute every provider-sourced rejection to `request` as a whole - `inst-algo-sa-text-discard-neutral-field`
3. [x] - `p1` - Record one log line carrying the category label, the discarded text's length, and whether a field was attributed — never the text or the field value, so no subscriber, file, or aggregator can hold what the response withholds - `inst-algo-sa-text-discard-safe-metadata-log`
4. [x] - `p1` - **IF** the failure is a routine reported absence (how an idempotent revoke confirms its work) emit no record of its own; **IF** it is a caller-attributable rejection record it at debug level, not at the default level an operator watches for provider trouble - `inst-algo-sa-text-discard-log-level`
5. [x] - `p1` - **IF** the failure category is not one this mapping knows (a future contract addition) **RETURN** the conservative internal answer and emit a loud error-level record naming the unmapped category — AM cannot truthfully promise the unavailable category's “nothing retained; retry is safe” semantics until the new category is classified, and the mapping gap **MUST NOT** become the one path that leaks adapter text - `inst-algo-sa-text-discard-unknown-category`

### Revoke Idempotency Guard

- [x] `p1` - **ID**: `cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard`

**Input**: `tenant_id`, `client_id`, and the provider-returned outcome of a `revoke_service_account` call.

**Output**: Success when the account is gone in this tenant scope after the call — whether removed now or already absent — and pass-through for every other failure category.

**Steps**:

> Idempotency by error-mapping, as on the tenant side: the adapter maps a vendor "client does not exist" response 1:1 to `NotFound`, and AM decides what that means. Adapters map vendor errors; AM business logic assigns meaning. The same fold discharges the non-disclosure obligation — an address owned by another tenant is reported exactly like one that never existed, so revoke never becomes a probe.

1. [x] - `p1` - **IF** the provider returned success **RETURN** idempotent success (caller answers 204) - `inst-algo-sa-revoke-idempotency-removed-return`
2. [x] - `p1` - **IF** the provider reported the account absent **RETURN** the same idempotent success, indistinguishable from the removal case - `inst-algo-sa-revoke-idempotency-absent-return`
3. [x] - `p1` - **ELSE** **RETURN** pass-through of the failure category — a clean failure or an ambiguous outcome is **NOT** absence-equivalent and **MUST NOT** be reported as a successful revoke - `inst-algo-sa-revoke-idempotency-other-return`

## 4. States (CDSL)

**Not applicable.** This feature owns no AM-side tables, projections, or caches: the account record, its enabled state, its scopes, and its credential all live in the `IdP` behind `IdpPluginClient`. The only lifecycle AM observes is "live" versus "absent", and it observes it per call rather than storing it. Tenant lifecycle states (`provisioning` / `active` / `suspended` / `deleted`) that gate every operation are owned by `feature-tenant-hierarchy-management`.

## 5. Definitions of Done

### Service Account Contract Trait Surface

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-contract-trait-surface`

The system **MUST** expose `provision_service_account`, `list_service_accounts`, `rotate_service_account_secret`, and `revoke_service_account` on the `IdpPluginClient` trait — the same trait and the same scoped `ClientHub` resolution as the tenant and user halves, per `adr-idp-contract-separation` — as the single outbound integration point from AM to its identity provider. Each method **MUST** ship a default implementation returning the `UnsupportedOperation` category so a tenant-only or user-only adapter compiles without stubs, and AM **MUST** surface that category as `unimplemented` (501) rather than as a simulated success. Handler code **MUST NOT** hard-code provider-specific logic, call a provider library directly, or bypass plugin resolution.

The public inter-gear `AccountManagementClient` **MUST** expose the corresponding create, list, rotate-secret, and revoke operations through AM's global `ClientHub` registration. Those methods **MUST** delegate to `ServiceAccountService`, preserving the same PEP authorization, active-tenant guard, input validation, audit, provider-failure translation, and canonical-error envelope as the REST surface; sibling gears **MUST NOT** need an HTTP call into AM to manage a machine identity. The AM-facing methods accept the caller's `SecurityContext`, tenant id, and ordinary operation inputs — they **MUST NOT** expose the provider-facing `TenantContext`, which AM resolves only after authorization.

Every invocation **MUST** carry the resolved `TenantContext` — `(tenant_id, tenant_name, tenant_type, metadata)` — so an adapter can resolve its vendor-side context exactly as it does for user operations; `metadata` is the plugin-private blob AM replays from `tenant_idp_metadata` and never inspects.

`(tenant_id, client_id)` **MUST** be the scoped resource address: an address that does not resolve within the addressed tenant **MUST** be reported as the absence category and **MUST NOT** act on any account, whatever tenant owns it. Client-id **formats** are adapter conventions and **MUST NOT** be contract — an `IdP` that assigns opaque ids conforms. The bridge from a caller-chosen name to an adapter-assigned id is the listing entry's `name`, which providers **MUST** report verbatim.

`(tenant_id, name)` **MUST** identify at most one live account: implementors **MUST** reject a provision whose name is already live in the tenant with the invalid-input category, **MUST NOT** resume, reveal, or modify the existing account — including a half-created one left behind by an earlier ambiguous outcome — and **MUST** make that check atomic, so two concurrent provisions for the same `(tenant_id, name)` cannot both succeed. Without that uniqueness the correlation key could match several entries and a recovering caller could act on the wrong identity.

Adapters **MUST** delete a tenant's accounts when that tenant is deprovisioned; the hook is the existing `deprovision_tenant` call owned by `feature-tenant-hierarchy-management`. AM performs no account-level offboarding orchestration of its own.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-provision`
- `cpt-cf-account-management-flow-service-accounts-list`
- `cpt-cf-account-management-flow-service-accounts-rotate`
- `cpt-cf-account-management-flow-service-accounts-revoke`
- `cpt-cf-account-management-algo-service-accounts-contract-invocation`

**Constraints**: `cpt-cf-account-management-constraint-legacy-integration`

**Touches**:

- Entities: `ServiceAccount`, `ServiceAccountCredentials`, `TenantId`
- Data: `gts://gts.cf.core.am.service_account.v1~` (managed-resource type)
- Sibling integration: global `AccountManagementClient` resolution plus scoped `IdpPluginClient` provider resolution through `ClientHub`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### One-Time Secret Disclosure

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-one-time-secret`

The system **MUST** disclose a plaintext client secret only in successful provision and rotate-secret results, and **MUST NOT** expose any read-back path for it — no by-id read, no listing field, no SDK read-back accessor. Both HTTP credential-bearing responses **MUST** carry `Cache-Control: no-store` so no intermediary retains the body. In process the secret **MUST** remain in a redacting, zeroize-on-drop, non-serializable secret type from the provider boundary through the public `AccountManagementClient` result. A REST caller receives the one-time value only through the credential DTO conversion; an in-process caller may expose it only to transfer it into its own credential custodian. Neither path permits AM to persist, log, cache, audit, or re-read the plaintext.

The listing model **MUST NOT** carry a secret field, and no future read model derived from it may add one. The caller-supplied `name` a listing entry carries for correlation is non-secret caller input and **MUST NOT** be treated as, or replaced by, credential material — inventory access must not become credential access.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-provision`
- `cpt-cf-account-management-flow-service-accounts-list`
- `cpt-cf-account-management-flow-service-accounts-rotate`

**Touches**:

- Entities: `ServiceAccountCredentials`
- Sibling integration: platform secret-management plane (the caller's own credential store) — AM neither writes to it nor reads from it.

### No Adapter Text on Wire or in Logs

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-no-adapter-text`

The system **MUST** treat every `detail` and `field` string a provider supplies with a service-account failure as untrusted text of unknown classification, and **MUST NOT** surface it — to a caller or to its own logs — in any form, whether whole, filtered, truncated, or digested. Each failure category **MUST** be answered with a fixed message AM owns, and the record AM keeps **MUST** be confined to safe metadata: the category label, the discarded text's length, and whether a field was attributed. Provider-attributed field names **MUST NOT** appear in a field violation; every provider-sourced rejection is attributed to `request` as a whole.

The stricter posture relative to the tenant and user halves (which forward an FNV digest for operator correlation) is deliberate and **MUST NOT** be relaxed to match them: this is the surface whose vendor errors are most likely to quote a freshly minted credential, and a credential is indistinguishable from prose to any filter. A failure category this mapping does not know **MUST** degrade to the conservative internal answer with a loud operator record — AM cannot assert that no state was retained or that retry is safe until it understands the new category, and it must never choose a path that forwards the text.

Because AM records none of the adapter's text, an adapter **MUST** log its own diagnostics in-process; a failure whose cause the adapter describes only in that text is a failure no operator can diagnose.

**Implements**:

- `cpt-cf-account-management-algo-service-accounts-adapter-text-discard`
- `cpt-cf-account-management-algo-service-accounts-contract-invocation`

**Touches**:

- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); categories referenced by name only.

### Ambiguous Outcome Signaling

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-ambiguous-outcome`

The system **MUST** report a provider outcome the adapter classifies as ambiguous on a retaining or mutating operation — state may have been retained — as its own distinct category: never as success, and never as the same category used for a safely retryable clean failure. It **MUST** surface as `aborted` (409) with `reason = AMBIGUOUS_OUTCOME` rather than `service_unavailable` (503), because a 503 advertises "retry the same request" and a retry against a landed provision would come back as an invalid-input name collision, telling the caller nothing about the credential. The non-retaining list operation is the explicit exception: it maps provider uncertainty to `service_unavailable`, because replaying the read cannot duplicate state and a reconciliation steer would only direct the caller back to the failed operation.

The response's human-readable text **MUST** steer the caller into reconciliation rather than retry. After such an outcome a caller **MUST** be able to determine, against any conforming provider, whether the account it asked for now exists: it lists the tenant, matches on the `name` it submitted, and then either rotates that account's secret or revokes it and provisions again. That determination **MUST NOT** require parsing a client-id format.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-provision`
- `cpt-cf-account-management-flow-service-accounts-rotate`
- `cpt-cf-account-management-flow-service-accounts-reconcile`
- `cpt-cf-account-management-algo-service-accounts-contract-invocation`

**Touches**:

- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); the `AMBIGUOUS_OUTCOME` reason token is declared in the AM SDK's `reason::aborted` vocabulary.

### Revoke Idempotency and Non-Disclosure

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-revoke-idempotency`

The system **MUST** treat revocation of an account the provider reports absent as indistinguishable from revocation of a present one — 204 No Content in both cases — so `DELETE` remains idempotent and retry-safe. Adapters **MUST** map vendor "client does not exist" responses to the absence category rather than folding them into success themselves; AM owns the interpretation. A clean failure or an ambiguous outcome **MUST NOT** be treated as absence-equivalent.

That same indistinguishability carries the non-disclosure obligation: an address that does not resolve within the target tenant is reported exactly as one that never existed, so revoke cannot be used to probe for accounts owned elsewhere. Rotation expresses the obligation differently and satisfies it equally — it reports absence as `not_found` whether the account never existed or belongs to another tenant. In neither case can a caller separate the three.

Authorization for the target tenant is evaluated before any address lookup, so a caller without permission learns no addressing outcome at all.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-revoke`
- `cpt-cf-account-management-flow-service-accounts-rotate`
- `cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard`

**Touches**:

- Entities: `ServiceAccount`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### Independent Management Permissions

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-independent-permissions`

The system **MUST** register `gts.cf.core.am.service_account.v1~` as a managed-resource type distinct from `gts.cf.core.am.user.v1~`, and **MUST** authorize `create`, `list`, `rotate_secret`, and `revoke` as four independently grantable `AuthzPermissionV1` instances against it. Sharing the user resource type is forbidden: a "manage users" grant **MUST NOT** confer machine-credential minting, and secret rotation has no user-side action to inherit a permission from. `rotate_secret` **MUST** stay separate from `create` so an operator who may re-key an existing account need not be able to mint new ones.

The resource type **MUST** remain distinct from the service-account *subject* classification type (`cf.core.security.subject_service.v1~`) — that is what the account IS when it authenticates; this is what RBAC protects when it is managed — and the two **MUST** remain in separate namespaces (`cf.core.am` vs `cf.core.security`), compared for equality wherever either is classified, so neither can be substituted for the other.

Every operation **MUST** be authorized against the explicit owning tenant. The tenant read that follows runs under the PDP-compiled `AccessScope`, so a caller whose grant does not cover the target tenant cannot resolve it and receives the absent answer — the clamp is evaluated against AM's `tenant_closure`, which is strictly stronger than comparing the target against a scope's uuid set. Authorization **MUST** fail closed: a denied decision, a constraint shape AM cannot compile, and a PDP evaluation failure all deny.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-provision`
- `cpt-cf-account-management-flow-service-accounts-list`
- `cpt-cf-account-management-flow-service-accounts-rotate`
- `cpt-cf-account-management-flow-service-accounts-revoke`

**Touches**:

- Data: `gts://gts.cf.core.am.service_account.v1~`; `gts.cf.toolkit.authz.permission.v1~cf.core.am.service_account_{create,list,rotate_secret,revoke}.v1`
- Sibling integration: `types-registry` link-time GTS inventory; `PolicyEnforcer` / AuthZ Resolver (external to this feature's surface)

### Authenticated Tenant-Scoped Invocation

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-authenticated-tenant-scoped-invocation`

Every service-account endpoint **MUST** require a valid `SecurityContext` from the platform AuthN pipeline per `nfr-authentication-context`; unauthenticated calls **MUST** return 401 without invoking the contract. Every contract invocation **MUST** carry a `tenant_id` resolved to an `active` tenant per `feature-tenant-hierarchy-management`; operations against a non-existent, `provisioning`, `suspended`, or `deleted` tenant **MUST** fail before the provider call is issued.

Guard ordering **MUST** be: PEP gate, then tenant existence and status, then AM boundary input caps, then the provider call. The gate runs first so a denied caller is refused before any tenant read or provider round-trip; the tenant guard runs before the caps so a request against an invisible tenant does not surface as a payload-shape error.

AM **MUST** bound the caller's inputs before forwarding — a trimmed, non-empty name of at most 64 Unicode scalars, at most 32 scopes, and at most 256 Unicode scalars per scope — so a megabyte-scale payload never rides the wire. Those bounds are **caps, not policy**: name charset, scope-allowlist membership, and per-tenant quota remain the provider's to enforce and surface as the invalid-input category, because a deployment's `IdP` owns client-id derivation and its own limits.

The injection classes the security baseline enumerates are **not applicable by construction, and MUST stay that way**: this feature builds no query language, shell command, filesystem path, or markup from caller input. Names and scopes are forwarded to the provider as structured request fields, never interpolated; AM persists nothing, so there is no SQL to parameterise. Exactly one caller-supplied value is reflected back to a caller — the addressed `client_id`, echoed as the structured `resource_name` of a rotate `404`. That is safe because it is the caller's own path input returned as a typed envelope field rather than interpolated into a message, and because it is the caller's own submission rather than provider text; it **MUST NOT** become a precedent for echoing anything the provider supplied, which `dod-service-accounts-no-adapter-text` forbids outright.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-provision`
- `cpt-cf-account-management-flow-service-accounts-list`
- `cpt-cf-account-management-flow-service-accounts-rotate`
- `cpt-cf-account-management-flow-service-accounts-revoke`

**Touches**:

- Entities: `TenantId`
- Sibling integration: platform AuthN middleware (`SecurityContext`), `PolicyEnforcer`, and the shared tenant-resolve guard owned by `feature-tenant-hierarchy-management`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### No Local Account or Credential Storage

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-service-accounts-no-local-storage`

The system **MUST NOT** maintain any AM-side service-account table, inventory projection, credential cache, or provider-state replica. Every read and write **MUST** be a live pass-through to the `IdP` through the contract, and no fallback to a local store is permitted when the provider is unavailable — a listing during an outage **MUST** return the envelope-mapped error, never a stale or partial inventory. AM **MUST** hold no plaintext or replayable credential in any table, file, or cache, so a gear restart requires no state restoration and loses no authoritative account state.

**Implements**:

- `cpt-cf-account-management-flow-service-accounts-list`
- `cpt-cf-account-management-algo-service-accounts-contract-invocation`

**Constraints**: `cpt-cf-account-management-constraint-no-user-storage`

**Touches**:

- Entities: `ServiceAccount`, `ServiceAccountCredentials`
- Data: none — this feature owns no AM-side table.

## 6. Acceptance Criteria

- [ ] An authenticated tenant-admin `POST /tenants/{tenant_id}/service-accounts` on an `active` tenant authorizes the `create` action, resolves the tenant, invokes `IdpPluginClient::provision_service_account` through `ClientHub` with the resolved `TenantContext` (including the replayed plugin-private `metadata`), and returns 201 Created carrying `client_id`, `client_secret`, `token_url`, and `subject_id`, with a `Location` header addressing the account and `Cache-Control: no-store` on the response. No AM-side row is written. Fingerprints `dod-service-accounts-contract-trait-surface`, `dod-service-accounts-one-time-secret`, `dod-service-accounts-no-local-storage`.
- [ ] A sibling gear resolving `dyn AccountManagementClient` from the global `ClientHub` can create, list, rotate, and revoke service accounts without an HTTP call; each method runs the same service-layer authorization and tenant guards as REST, returns the same canonical failure categories, and never exposes provider-only `TenantContext` construction to the caller. Fingerprints `dod-service-accounts-contract-trait-surface`, `dod-service-accounts-authenticated-tenant-scoped-invocation`.
- [ ] A `POST` whose `name` is already live in the tenant returns 400 `invalid_argument` attributed to `request`, and the response body contains neither the existing account's `client_id` nor any credential — the existing account is not resumed, revealed, or modified. Fingerprints `dod-service-accounts-contract-trait-surface`, `dod-service-accounts-no-adapter-text`.
- [ ] A provider failure whose `detail` contains credential-shaped text (`secret=abc123`) produces a response carrying only AM's fixed message for that category, and no log record contains the text or an adapter-attributed field value; the record does carry the category label, `detail_len`, and `field_present`. Fingerprints `dod-service-accounts-no-adapter-text`.
- [ ] A provision whose provider outcome is ambiguous returns 409 with `context.reason = AMBIGUOUS_OUTCOME` and a detail that directs the caller to reconcile by the submitted name — not 503, and not a success. Listing the tenant afterwards either shows an entry whose `name` equals the submitted one (the provision landed; rotate or revoke-and-reprovision it) or does not (a plain retry is safe). Fingerprints `dod-service-accounts-ambiguous-outcome`.
- [ ] A `GET /tenants/{tenant_id}/service-accounts` returns 200 OK with every account for that tenant, each carrying the caller-supplied `name` verbatim alongside its `client_id`, `enabled`, and `scopes`; no response field carries a secret. A tenant with no accounts returns 200 with an empty collection, not 404. Two tenants may each hold an account named `ci`, and each listing shows only its own. Fingerprints `dod-service-accounts-one-time-secret`, `dod-service-accounts-contract-trait-surface`, `dod-service-accounts-no-local-storage`.
- [ ] A `POST …/{client_id}/rotate-secret` returns 200 OK with a new secret under `Cache-Control: no-store` and an unchanged `client_id`; the same call against a `client_id` that does not resolve within the tenant — including one owned by another tenant — returns 404 carrying that client id as the resource. Fingerprints `dod-service-accounts-one-time-secret`, `dod-service-accounts-revoke-idempotency`.
- [ ] A `DELETE …/{client_id}` returns 204 No Content, and a repeated DELETE of the same account also returns 204; a DELETE of an account that never existed likewise returns 204. A clean provider failure on the same endpoint returns 503, not 204. After a revoke the freed name may be provisioned again. Fingerprints `dod-service-accounts-revoke-idempotency`.
- [ ] Every service-account operation routed to an adapter that did not override the contract's service-account methods returns 501 `unimplemented` — never a simulated success. Fingerprints `dod-service-accounts-contract-trait-surface`.
- [ ] All four service-account permission instances (`create`, `list`, `rotate_secret`, `revoke`) are present in the GTS link-time inventory, reference `gts.cf.core.am.service_account.v1~`, and match the action vocabulary the PEP gate actually passes; the resource-type id is registered in the type-schema inventory and is distinct from the `cf.core.security` subject-type id. Fingerprints `dod-service-accounts-independent-permissions`.
- [ ] Under a PDP that grants exactly one of the four actions, that verb reaches its success status and the other three are refused 403 — probed once per verb, so no verb is reachable on another's grant and none is gated by a permission it does not declare. A permissive fake PDP cannot show this, so the probe supplies its own action-aware one. Fingerprints `dod-service-accounts-independent-permissions`.
- [ ] A caller whose PDP grant does not cover the target tenant receives the absent answer without any provider call being issued, and a denied decision, an uncompilable constraint shape, or a PDP evaluation failure all deny. A request against a non-`active` tenant is rejected before the provider call. Fingerprints `dod-service-accounts-authenticated-tenant-scoped-invocation`, `dod-service-accounts-independent-permissions`.
- [ ] A `POST` with an empty / whitespace-only name, an over-cap name, more scopes than the cap, or an over-cap scope string is rejected with `validation` before any provider call. Fingerprints `dod-service-accounts-authenticated-tenant-scoped-invocation`.

## 7. Deliberate Omissions

- **Conforming provider adapters (Keycloak, Zitadel, Dex, …)** — *Delivered in separate crates outside this feature's scope,* exactly as for the user half per `adr-idp-contract-separation`. This feature owns the contract surface and the AM-side service, never a provider translation. Two adapters do ship in this repository, as sibling crates under `account-management/plugins/`: `static-idp-plugin`, the in-memory echo used by dev stacks and the E2E suite, and `keycloak-idp-plugin`, which implements this half against the Keycloak admin API (see its PRD §5.4). Neither is a dependency of this feature — a deployment that registers no adapter, or one whose adapter leaves these methods on their declining defaults, sees every operation as 501.
- **Automatic reconciliation of an ambiguous outcome (idempotency keys, operation keys, retry workers)** — *Out of scope.* AM surfaces the ambiguity as its own category and leaves recovery to the caller, who reconciles by matching the submitted name in the listing (`flow-service-accounts-reconcile`). Correlating an ambiguous provision with its account is in scope; performing the retry on the caller's behalf is not — that would require AM to hold reconciliation state, which `constraint-no-user-storage` forbids.
- **Name syntax, scope-allowlist, and per-tenant quota enforcement** — *Owned by the registered adapter.* AM applies only boundary caps (see `dod-service-accounts-authenticated-tenant-scoped-invocation`); a deployment's `IdP` owns client-id derivation and its own limits, so encoding them in AM would make one of the two authoritative and the other wrong.
- **A by-id read on the item path** — *Not exposed.* The contract has no by-id read, so the URL `create` returns in `Location` answers `DELETE` and prefixes rotate-secret but 405s a `GET`. RFC 9110 §10.2.2 has `Location` *identify* the created resource without promising it is `GET`-able, and nothing is write-only: the collection listing enumerates the tenant's accounts in full. Adding one means changing the trait and every adapter.
- **Retrieval of an existing plaintext secret** — *Impossible by design* per `dod-service-accounts-one-time-secret`. Recovery from a lost secret is a rotation.
- **Long-term credential storage, secret distribution, and workload injection** — *Owned by the caller and the platform secret-management plane.* AM hands the secret over once and keeps nothing.
- **Token issuance, validation, exchange, refresh, and workload-side token acquisition or caching** — *Owned by the `IdP` and the platform AuthN layer.* AM manages the client, not the tokens it can obtain.
- **Account update, enable / disable, filtering, sorting, and pagination** — *Not exposed in v1.* The listing is unpaginated by contract because it is also the ambiguous-outcome reconciliation path; provider quotas bound a tenant to a handful of accounts.
- **Bulk lifecycle operations and multi-provider selection per tenant or request** — *Out of scope.* Exactly one adapter is resolved per deployment, as for every other half of the `IdP` contract.
- **Tenant-offboarding cleanup orchestration** — *Discharged by the adapter* through the existing `deprovision_tenant` hook owned by `feature-tenant-hierarchy-management`. AM performs no account-level offboarding of its own, and cannot detect an adapter that ignores the obligation.
- **A cross-adapter provider-conformance harness** — *Does not exist yet.* Only the trait contract is defined here; nothing in this repository validates that a registered adapter actually enforces the `(tenant_id, name)` uniqueness obligation or invalidates a superseded secret. Qualification work before a second adapter is accepted.
- **Human-user identity, password, session, MFA, and interactive-login management** — *Owned by `cpt-cf-account-management-feature-idp-user-operations-contract`* and the platform AuthN layer. Machine identities are a separate resource type precisely so the two grant surfaces stay separate.
- **AuthZ policy authoring and evaluation** — *Owned by `PolicyEnforcer` / AuthZ Resolver.* This feature declares the resource type and four permissions and enforces the returned decision; it authors no policy.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, metric catalog** — *Owned by `cpt-cf-account-management-feature-errors-observability`.* This feature emits categories by name and defers envelope formatting, status mapping, and metric naming there.
- **AM-side account table, inventory projection, or credential cache** — *Forbidden by `cpt-cf-account-management-constraint-no-user-storage`,* which this feature reads as covering machine identities as well as users.
- **Per-operation metrics** — *Not emitted, and this is a known limitation rather than a considered exclusion.* The user half emits `am.dependency_health` per `IdP` call with the failure category as the outcome label; this surface emits only the `am.events` audit lines and the safe-metadata failure record. The consequence is concrete: an operator watching `am.dependency_health` for provider health sees nothing when machine-identity calls fail, including credential-minting outages. The failure enum already carries `as_metric_label()` for exactly this purpose, so closing the gap is a wiring change in `ServiceAccountService`, not a design one.
- **Operator-tunable limits** — *Deliberately absent.* The AM boundary caps are compile-time constants, unlike the user and tenant listings' `listing.max_top`. The provider owns name charset, scope allowlist, and quota, so making AM's caps configurable would create two places to state one policy and let them disagree.
- **A feature flag gating the endpoints** — *Deliberately absent, and worth stating on a credential-minting surface.* Unlike the opt-in `tr_plugin.enabled`, the four routes register unconditionally whenever the gear does. The only way to withhold machine-identity management from a deployment is to register an adapter that does not implement the contract half, which answers `501`. There is no runtime off switch.
- **A version marker of this feature's own** — *Inherited.* The REST surface versions with AM's `/v1` path prefix and the contract with the SDK, under the breaking-change policy in PRD §7.1; this feature introduces no independent version.
