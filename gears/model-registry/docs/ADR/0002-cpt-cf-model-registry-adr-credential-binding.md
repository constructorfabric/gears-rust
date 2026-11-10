---
description: "A model's secret reference must resolve, for the writing tenant, to a credential that tenant owns; the check is a metadata-only CredStore read at write time under the caller's context, is not repeated at evaluation, and the fallback to an ancestor's shared secret after the owner removes its own is an accepted residual that the owner closes by suppressing the reference in CredStore."
status: accepted
date: 2026-11-02
---

Created:  2026-11-02 by Constructor Tech

# ADR-0002: Secret References Bind to a Credential the Writing Tenant Owns

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The check is a metadata read of the record under the caller's context](#the-check-is-a-metadata-read-of-the-record-under-the-callers-context)
  - [Ownership is the only condition](#ownership-is-the-only-condition)
  - [The check runs once, at write time](#the-check-runs-once-at-write-time)
  - [Evaluation does not verify the owner](#evaluation-does-not-verify-the-owner)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Write-time ownership check by metadata read, residual fallback accepted](#write-time-ownership-check-by-metadata-read-residual-fallback-accepted)
  - [No check](#no-check)
  - [Write-time check plus an owner comparison in LLM Gateway at evaluation](#write-time-check-plus-an-owner-comparison-in-llm-gateway-at-evaluation)
  - [Re-check on every resolution](#re-check-on-every-resolution)
  - [Check under the gear's service identity](#check-under-the-gears-service-identity)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-credential-binding`

## Context and Problem Statement

A model record carries an optional `secret_ref`, the name of a CredStore credential that LLM Gateway loads at evaluation time to call the provider (`cpt-cf-model-registry-fr-model-management`). The registry stores the name only, never the value (`cpt-cf-model-registry-constraint-no-secret-storage`). An inheriting tenant sees the full inherited record, `secret_ref` included (`cpt-cf-model-registry-fr-effective-catalog`).

A `SecretRef` is a per-tenant name, not an identity. CredStore resolves it hierarchically for the calling tenant: the tenant's own row wins, otherwise the nearest ancestor's `shared` record ([CredStore DESIGN](../../../credstore/docs/DESIGN.md), `cpt-cf-credstore-fr-get-credential`). Without a rule, a descendant administrator can copy an ancestor's `secret_ref` from an inherited model into a model of its own, by mistake or by copy-paste, and the registry silently attaches the ancestor's credential to a model with its own `base_url`. The PRD closes this write-time route: a `secret_ref` **MUST** name a credential the writing tenant owns, and an inherited or unknown reference **MUST** be rejected with a validation error (`cpt-cf-model-registry-fr-model-management`). The rule is not a boundary against a descendant that deliberately sets up the fallback; that boundary is CredStore sharing and the `read_secret` action (D6).

Two facts of the CredStore contract shape how the rule can be enforced:

1. `CredStoreClientV2::get_record` is a hierarchical metadata read that never returns the value and needs the `read` action only. Its `Credential` carries `inheritance` (`own`, `inherited`, `overridden`, `suppressed`) and the caller's own row, when one exists, as `own: Option<OwnRow>` with its `status` (`declared`, `active`, `expired`).
2. [CredStore ADR-0009](../../../credstore/docs/ADR/0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md) withdrew `owner_tenant_id` and `is_inherited` from every response: a caller learns nothing about the ancestor that serves an inherited secret, and `get_secret` returns the value with its usage envelope and nothing administrative.

How does the registry enforce the ownership rule, under whose authority, and what happens when the binding goes stale because the owner later removes its own secret and resolution falls through to an ancestor's `shared` record?

## Decision Drivers

* **D1** — The registry never holds a secret value, not even transiently (`cpt-cf-model-registry-constraint-no-secret-storage`).
* **D2** — The check uses the published CredStore contract and does not require CredStore to disclose what ADR-0009 withholds.
* **D3** — The caller's own CredStore grant decides what the caller may bind; the registry gains no CredStore privilege of its own.
* **D4** — Nothing is added to the resolution path (`cpt-cf-model-registry-nfr-resolution-latency`).
* **D5** — A rejected write leaves no row and gives the caller a field-level reason.
* **D6** — The rule is a defense in depth, not the tenant boundary: the boundary for secret values is CredStore sharing and the `read_secret` action, and this rule must not be mistaken for it.

## Considered Options

1. Write-time ownership check by metadata read, residual fallback accepted
2. No check
3. Write-time check plus an owner comparison in LLM Gateway at evaluation
4. Re-check on every resolution
5. Check under the gear's service identity

## Decision Outcome

Chosen option: "Write-time ownership check by metadata read, residual fallback accepted", because it enforces the PRD rule with the CredStore contract as published, keeps secret values out of the registry, adds nothing to resolution, and is honest about what it protects (D1, D2, D3, D4, D6).

### The check is a metadata read of the record under the caller's context

On model create with `secret_ref`, and on an update whose `secret_ref` is a non-null value different from the stored one, the catalog service calls `CredStoreClientV2::get_record(ctx, &secret_ref)` with the caller's `SecurityContext`, bounded by `credstore_deadline_ms`. The call needs the CredStore `read` action on the reference, not `read_secret`; no value is returned, so none is handled, logged, or dropped. Clearing `secret_ref` (`null`) or resending the stored value makes no CredStore call.

A caller without the `read` grant is refused with `NOT_PERMITTED` and an operator-visible log diagnostic naming the missing grant. CredStore unavailable, an expired deadline, or an internal failure is dependency unavailable (`service_unavailable`), and the write is not performed.

### Ownership is the only condition

The reference is accepted when the response is `Ok(Some(credential))` and the caller's own row holds a secret: `own` is present with `status` `active` or `expired`. In that case `inheritance` is `own` or `overridden`. Otherwise the write is rejected with a field violation on `secret_ref`:

| CredStore result | Reason |
|------------------|--------|
| `Ok(None)` | `SECRET_REF_NOT_FOUND` |
| `Ok(Some)` with no `own` row, or an `own` row with `status` `declared` | `SECRET_REF_NOT_OWNED` |
| `PermissionDenied` | `NOT_PERMITTED` (not a field violation) |
| any other error | dependency unavailable |

An `expired` own secret is accepted: the binding is about ownership, and rotation keeps the reference. Sharing mode of the secret is not checked: a `private` secret resolves at evaluation only for its creating subject, and a `tenant` secret is invisible to a descendant, whose resolution walks past it to a same-named `shared` secret of a tenant above the owner when one exists and otherwise fails with `credentials_unavailable` in LLM Gateway. That fallback while the owner's secret still exists is accepted on the same grounds as the stale-binding residual below: the ancestor's `shared` secret is one the descendant can already read under CredStore's rules. Configuring CredStore inheritance, or choosing a unique reference to stay out of it, is the tenant administrator's responsibility. The registry does not predict the outcome.

### The check runs once, at write time

The check runs before the write transaction and holds no database connection. It is not repeated on `resolve_model`, `list_available_models`, or any read, and not on an update that omits `secret_ref` or resends the stored value. A provider type migration (`update_provider` with a new `gts_type`) does not re-check model secret references.

### Evaluation does not verify the owner

The stored binding goes stale when the owner removes its own secret and leaves `fallback: inherit` on the record, or deletes the record: resolution then serves the nearest ancestor's `shared` secret under the same name. This is accepted as a residual, not closed by LLM Gateway, for two reasons:

1. It cannot be closed there under the CredStore contract. `get_secret` names no owner (ADR-0009), and under the evaluating caller's context an inherited model legitimately resolves to a secret the caller does not own, so no owner comparison distinguishes the stale case from the intended one.
2. It is not a wider route to a value. The fallback reaches only an ancestor secret that is `shared` to the owner's tenant, one its administrators may already read and use under CredStore's rules; the model is a second route to a value CredStore already exposes to that tenant. The writing tenant can also reach the stale state itself: bind its own secret, then remove it with `fallback: inherit`. This is why the rule is defense in depth, not a boundary. The ancestor's control is CredStore sharing (stop sharing the secret) and the `read_secret` grant.

The owner closes the residual on the CredStore side when it wants to, by suppressing the reference ([CredStore ADR-0008](../../../credstore/docs/ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md), `cpt-cf-credstore-fr-suppression`): while its record is `active`, by removing the secret as `PATCH {"secret": null, "fallback": "none"}` under `If-Match`; after removing the secret with `fallback: inherit`, which leaves a `declared` row, by `PATCH {"fallback": "none"}` under `If-Match`; after deleting the record, by `PUT {"type": …, "sharing": …, "fallback": "none", "secret": null}` with `If-None-Match: *`, since `PATCH` never creates a record. With `sharing: shared` on the suppressing record, the owner and its descendants are covered; with `sharing: tenant`, the owner tenant only. Suppression blocks the ancestor fallback only: a descendant whose own active secret has the same reference keeps evaluating with that secret. `resolve_model` keeps returning the model; where the suppression applies, evaluation fails with `credentials_unavailable`.

### Consequences

* Model writes that set a new `secret_ref` take one CredStore round trip and depend on CredStore availability; other writes and all reads do not.
* The writing administrator needs the CredStore `read` grant on the reference. No new PDP action is added to this gear.
* The registry holds the invariant "a model's credential belongs to the model's owner" for the catalog it manages; it cannot hold it against later CredStore changes.
* A `shared` model whose secret is not `shared` in CredStore is accepted. For a descendant, evaluation uses whatever secret CredStore resolves under the reference for that descendant, its own or an ancestor's `shared` one, and fails with `credentials_unavailable` when there is none. The two sharing settings are independent and owned by their gears.
* The open question "CredStore metadata-only read" is resolved: `get_record` is that read. The SDK that ships today (`CredStoreClientV1::get`, which returns the value with `owner_tenant_id` and `is_inherited`) is not the published contract; implementing against it would put the value in this gear's memory and rely on fields ADR-0009 withdraws.
* The LLM Gateway DESIGN follow-up (DESIGN §3.3 Contract Migration) carries no owner check; the Credential Broker's existing behavior on a missing or failed secret is sufficient.
* Security: the check prevents an accidental or copy-paste binding to an inherited reference, including a `tenant`-mode ancestor credential or a `shared` one the writer could not read. A deliberate fallback by the owner's tenant is not prevented and is governed by CredStore sharing and `read_secret`; the value boundary stays with CredStore (D6). Performance: unchanged on reads. Reliability: a CredStore outage blocks writes that set a new `secret_ref` only. Data: none; the `secret_ref` column is unchanged. Integration: the gear depends on `CredStoreClientV2`. Operations: a rejected reference is visible as a `secret_ref` field violation and in the rejection metric by reason. Compliance, UX, business: not affected.

### Confirmation

* Integration: a reference the tenant owns (`own.status` `active`) is accepted; `expired` is accepted; an own row with `status` `declared` and an entry with no own row are rejected with `SECRET_REF_NOT_OWNED`; `Ok(None)` is rejected with `SECRET_REF_NOT_FOUND`; `PermissionDenied` yields `NOT_PERMITTED`; an error or a delay past `credstore_deadline_ms` yields `service_unavailable`. No row is written in any rejected case.
* Integration: the mock CredStore records that only `get_record` is called, under the caller's context, once per create with `secret_ref` and per update that changes it to a non-null value, and never on resolution, on lists, on an update that omits `secret_ref` or resends the stored value, or on a `null`.
* Unit: no CredStore type carrying a secret value is reachable from the catalog service; `get_secret` is not called anywhere in the gear.
* Review: the registry's dependency declaration names CredStore for writes only.

## Pros and Cons of the Options

### Write-time ownership check by metadata read, residual fallback accepted

`get_record` under the caller's context; accept only an own row holding a secret; nothing at evaluation.

* Good, because it enforces the PRD rule with the published contract and no value in the gear.
* Good, because the caller's own CredStore grant gates what it may bind.
* Good, because resolution is untouched.
* Bad, because the binding can go stale, and the owner's tenant can create that state itself; closing it is the owner's operational choice (suppression, covering descendants only with `sharing: shared`), not a registry guarantee.
* Bad, because the check is only as strong as the name: a later `shared` secret under the same name in an ancestor is not detected.

### No check

Store any `secret_ref`; rely on CredStore sharing and `read_secret` at evaluation.

* Good, because the registry has no CredStore dependency.
* Bad, because it violates the PRD rule, and a descendant can configure a model that spends an ancestor's `shared` credential under the descendant's `base_url` while only ever holding a name.

### Write-time check plus an owner comparison in LLM Gateway at evaluation

LLM Gateway compares the loaded secret's owner with the model's `tenant_id` and refuses on mismatch.

* Good, because it would detect the stale binding at the moment it matters.
* Bad, because the owner is not available: `get_secret` carries no `owner_tenant_id` (ADR-0009), and asking CredStore to restore it reopens the ancestor disclosure that ADR decided against.
* Bad, because the comparison cannot be made under the caller's context for inherited models: a descendant caller legitimately loads a secret owned by the model's tenant, not its own, and the resolved owner is unknowable to it.

### Re-check on every resolution

The registry calls CredStore on `resolve_model` to confirm the binding.

* Good, because the binding would be re-validated continuously.
* Bad, because it adds a CredStore round trip to every resolution against a 50 ms gateway budget, and `list_available_models` would need one per row.
* Bad, because under the resolving caller's context the check has the same blind spot as the previous option for inherited models.

### Check under the gear's service identity

Run `get_record` under a Model Registry service context with an explicit tenant, instead of the caller's.

* Good, because the writing administrator would not need a CredStore `read` grant.
* Bad, because the gear would need CredStore read rights over every tenant's credentials, a privilege it otherwise has no use for; a defect in the gear would then read any tenant's metadata.
* Bad, because the caller could bind a reference it may not read itself, a confused-deputy path the caller-context check does not have.

## More Information

Revisit triggers:

* CredStore offers a tenant-stable credential identity usable across callers, or a way for a consumer to pin a reference to the owner's row (a binding token) that evaluation can present.
* A product need for the registry to enforce that a `shared` model's secret is `shared` in CredStore.
* LLM Gateway loads credentials under an identity other than the evaluating caller's.
* CredStore changes the `inheritance` or `own` semantics of `get_record`.

Related: [CredStore ADR-0004](../../../credstore/docs/ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md) (one record, `secret` only under `$select`, the basis for a metadata-only read), [CredStore ADR-0008](../../../credstore/docs/ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md) (suppression), [CredStore ADR-0009](../../../credstore/docs/ADR/0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md) (no owner in responses). The LLM Gateway side of the chain is `cpt-cf-llm-gateway-component-credential-broker-v1` and `cpt-cf-llm-gateway-constraint-key-destination-binding-v1` in the [LLM Gateway DESIGN](../../../llm-gateway/docs/DESIGN.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-model-registry-fr-model-management` — a `secret_ref` is accepted only when the writing tenant's own CredStore row holds a secret; otherwise a field violation.
* `cpt-cf-model-registry-fr-effective-catalog` — inherited records keep showing `secret_ref`; the name alone cannot be bound by an inheriting tenant.
* `cpt-cf-model-registry-fr-tenant-isolation` — the registry is not a route by which a tenant spends another tenant's credential.
* `cpt-cf-model-registry-constraint-no-secret-storage` — the check is a metadata read; no value enters the gear.
* `cpt-cf-model-registry-nfr-resolution-latency` — no CredStore call on resolution or lists.
* `cpt-cf-model-registry-component-catalog-service` — owns the check, before the write transaction.
