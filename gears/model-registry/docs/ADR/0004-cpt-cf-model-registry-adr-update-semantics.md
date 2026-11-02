---
description: "Updates replace present top-level fields as a whole on SDK and REST alike; every record carries a version, REST PATCH requires If-Match, and a stale precondition is reported as aborted (409) with the platform's optimistic-lock reason, not 412."
status: accepted
date: 2026-11-03
---

Created:  2026-11-03 by Constructor Tech

# ADR-0004: Whole-Field Updates, Version Preconditions, and 409 for Stale Writes

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [A: present top-level fields replace, absent fields stay, null clears](#a-present-top-level-fields-replace-absent-fields-stay-null-clears)
  - [B: a version on every record, a validator on every REST update](#b-a-version-on-every-record-a-validator-on-every-rest-update)
  - [C: a stale precondition is `aborted`, reason `OPTIMISTIC_LOCK_FAILURE`](#c-a-stale-precondition-is-aborted-reason-optimistic_lock_failure)
  - [Concurrency](#concurrency)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A1: top-level whole-field replacement](#a1-top-level-whole-field-replacement)
  - [A2: RFC 7396 JSON Merge Patch](#a2-rfc-7396-json-merge-patch)
  - [A3: full replacement with `PUT`](#a3-full-replacement-with-put)
  - [A4: field mask](#a4-field-mask)
  - [B1: `version` counter, `If-Match` required on `PATCH`, optional on `DELETE`](#b1-version-counter-if-match-required-on-patch-optional-on-delete)
  - [B2: last writer wins](#b2-last-writer-wins)
  - [B3: optional precondition everywhere](#b3-optional-precondition-everywhere)
  - [B4: generation-bound validator `"<id>.<version>"`](#b4-generation-bound-validator-idversion)
  - [C1: `aborted` (409) through the canonical projection](#c1-aborted-409-through-the-canonical-projection)
  - [C2: map to 412 in the REST layer](#c2-map-to-412-in-the-rest-layer)
  - [C3: add a canonical category for 412](#c3-add-a-canonical-category-for-412)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-update-semantics`

## Context and Problem Statement

Providers and models are edited by administrators through forms and by gears through the SDK. The PRD requires every record to carry a version and every update to be conditional on the version the client last saw, so a stale client cannot overwrite a newer change (`cpt-cf-model-registry-fr-management-api-conventions`). Several fields are security-relevant: `approval_state`, `enabled`, `sharing_mode`, and `secret_ref`. A model record also carries nested objects, `capabilities`, `default_parameters`, and opaque `provider_settings`, whose change must trigger schema re-validation (DESIGN §4 GTS Checks). A provider type migration rewrites the provider and all its models in one step.

Three platform rules apply and pull in different directions:

1. The [REST guideline](../../../../guidelines/DNA/REST/API.md) specifies `PATCH` as RFC 7396 JSON Merge Patch and `ETag` with `If-Match`, with a mismatch reported as 412.
2. The SDK is the primary contract (`cpt-cf-model-registry-principle-sdk-first-contract`); REST and SDK must expose the same operations with the same semantics, and the SDK has no HTTP status codes.
3. Errors are canonical (`docs/arch/errors`): the category list has no 412. `aborted` is defined as "concurrency conflict (optimistic lock, transaction)" and maps to 409, with `OPTIMISTIC_LOCK_FAILURE` as its reason example. CredStore reports a stale `If-Match` as `aborted` 409 with that reason.

What does an update mean, how is a stale write detected, and how is it reported on the SDK and on REST?

## Decision Drivers

* **D1** — SDK and REST updates have one meaning; a request body means the same thing on both.
* **D2** — A stale client cannot revert a security-relevant change, in particular re-approve a revoked model or re-enable a disabled provider.
* **D3** — Re-validation of settings and the credential check run exactly when the value changes.
* **D4** — One error category per condition on both surfaces, projected by `.standard_errors()`; no REST-only status mapping.
* **D5** — Conflicts are detectable and retriable by clients, and the reason code is the one other gears use for the same condition.
* **D6** — Writes are correct under concurrency on PostgreSQL and SQLite with the toolkit's default isolation.

## Considered Options

Update semantics (A):

1. Top-level whole-field replacement
2. RFC 7396 JSON Merge Patch
3. Full replacement with `PUT`
4. Field mask

Precondition (B):

1. `version` counter, `If-Match` required on `PATCH`, optional on `DELETE`
2. Last writer wins
3. Optional precondition everywhere
4. Generation-bound validator `"<id>.<version>"`

Conflict reporting (C):

1. `aborted` (409) through the canonical projection
2. Map to 412 in the REST layer
3. Add a canonical category for 412

## Decision Outcome

Chosen: **A1 + B1 + C1**. A1 is the only update semantics that is identical on the SDK and on REST, and it replaces each field whole, so a change is a comparison of the sent value with the stored one (D1, D3). B1 makes every REST update conditional on the version the client last saw unless the client explicitly asks for an unconditional write; it is the weakest precondition that meets D2 on the surface forms use, and the version check that the write transaction in DESIGN keeps correct under concurrency on both backends (D2, D6). The decision requires amending the PRD requirement to allow the explicit unconditional update. C1 keeps one category per condition on both surfaces and matches CredStore and the platform error catalog (D4, D5).

### A: present top-level fields replace, absent fields stay, null clears

`UpdateProviderRequestV1` and `UpdateModelRequestV1` carry optional fields. A field present in the request replaces the stored value as a whole; this includes the nested objects `capabilities`, `default_parameters`, `provider_settings`, and `metadata`, which are never merged key by key. An absent field is unchanged. An optional field present as `null` is cleared (`base_url`, `secret_ref`, `metadata`); a required field cannot be `null`. Identity fields are rejected with `IMMUTABLE_FIELD` (`cpt-cf-model-registry-constraint-immutable-identity`).

On REST, `PATCH` takes the same body with `Content-Type: application/json`. It is not advertised as `application/merge-patch+json`, because the semantics differ from RFC 7396 below the top level. Presence of `provider_settings` with a value different from the stored one triggers schema validation; presence of `secret_ref` with a non-null value different from the stored one triggers the credential check (ADR-0002); presence of `gts_type` on a provider with a value different from the stored one is a type migration. A present field equal to the stored value is written and triggers neither check. Equality is JSON value equality. The comparison that skipped a check is repeated against the row loaded under the lock in the write transaction; when a field whose check was skipped differs from the locked row, the write fails with `aborted` and reason `OPTIMISTIC_LOCK_FAILURE` (the client re-reads and retries).

### B: a version on every record, a validator on every REST update

`ProviderV1` and `ModelV1` carry `version: i64`, 1 on create and incremented by every successful update, including the increment of every model in a type migration. The SDK update and delete requests carry `expected_version: Option<i64>`; when present, the write applies only if the stored version equals it, otherwise nothing changes. An absent `expected_version` on the SDK is an explicit unconditional write; SDK callers are gears acting on records they just read, not forms.

On REST the version is the strong `ETag` `"<version>"` on `POST`, `GET /{id}`, and `PATCH` responses. `PATCH` requires `If-Match`: a validator maps to `expected_version`, `*` is the unconditional write, and a missing header is rejected as `invalid_argument` with reason `IF_MATCH_REQUIRED`. `DELETE` accepts an optional `If-Match`. The validator is the bare version, not `"<id>.<version>"` as in CredStore: a registry record is addressed by its own UUID, minted at create and never reused, so a validator from a deleted and recreated record cannot reach the new one, which has a new address. CredStore needs the generation because its address is a caller-chosen name.

### C: a stale precondition is `aborted`, reason `OPTIMISTIC_LOCK_FAILURE`

A mismatch between `expected_version` or `If-Match` and the stored version is `CanonicalError::Aborted` with reason `OPTIMISTIC_LOCK_FAILURE`, projected as 409 by `.standard_errors()` on REST. The same category and reason report a type migration whose provider or model set changed between validation and commit. A write that raced a type migration, a model create or a settings update whose provider `gts_type` changed in between, is `aborted` with reason `PROVIDER_TYPE_CHANGED`. The version compare is evaluated first, so a conditional update that raced a type migration reports `OPTIMISTIC_LOCK_FAILURE`, and `PROVIDER_TYPE_CHANGED` is reported for model creates and unconditional updates. In both cases the client re-reads and retries.

This is a recorded deviation from the REST guideline's 412. The canonical error model has no 412 category, and a REST-only mapping would make the same condition a different error on the two surfaces (D4). The reason code follows the platform error catalog's example and CredStore, so a client's conflict handling is the same across gears (D5).

### Concurrency

The version compare and the write run in one transaction under the row locks specified in DESIGN `cpt-cf-model-registry-seq-provider-write` and `cpt-cf-model-registry-seq-model-write`, so two writes with the same expected version apply in commit order and the second fails.

### Consequences

* A client that wants to change one key inside `provider_settings` or `default_parameters` must send the whole object; the management UI reads, edits, and writes the object. The entry it writes from carries the `ETag` it must send back.
* A REST form that posts after a revocation gets 409, re-reads, and sees the revoked state; it cannot re-approve by accident. An SDK caller that omits `expected_version` can, and that is its explicit choice.
* Creates are not idempotent: a retried create after a lost response fails with `already_exists`, and the caller reads the existing entry. No `Idempotency-Key` is supported.
* Every conflict is one category, `aborted`, with two reasons, `OPTIMISTIC_LOCK_FAILURE` and `PROVIDER_TYPE_CHANGED`; both are retriable after a re-read.
* The two deviations from the REST guideline, top-level replacement instead of RFC 7396 and 409 instead of 412, are recorded in DESIGN §4 Deviations with this ADR as their reference.
* Migration obligation: DESIGN uses the reason `OPTIMISTIC_LOCK_FAILURE` in the error table, the write sequences, the REST conventions, the testability bullets, and `ModelRegistryError`, states `Content-Type: application/json` for `PATCH`, adopts the compare-with-stored rule in the write sequences, GTS Checks, and Credential Binding, including the re-compare under the lock in step 4 of `cpt-cf-model-registry-seq-model-write`, and links this ADR in the Key ADRs table, the Deviations rows ("makes 'settings changed' a simple presence check"), and Traceability; the SDK `ModelRegistryError` and reason constants use `OPTIMISTIC_LOCK_FAILURE`; PRD `cpt-cf-model-registry-fr-management-api-conventions` allows an explicit unconditional update (SDK `expected_version` absent, REST `If-Match: *`), and the PRD ADR list links this ADR; ADR-0002's credential check (its update, write, and Confirmation lines that say "sets `secret_ref`") counts only writes that change `secret_ref`.
* Security: stale writes cannot revert approval, enablement, sharing, or credential binding on REST unless the client sends `If-Match: *`. Performance: no extra round trip. Reliability: a lost response on create is recoverable by a read. Data: `version BIGINT NOT NULL` on both tables. Integration: consumers see `aborted` for every conflict. Operations: version mismatches are a metric. Compliance, UX, business: not affected beyond the whole-object edit.

### Confirmation

* Integration (SQLite and PostgreSQL): an update with a stale `expected_version` or `If-Match` fails with `aborted` `OPTIMISTIC_LOCK_FAILURE` and changes nothing, including a stale form that would re-approve a revoked model or re-enable a disabled provider; the response of the successful update carries the incremented `ETag`.
* Integration: `PATCH` without `If-Match` is `invalid_argument` `IF_MATCH_REQUIRED`; `If-Match: *` updates unconditionally; `DELETE` with a stale `If-Match` fails and the row remains.
* Integration: a present nested object replaces the stored one whole, an absent field is unchanged, `null` clears an optional field, a required field with `null` and an identity field are `invalid_argument`.
* Integration: `provider_settings` is validated against the schema and `secret_ref` runs the credential check when the sent value differs from the stored one, and neither runs when the field is absent or equal to the stored value.
* Integration (PostgreSQL): two concurrent updates with the same `expected_version` yield one success and one `OPTIMISTIC_LOCK_FAILURE`; a conditional settings update concurrent with a type migration fails with `OPTIMISTIC_LOCK_FAILURE`; an unconditional one or a model create fails with `PROVIDER_TYPE_CHANGED`; never both applied.
* Integration: an unconditional update that resends a stored `provider_settings` or `secret_ref` while a concurrent write changes that field fails with `OPTIMISTIC_LOCK_FAILURE` and writes nothing.
* Integration: a type migration increments the provider version and every model version.
* Contract: the REST error for a stale validator is 409 with the `aborted` problem type and reason `OPTIMISTIC_LOCK_FAILURE`; the SDK error is `Aborted` with the same reason.

## Pros and Cons of the Options

### A1: top-level whole-field replacement

* Good, because the SDK request type and the REST body are one shape with one meaning.
* Good, because presence selects the fields to write, and a comparison of each present field with the stored row decides whether re-validation and the credential check run.
* Bad, because editing one key inside a nested object requires sending the object.

### A2: RFC 7396 JSON Merge Patch

* Good, because it follows the REST guideline and lets a client change one nested key.
* Bad, because a merge inside `provider_settings` is a merge inside an opaque document the gear does not understand; `null` would delete keys the schema may require, and the SDK would need a merge-patch request type of its own.
* Bad, because partial settings changes cannot be validated without first merging, and the SDK and REST would differ unless the SDK adopted the same recursive semantics.

### A3: full replacement with `PUT`

* Good, because the request is unambiguous.
* Bad, because every change resends the full record, including fields the caller did not read, and the approval and sharing fields travel with every edit.

### A4: field mask

* Good, because it is explicit about what changes and works with nested paths.
* Bad, because it adds a second language (the mask) to every update on both surfaces, with no precedent in the platform.

### B1: `version` counter, `If-Match` required on `PATCH`, optional on `DELETE`

* Good, because it meets D2 on the surface forms use, with one integer.
* Good, because the SDK keeps an explicit unconditional write for gears.
* Bad, because REST clients must round-trip the `ETag`, and an SDK caller can still write unconditionally.
* Bad, because `If-Match: *` lets a REST client opt out explicitly, and `DELETE` does not require a precondition; a delete carries no field values that could revert a newer change. Adopting it requires amending the PRD requirement to allow an explicit unconditional update.

### B2: last writer wins

* Good, because it is simplest.
* Bad, because it violates the PRD, and a stale form can revert a revocation.

### B3: optional precondition everywhere

* Good, because REST clients that do not care can omit the header.
* Bad, because the clients that most need it, forms, are the ones that would omit it; D2 is not met.

### B4: generation-bound validator `"<id>.<version>"`

* Good, because it matches CredStore's validator shape.
* Bad, because it solves an ABA problem the registry does not have: the address already is the generation.

### C1: `aborted` (409) through the canonical projection

* Good, because the category exists, is defined for exactly this condition, and projects identically on SDK and REST.
* Good, because CredStore reports the same condition the same way.
* Bad, because it deviates from the REST guideline's 412.

### C2: map to 412 in the REST layer

* Good, because REST clients would see the guideline's status.
* Bad, because the same error has two statuses depending on the surface, and the mapping sits outside `.standard_errors()`, which every other route uses.

### C3: add a canonical category for 412

* Good, because it would let every gear follow the guideline.
* Bad, because it is a platform change to the error catalog and every projection; not this gear's decision.

## More Information

Revisit triggers:

* The platform adds a `failed_precondition`-style category projected as 412, or changes the `aborted` projection.
* A consumer needs partial edits inside `provider_settings` or `default_parameters`; A2 or A4 is the option to re-examine.
* A REST client class that cannot round-trip `ETag` appears.
* Idempotent creates are required; `Idempotency-Key` is the guideline's mechanism.

Related: [CredStore ADR-0007](../../../credstore/docs/ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md) (`PUT` replaces, `PATCH` merges, on a name-addressed record), CredStore DESIGN §4.10 (generation-bound validator), platform error category [`aborted`](../../../../docs/arch/errors/categories/10-aborted.md), [REST guideline](../../../../guidelines/DNA/REST/API.md). Platform rules departed from: RFC 7396 `PATCH` and 412 on `If-Match` mismatch.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-model-registry-fr-management-api-conventions` — every record carries `version`; REST `PATCH` is conditional on it; errors are canonical.
* `cpt-cf-model-registry-fr-provider-management` — provider updates replace present fields; type migration bumps every version.
* `cpt-cf-model-registry-fr-model-management` — model updates replace present fields, including nested settings, and trigger re-validation when a sent value differs from the stored one.
* `cpt-cf-model-registry-fr-model-approval` — a stale write cannot revert an approval change.
* `cpt-cf-model-registry-constraint-immutable-identity` — identity fields are rejected on update.
* `cpt-cf-model-registry-seq-provider-write` — the provider transaction, lock, version compare, and type migration conflict.
* `cpt-cf-model-registry-seq-model-write` — the model transaction, lock, version compare, and `PROVIDER_TYPE_CHANGED`.
