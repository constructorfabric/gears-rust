<!-- Created: 2026-08-26 by Constructor Tech -->

# Feature: Metric Types & Subject Admission

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-featstatus-projection-contracts-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-quota-enforcement-feature-projection-contracts`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Owner Metric Type Publication](#owner-metric-type-publication)
  - [Evaluation Request Ingress Validation](#evaluation-request-ingress-validation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Catalogue Read and Checks](#catalogue-read-and-checks)
  - [Subject Admission](#subject-admission)
  - [Admission Check for Quota and Policy Writes](#admission-check-for-quota-and-policy-writes)
- [4. States (CDSL)](#4-states-cdsl)
  - [MetricCatalog State Machine](#metriccatalog-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Resolver-Owned Base Types](#resolver-owned-base-types)
  - [Metric Catalogue](#metric-catalogue)
  - [Gateway Ingress Validation](#gateway-ingress-validation)
  - [Subject Admission at Ingress](#subject-admission-at-ingress)
  - [Admission Check for Writes](#admission-check-for-writes)
  - [Contract-Validation Telemetry](#contract-validation-telemetry)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context (optional)](#7-additional-context-optional)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

A **metric type** is a GTS type derived from the quota resolver's metric base `gts.cf.core.quota.metric.v1~`
([ADR-0013](../ADR/0013-cpt-cf-quota-enforcement-adr-metric-types.md)). The module that owns a metric registers its
type in `types-registry`. The type has three traits: `kind` (`counter` or `gauge`), `admitted_subjects` (the subject
types the metric's quotas may count), and `enforcement` (`quota_gated` or `direct`, default `quota_gated`). Its schema
refines the `metadata` of the metric object.

A **metric object** is how a request names its metric: `metric: {type, id?, metadata}`. `type` is the metric type.
`id` is optional and names the one thing the amount is counted for, for example a model or a virtual machine.
`metadata` holds the attributes of the operation that policies read.

This feature reads metric types into `MetricCatalog` while QE runs, validates the public shape of every evaluation
request at Gateway ingress, admits the request through the admission mode of its path, validates the metric object
against its metric type, and checks each named subject against the metric's admitted subject types. The whole
evaluation pipeline trusts this validation.

`MetricCatalog` is the process-local catalogue of the metric types that QE has read
([ADR-0012](../ADR/0012-cpt-cf-quota-enforcement-adr-contract-catalogue.md)). A **catalogue entry** holds, for one
metric type, the resolved schema and the traits. An entry is **served**, and QE evaluates requests for its metric, or
**unavailable**, when the type fails a check: requests for that metric then fail with canonical `FailedPrecondition`,
other metrics are served, and bootstrap does not fail. QE reads a metric type from `types-registry` by the id that a
request, a Quota, or a Policy names.

### 1.2 Purpose

Without an enforced metric type, a Policy expression can silently miss because a request key is absent, misspelled, or
of the wrong type, and a quota can count a kind of subject its metric was never meant to count. This feature turns
those failures into save-time or ingress-time errors, or into an unavailable metric: the metric owner registers one
metric type with its admitted subject types, the caller names subjects as quota subjects, QE checks the public shape,
the admission mode authorizes the request, and QE checks admission and validates the metric object against the local
catalogue ([ADR-0008](../ADR/0008-cpt-cf-quota-enforcement-adr-quota-resolver-subjects.md)). It also defines the
subject set that the consumption-operations feature consumes inside its `EvaluationOrchestrator` pipeline.

A **quota subject** is `{type, id}`: `type` is a GTS type id derived from the quota resolver's subject base
`gts.cf.core.quota.subj.v1~`, `id` an opaque non-empty string. The quota resolver SDK owns that base, the final tenant
subject type `gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~` (its id is the tenant id), and the metric base. QE
owns no GTS type for subjects or metrics; the period type `gts.cf.core.qe.period_type.v1~` and the quota type
`gts.cf.core.qe.quota_type.v1~`, with their instances, stay QE's own.

**Scope**: `MetricCatalog` reads with the checks of each metric type; admission checks on Quota and Policy writes;
ingress validation of the subjects and of the metric object; contract-validation telemetry counters.

**Out of scope**: Quota records themselves (quota-lifecycle feature), Quota metadata and its check by the Engine
(quota-lifecycle and resolution-policy-engine features), Engine consumption of request and arbitration data
(resolution-policy-engine feature), the evaluation pipeline that consumes the subject set (consumption-operations
feature), and the trusted admission mode itself (foundation feature).

**Requirements**: `cpt-cf-quota-enforcement-fr-projection-contracts`,
`cpt-cf-quota-enforcement-fr-contract-validation`, `cpt-cf-quota-enforcement-fr-subject-type-registry`,
`cpt-cf-quota-enforcement-fr-subject-resolution`

**Principles**: `cpt-cf-quota-enforcement-principle-declarative-projection-contracts`,
`cpt-cf-quota-enforcement-principle-pdp-authorized-attribution`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-quota-enforcement-actor-metric-owner` | Registers one metric type per metric in `types-registry`, with its traits and the schema of its `metadata` |
| `cpt-cf-quota-enforcement-actor-quota-consumer` | Sends evaluation requests through QE's API carrying `tenant_id`, quota subjects, and a conforming metric object |
| `cpt-cf-quota-enforcement-actor-quota-resolver` | Sends the same requests through the resolver plugin, admitted in trusted mode |
| `cpt-cf-quota-enforcement-actor-types-registry` | Authoritative store of the bases, metric types, and subject types; answers QE's reads by type id |
| `cpt-cf-quota-enforcement-actor-platform-operator` | Registers the subject types of principals; learns of an unavailable metric from telemetry |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) (§5.1 metric types, request validation, subject registry, subject attribution)
- **Design**: [DESIGN.md](../DESIGN.md) (Gateway, EvaluationOrchestrator subject resolution, `MetricCatalog`)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) (§2.2)
- **ADR**: [ADR-0013 Metric types](../ADR/0013-cpt-cf-quota-enforcement-adr-metric-types.md)
  (`cpt-cf-quota-enforcement-adr-metric-types`; the metric type, the metric object, Quota metadata), [ADR-0012 Metric
  catalogue, filled while running](../ADR/0012-cpt-cf-quota-enforcement-adr-contract-catalogue.md)
  (`cpt-cf-quota-enforcement-adr-contract-catalogue`; catalogue reads, served and unavailable entries), [ADR-0008
  Quota subjects are quota resolver subject
  types](../ADR/0008-cpt-cf-quota-enforcement-adr-quota-resolver-subjects.md)
  (`cpt-cf-quota-enforcement-adr-quota-resolver-subjects`; subjects, admission, resolver-owned types), [ADR-0007
  Declarative GTS projection contracts](../ADR/0007-cpt-cf-quota-enforcement-adr-projection-contracts.md)
  (`cpt-cf-quota-enforcement-adr-projection-contracts`; validation of a whole document against the resolved schema of
  its type, evaluation against the process-local catalogue), [quota resolver ADR-0004 Metric
  types](../../../quota-resolver/docs/ADR/0004-cpt-cf-quota-resolver-adr-metric-types.md)
  (`cpt-cf-quota-resolver-adr-metric-types`; the metric base and its traits)
- **Dependencies**: `cpt-cf-quota-enforcement-feature-foundation` (bootstrap hook, Gateway admission modes, telemetry
  conventions)

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-quota-enforcement-usecase-debit` (ingress-validation prefix only; the debit body is owned by the
consumption-operations feature), `cpt-cf-quota-enforcement-usecase-create-quota` (admission step only; the Quota write
itself is owned by the quota-lifecycle feature)

### Owner Metric Type Publication

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-owner-projection-publication`

**Actor**: `cpt-cf-quota-enforcement-actor-metric-owner`

**Success Scenarios**:
- The owner's metric type is registered, read and checked by QE with no restart, and served from the catalogue; every
  caller charges the same quotas through it

**Error Scenarios**:
- The metric type is abstract, is not derived from the metric base, has an own version of major 0, or has a wrong
  trait: the metric is unavailable; bootstrap does not fail
- The metric type is not registered: it has no entry, and its requests are `InvalidArgument`

**Steps**:
1. [ ] - `p1` - The module that charges a kind of subject, or the operator for principals, registers a quota subject type derived from `gts.cf.core.quota.subj.v1~` - `inst-pub-author`
2. [ ] - `p1` - Owner registers one concrete metric type per metric, derived from `gts.cf.core.quota.metric.v1~` and named under the owning module; its traits set `kind`, `enforcement`, and `admitted_subjects` (default empty: tenant quotas only; an owner that does not know the metric's kinds of subject admits the subject base `gts.cf.core.quota.subj.v1~`, which admits every subject type); its schema refines `metadata` - `inst-pub-attrs`
3. [x] - `p1` - API: registration happens in `types-registry`; QE exposes no registration endpoint (`cpt-cf-quota-enforcement-constraint-types-registry-delegation`) - `inst-pub-registry`
4. [ ] - `p1` - QE reads and checks the metric type by its id (`cpt-cf-quota-enforcement-algo-catalog-bootstrap`): at start when an active Quota or Policy names it, otherwise when a request or a Quota or Policy write names it for the first time, and again on a bounded schedule; QE serves the metric from its `MetricCatalog` entry - `inst-pub-boot`
5. [ ] - `p1` - **RETURN** any caller names quota subjects of admitted types and the metric type; a Quota is bound to a subject and a metric type, so every caller charges one shared set of quotas; another major version of the metric type is another metric with its own Quotas - `inst-pub-return`

### Evaluation Request Ingress Validation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-ingress-validation`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer`

**Success Scenarios**:
- A conforming request passes public shape checks, is admitted, passes the admission check of every subject, and
  proceeds with one validated metric object

**Error Scenarios**:
- Missing required fields, wrong public container types, no subject, an empty id, two subjects of one type, or a
  tenant subject whose id is not `tenant_id`: canonical `InvalidArgument` before admission
- PDP denies a structurally valid request on QE's API: canonical `PermissionDenied` before any catalogue lookup
- Admitted request with a subject type that is not registered or that the metric does not admit, a metric object that
  does not fit its metric type, or a metric type that is not registered: canonical `InvalidArgument` with a stable
  field-level reason, never `Decision::Denied`
- Admitted request with a subject type whose registry lookup fails: canonical `ServiceUnavailable`
- Admitted request for a metric whose catalogue entry is unavailable: canonical `FailedPrecondition`; nothing changes
  and no Decision is produced
- Admitted request for a metric the catalogue does not hold, when the registry read of the metric type fails or
  reaches its time limit: canonical `ServiceUnavailable`

**Steps**:
1. [ ] - `p1` - Caller sends an evaluation operation carrying `tenant_id`, `subjects: [{type, id}]` (at least one), and `metric: {type, id?, metadata}`; platform authentication has attached the caller's `SecurityContext` and the DTO is deserialized - `inst-ing-request`
2. [ ] - `p1` - Validate public request shape: required fields and container types, non-empty `tenant_id` and subject ids, at least one subject, no two subjects of one type, a subject of the tenant type only with `id == tenant_id`, and a metric object with `type` and `metadata`; return canonical `InvalidArgument` before admission on failure - `inst-ing-shape`
3. [ ] - `p1` - Admit the request in the mode of its path (`cpt-cf-quota-enforcement-flow-authorized-admission`): on QE's API, send the complete attribution (the tenant, the subjects, and the metric with its type and optional id) to PDP and fail closed on denial or PDP unavailability; on the resolver plugin path, trusted mode calls no PDP and scopes storage to `tenant_id` - `inst-ing-authz`
4. [ ] - `p1` - Look up the entry of `metric.type` in the process-local `MetricCatalog`; a metric the catalogue holds needs no registry call; for a metric it does not hold, read the metric type by its id once, with a time limit, and build its entry (`cpt-cf-quota-enforcement-algo-catalog-bootstrap`) - `inst-ing-lookup`
5. [ ] - `p1` - **IF** the entry is unavailable - `inst-ing-unavail-if`
   1. [ ] - `p1` - **RETURN** canonical `FailedPrecondition`; nothing changes and no Decision is produced - `inst-ing-unavail`
6. [ ] - `p1` - **IF** the read of a metric type the catalogue does not hold fails or reaches its time limit - `inst-ing-registry-if`
   1. [ ] - `p1` - **RETURN** canonical `ServiceUnavailable`; the caller may repeat the request - `inst-ing-registry`
7. [ ] - `p1` - **IF** the metric type is not registered (an answer the catalogue remembers for a short time), or a subject type is not admitted (`cpt-cf-quota-enforcement-algo-subject-resolution`) - `inst-ing-metric-if`
   1. [ ] - `p1` - **RETURN** canonical `InvalidArgument`; increment `admitted_metric_violations_total` by closed validation surface - `inst-ing-metric`
8. [ ] - `p1` - Validate the whole metric object `{type, id?, metadata}` against the resolved schema of its metric type (never the `metadata` subschema alone); absent `metadata` was rejected before admission and is never defaulted to `{}`; a metric object that does not fit is canonical `InvalidArgument` - `inst-ing-metadata`
9. [ ] - `p1` - Add the implicit tenant subject from `tenant_id` unless the request names it, and exclude the caller's identity from Policy input `{request, arbitration}`, where `request` is the `metadata` of the metric object and `arbitration` is the metadata of a Quota - `inst-ing-map`
10. [x] - `p1` - **RETURN** the validated, admitted request to the evaluation pipeline; this validation runs at ingress of every debit, lease acquisition, preview, and each batch item, and its failures are canonical errors, never `Decision::Denied` (`cpt-cf-quota-enforcement-fr-contract-validation`) - `inst-ing-forward`

## 3. Processes / Business Logic (CDSL)

### Catalogue Read and Checks

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-catalog-bootstrap`

**Input**: the `types-registry` client, the current `MetricCatalog`, and, at start only, the metric types that the stored active Quotas and Policies name

**Output**: `MetricCatalog` with a served or unavailable entry for each metric type that QE has read

**Steps**:
1. [ ] - `p1` - API: at start, read by id from `types-registry` the metric types that the active Quotas and the active Policies name; `types-registry` remains authoritative and the catalogue is only a checked local copy; no catalogue read changes readiness: a type whose read at start fails or reaches its time limit gets no entry and is read when a request or a write names it - `inst-cat-start`
2. [ ] - `p1` - API: when a request or a Quota or Policy write names a metric type the catalogue does not hold, check the form of the id first (an id that does not start with `gts.cf.core.quota.metric.v1~` is canonical `InvalidArgument` with no registry call), then read that type by its id, once and with a time limit, through `PlatformTypesRegistryApi`, asking the registry itself and not the client's cached copy; a type with the deleted status is treated as deleted; the memory of unknown ids has a size limit and drops the entry that was used least recently, and the catalogue has none, because it holds only registered types - `inst-cat-resolve`
3. [ ] - `p1` - **IF** the registry does not know the type - `inst-cat-unknown-if`
   1. [ ] - `p1` - **RETURN** canonical `InvalidArgument` and remember the answer for a short time, so repeated requests for an unknown metric do not call the registry each time; no entry is built - `inst-cat-unknown`
4. [ ] - `p1` - **IF** the read fails or reaches its time limit - `inst-cat-read-failed-if`
   1. [ ] - `p1` - **RETURN** canonical `ServiceUnavailable`; no entry is built and nothing is remembered - `inst-cat-read-failed`
5. [ ] - `p1` - Check the type: it is concrete; it is derived from `gts.cf.core.quota.metric.v1~`; its own version is major 1 or higher, because `types-registry` admits any change of a type of major 0 (ADR-0012); its traits are valid against the traits schema of the metric base; each `admitted_subjects` entry is `gts.cf.core.quota.subj.v1~` or a type id derived from it (whether a subject type is registered is checked when a request names it, `inst-res-registered`) - `inst-cat-concrete`
6. [ ] - `p1` - Make no check against stored Quotas or Policies: a type that stops admitting the subject type of a stored Quota, or that an active Policy does not fit, stays served; the first is found when a request names the subject type, the second at evaluation (ADR-0012) - `inst-cat-compat`
7. [ ] - `p1` - **IF** the checks of step 5 pass - `inst-cat-served-if`
   1. [ ] - `p1` - Build a served entry with the resolved schema and the traits `kind`, `admitted_subjects`, and `enforcement` - `inst-cat-index`
8. [ ] - `p1` - **ELSE** - `inst-cat-unavail-if`
   1. [ ] - `p1` - Build an unavailable entry; write a log entry with the metric and the reason and increment `contract_validation_failures_total` with the `bootstrap` surface, which covers every catalogue read, and the reason; bootstrap does not fail and readiness does not change - `inst-cat-unavail`
9. [ ] - `p1` - An entry is built from the current registry content only, never from stored state and never from the entry it replaces, so a running and a restarted replica build the same entry - `inst-cat-source`
10. [ ] - `p1` - On a bounded schedule, read each metric type the catalogue holds and each subject type in the cache of registered subject types with a conditional registry read, which returns a document only when it changed since QE read it; build again, through steps 5 to 9, each entry whose type changed; remove the entry of a deleted metric type; drop a deleted subject type from the cache; the scheduled read reads nothing from storage; a scheduled read that fails or reaches its time limit changes no entry (a served entry stays served) and increments `catalog_read_failures_total` with the phase `scheduled`; a read that succeeds sets `catalog_last_scheduled_read_timestamp_seconds`; a failed read at start and a failed read on first use increment the same counter with the phases `start` and `first_use`; `catalog_unavailable_metrics` reports the number of unavailable entries the replica holds - `inst-cat-refresh`
11. [ ] - `p1` - **RETURN** the catalogue; a wrong metric type never fails gear bootstrap, which fails only on QE's own state and configuration (`cpt-cf-quota-enforcement-flow-gear-bootstrap`, foundation feature) - `inst-cat-publish`

### Subject Admission

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-subject-resolution`

**Input**: the shape-checked and admitted request (`tenant_id`, `subjects[{type, id}]`, metric) and the served
`MetricCatalog` entry of the metric

**Output**: the complete set of `(subject_type, subject_id)` pairs for the request, or a canonical error before
evaluation

**Steps**:
1. [ ] - `p1` - Reject an empty subject list, an empty subject id, two subjects of one type, or a tenant subject whose id is not `tenant_id` - `inst-res-shape`
2. [ ] - `p1` - Require admission of the request in the mode of its path before any catalogue lookup (`cpt-cf-quota-enforcement-flow-authorized-admission`) - `inst-res-authz`
3. [ ] - `p1` - Add the tenant subject `(gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~, tenant_id)` unless the request names it; it is the same subject either way - `inst-res-tenant`
4. [ ] - `p1` - **FOR EACH** subject - `inst-res-each`
   1. [ ] - `p1` - Check the form of the subject type id first: an id that does not start with `gts.cf.core.quota.subj.v1~` is canonical `InvalidArgument` with no registry call. Then check the subject type against the `admitted_subjects` trait of the metric type: the tenant type is always admitted, and a type is admitted when it equals an admitted type or derives from one (the admitted type id is a prefix of its chained GTS type id) - `inst-res-map`
   2. [ ] - `p1` - For a type that step 4.1 admits, check that the type is registered and that the subject fits its schema, through the process-local cache of registered subject types: a type the cache does not hold is looked up in `types-registry` once, a registered type stays cached until a scheduled catalogue read finds it deleted (`inst-cat-refresh`), an unknown type is cached for a short time and is `InvalidArgument`, and a failed lookup is `ServiceUnavailable` - `inst-res-registered`
   3. [ ] - `p1` - **IF** the type is not admitted - `inst-res-invalid-if`
      1. [x] - `p1` - **RETURN** canonical `InvalidArgument` before evaluation - `inst-res-invalid`
   4. [ ] - `p1` - Append `(subject_type, id)` - `inst-res-append`
5. [x] - `p1` - **RETURN** the complete subject set for applicable-Quota lookup - `inst-res-return`

### Admission Check for Quota and Policy Writes

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-catalog-membership`

**Input**: the subject type and metric type of a Quota write, or the metric type that a Policy write names in its
scope, and `MetricCatalog`

**Output**: pass, or a canonical rejection before persistence

**Steps**:
1. [ ] - `p1` - Look up the metric's entry in `MetricCatalog`; for a metric the catalogue does not hold the write reads the metric type as a request does (`inst-ing-lookup`), with the same errors: a type that is not registered is canonical `InvalidArgument`, and a read that fails or reaches its time limit is `ServiceUnavailable`; the evaluation hot path never runs this check - `inst-mem-lookup`
2. [ ] - `p1` - **IF** the metric's entry is unavailable; a Quota deactivation, a Policy rollback, and a Policy delete do not run this check - `inst-mem-entry-if`
   1. [ ] - `p1` - **RETURN** rejection with `PROJECTION_NOT_RESOLVABLE`; increment `contract_validation_failures_total` with the closed write surface - `inst-mem-entry`
3. [ ] - `p1` - **IF** the subject type of a Quota write is unregistered or not derived from `gts.cf.core.quota.subj.v1~` - `inst-mem-invalid-if`
   1. [ ] - `p1` - **RETURN** canonical rejection per `cpt-cf-quota-enforcement-fr-subject-type-registry`; increment `contract_validation_failures_total` with the closed write surface - `inst-mem-invalid`
4. [ ] - `p1` - **IF** a Quota write names a subject type that the `admitted_subjects` trait of its metric type does not admit - `inst-mem-metric-if`
   1. [ ] - `p1` - **RETURN** canonical `InvalidArgument`; increment `admitted_metric_violations_total` with the closed write surface - `inst-mem-metric`
5. [x] - `p1` - **RETURN** pass; the quota-lifecycle and resolution-policy-engine features invoke this check inside their write paths, which own the writes themselves - `inst-mem-pass`

## 4. States (CDSL)

### MetricCatalog State Machine

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-state-projection-catalog`

The states are those of one metric type's entry. A metric type that QE has not read, or that is not registered, has no
entry.

**States**: Absent, Served, Unavailable

**Initial State**: Absent

**Transitions**:
1. [ ] - `p1` - **FROM** Absent **TO** Served **WHEN** QE reads the metric type and every check of `cpt-cf-quota-enforcement-algo-catalog-bootstrap` passes - `inst-catst-publish`
2. [ ] - `p1` - **FROM** Absent **TO** Unavailable **WHEN** QE reads the metric type and a check of `cpt-cf-quota-enforcement-algo-catalog-bootstrap` fails; gear bootstrap does not fail (`cpt-cf-quota-enforcement-fr-contract-validation`) - `inst-catst-reject`
3. [ ] - `p1` - **FROM** Unavailable **TO** Served **WHEN** a scheduled catalogue read finds that the metric type changed, builds the entry again, and every check passes - `inst-catst-recover`
4. [ ] - `p1` - **FROM** Served **TO** Served or Unavailable **WHEN** a scheduled catalogue read finds that the metric type changed in `types-registry` and builds the entry again: Served when every check passes, Unavailable when one fails - `inst-catst-revised`
5. [ ] - `p1` - **FROM** Served or Unavailable **TO** Absent **WHEN** a scheduled catalogue read finds that the metric type was deleted - `inst-catst-deleted`

An entry has no other transition. `types-registry` lets an owner replace the content of a registered type under the
same id, or delete the type (ADR-0012). Transitions 4 and 5 bring such a change to every replica within one read
interval. Another major version of a metric type has another type id, so it is another metric with its own entry and
its own Quotas. A metric type has no QE-side lifecycle; `types-registry` owns its versions.

## 5. Definitions of Done

### Resolver-Owned Base Types

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-base-registration`

The system **MUST NOT** define or register a GTS type for subjects or metrics, at bootstrap or later, and **MUST NOT**
expose a registration endpoint. The period type `gts.cf.core.qe.period_type.v1~` and the quota type
`gts.cf.core.qe.quota_type.v1~`, with their instances, stay QE's own. The metric base `gts.cf.core.quota.metric.v1~`
with its traits `kind`, `admitted_subjects`, and `enforcement`, the subject base `gts.cf.core.quota.subj.v1~`, and the
tenant subject type belong to the quota resolver SDK. The quota resolver gear publishes them through the ToolKit GTS publication of its gear attribute, and the
runtime waits for their admission before any application `init` runs; QE registers none of them and needs no start
order with the quota resolver. The module that owns a metric registers its metric type.

**Implements**:
- `cpt-cf-quota-enforcement-flow-owner-projection-publication`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

**Touches**:
- API: `PlatformTypesRegistryApi` (platform `types-registry-sdk`, obtained from ClientHub; reads of metric and subject
  types only)
- Entities: `MetricCatalog`

### Metric Catalogue

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-projection-catalog`

The system **MUST** keep the process-local `MetricCatalog` with one entry per metric type that QE has read. It
**MUST** read a metric type from `types-registry` by its id: at start for the metric types of the active Quotas and
Policies, and once, with a time limit, when a request or a Quota or Policy write names a metric type the catalogue
does not hold. A type that is not registered **MUST** be `InvalidArgument`, remembered for a short time; a read that
fails or reaches its time limit **MUST** be `ServiceUnavailable`. The system **MUST** check each type it reads
(concrete, derived from the metric base, an own version of major 1 or higher, valid traits, admitted subject types
derived from the subject base), serve an entry that passes, and make an entry that
fails unavailable without failing bootstrap or changing readiness. A scheduled read **MUST** ask the registry, with a
conditional read, whether each held metric type and each cached subject type changed; it **MUST** build again each
changed entry, remove the entry of a deleted metric type, and drop a deleted subject type,
so that a running replica holds the same entry as a restarted one within one read interval. An entry **MUST** be built
from the registry content only; a read **MUST NOT** check stored Quotas or Policies. Metric types are
registry-resident; no QE-side table exists.

**Implements**:
- `cpt-cf-quota-enforcement-algo-catalog-bootstrap`
- `cpt-cf-quota-enforcement-state-projection-catalog`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

**Touches**:
- API: `PlatformTypesRegistryApi` (catalogue reads; never for a request whose metric the catalogue holds)
- Entities: `MetricCatalog`

### Gateway Ingress Validation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-ingress-validation`

The system **MUST** extend the foundation Gateway with fail-closed validation of debit, lease acquisition, preview,
and each batch item: explicit `tenant_id`, at least one `{type, id}` subject, and a metric object
`{type, id?, metadata}` whose `metadata` is never defaulted to `{}`. It **MUST** reject malformed public request shape
before admission, admit the request in the mode of its path, and only then check subject admission and validate the
whole metric object against the resolved schema of its metric type through the process-local catalogue. A request for
a metric whose entry is unavailable **MUST** receive canonical `FailedPrecondition` and change nothing; credit, lease
commit, lease release, return, and rollback do not pass this validation and **MUST** run while the entry is
unavailable. A request for a metric the catalogue does not hold **MUST** receive `InvalidArgument` when the metric
type is not registered, and `ServiceUnavailable` when the registry read fails or reaches its time limit. Failures
**MUST** map to the appropriate canonical error and **MUST NOT** be encoded as `Decision::Denied`. Consumer DTOs
contain no `caller_type`, a field that names the kind of the caller.

**Implements**:
- `cpt-cf-quota-enforcement-flow-ingress-validation`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

**Touches**:
- API: the subject and metric fields on debit, lease acquisition, preview, and batch-item DTOs (no new endpoint)
- Entities: `MetricCatalog`

### Subject Admission at Ingress

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-subject-resolution`

The system **MUST** shape-check `tenant_id` and the `{type, id}` subjects, require admission of the request, add the
implicit tenant subject, and check every subject type against the `admitted_subjects` trait of the metric type (exact
or derived type; the tenant type always admitted). The admission check makes no registry call; whether a type is
registered comes from the process-local cache of registered subject types, which calls the registry once for a type it
does not hold. It returns the complete `(subject_type, subject_id)` set consumed by `EvaluationOrchestrator`.

**Implements**:
- `cpt-cf-quota-enforcement-algo-subject-resolution`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

**Touches**:
- API: subject fields on every evaluation and snapshot DTO
- Entities: `MetricCatalog`

### Admission Check for Writes

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-catalog-membership`

The system **MUST** check every Quota or Policy write against the metric's `MetricCatalog` entry before persistence.
A write for a metric the catalogue does not hold **MUST** read the metric type as a request does, with the same
errors. A Quota create or change and a Policy create or change for a metric whose entry is unavailable **MUST**
receive `PROJECTION_NOT_RESOLVABLE`. A Quota deactivation, a Policy rollback, and a Policy delete **MUST NOT** run
this check. A subject
type that is unregistered or not derived from the subject base **MUST** receive a canonical rejection per
`cpt-cf-quota-enforcement-fr-subject-type-registry`; a Quota whose subject type its metric does not admit **MUST** be
rejected with `InvalidArgument`. The evaluation hot path **MUST NOT** run this check.

**Implements**:
- `cpt-cf-quota-enforcement-algo-catalog-membership`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

**Touches**:
- API: no new endpoint (the quota-lifecycle and resolution-policy-engine write paths invoke this check)
- Entities: `MetricCatalog`

### Contract-Validation Telemetry

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-contract-validation-telemetry`

The system **MUST** emit `contract_validation_failures_total` (rejected metric objects, rejected writes, and
unavailable metric types, by closed validation surface/reason) and `admitted_metric_violations_total` (subject types a
metric does not admit, by closed validation surface) through the foundation telemetry conventions. The ingress surface
increments in `cpt-cf-quota-enforcement-flow-ingress-validation`; the Quota/Policy write and catalogue-read surfaces
increment in the rejection steps of `cpt-cf-quota-enforcement-algo-catalog-membership` and in the unavailable-entry
step of `cpt-cf-quota-enforcement-algo-catalog-bootstrap`. Each unavailable metric **MUST** be counted there with its
reason and named with that reason in a log entry. Metric names, subject types, and caller attribution **MUST NOT**
appear as label values; permitted dimensions are the closed `surface` and `reason` enums from PRD §5.16.

**Implements**:
- `cpt-cf-quota-enforcement-flow-ingress-validation`
- `cpt-cf-quota-enforcement-algo-catalog-membership`
- `cpt-cf-quota-enforcement-algo-catalog-bootstrap`

**Constraints**: `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

**Touches**:
- API: platform observability stack (`QeMetrics` port with its `OpenTelemetry` adapter for metrics, `tracing` for logs and spans, `toolkit` `otel` feature, per the foundation telemetry conventions)
- Entities: gear-specific counters per PRD §5.16

## 6. Acceptance Criteria

- [ ] QE registers no GTS type for subjects or metrics: after bootstrap, `types-registry` holds no subject type,
  metric base, or metric type that QE wrote
- [ ] Bootstrap succeeds, and readiness does not change, when a metric type of an active Quota is wrong; bootstrap
  fails only on QE's own state and configuration
- [ ] A metric is unavailable, while other metrics are served and bootstrap succeeds, when its metric type is
  abstract, is not derived from `gts.cf.core.quota.metric.v1~`, has a wrong trait (for example an admitted subject
  type that is not a quota subject type), or has an own version of major 0; its requests receive canonical `FailedPrecondition` and change nothing
- [ ] A metric whose type is registered after QE started is served on its first request with no restart; an
  unavailable metric is served after its type becomes valid; a metric type changed under the same id in
  `types-registry` is served from its new content by a running replica within one read interval, with the same entry
  as on a restarted replica, and stays served when the new content stops admitting the subject type of a stored Quota or when an active Policy
  does not fit it; a
  deleted metric type removes its metric from the catalogue; each unavailable metric is reported with its reason in a
  log entry and in `contract_validation_failures_total`
- [ ] A request for a metric type that is not registered receives `InvalidArgument`; a second such request inside the
  short time QE remembers the answer makes no registry call; when the registry read of a metric type the catalogue
  does not hold fails or reaches its time limit, the request receives `ServiceUnavailable` and succeeds when repeated
  after the registry answers; commit, release, return, and rollback of a lease or charge whose metric is unavailable
  succeed
- [ ] A registration by another module that names an existing metric type in any of its fields changes nothing in how
  that metric is served
- [ ] A request with no subject, two subjects of one type, a tenant subject of another tenant, a subject type the
  metric does not admit, a metric object with no `metadata`, a metric object that does not fit its metric type, or a
  metric type that is not registered receives canonical `InvalidArgument` with a stable field-level reason, never
  `Decision::Denied`, and increments `contract_validation_failures_total` or `admitted_metric_violations_total`
  accordingly; absent `metadata` is never defaulted to `{}`
- [ ] A subject type derived from an admitted type is admitted; a metric type that admits the subject base
  `gts.cf.core.quota.subj.v1~` admits every registered subject type; the tenant type is admitted for a metric with
  empty `admitted_subjects`
- [ ] With `types-registry` unreachable, ingress validation of a metric the catalogue holds still succeeds against the
  process-local catalogue: no evaluation write or preview request path performs a registry call for the metric type
  (verified by fault injection on the registry dependency), and a request for a metric the catalogue does not hold
  receives `ServiceUnavailable`
- [ ] With `tenant_id=T` and subject `{type=U_t, id=U}` for an admitted user type `U_t`, the subject set is exactly
  `{(tenant type, T), (U_t, U)}`; naming `{type=tenant type, id=T}` as well yields the same set
- [ ] On QE's API the complete attribution is sent to PDP; changing `tenant_id`, a subject, the metric type, or the
  metric id to an unauthorized target produces `PermissionDenied` before evaluation and no storage mutation
- [ ] Error precedence is stable: malformed public request shape returns `InvalidArgument` before admission; a
  structurally valid unauthorized request returns `PermissionDenied` before any catalogue lookup; an admitted request
  with a subject type that is not admitted or a metric object that does not fit its metric type returns
  `InvalidArgument`
- [ ] The write check rejects, with `PROJECTION_NOT_RESOLVABLE`, a Quota create or change and a Policy create or
  change for a metric whose entry is unavailable; a Quota deactivation, a Policy rollback, and a Policy delete for
  such a metric run; and it rejects a Quota whose subject type its metric does not admit with `InvalidArgument`
- [ ] Metrics scrape shows no metric name, subject type, or caller attribution as a label value on
  `contract_validation_failures_total` or `admitted_metric_violations_total`

## 7. Additional Context (optional)

- **ADR dependency**: ADR-0013 decides the metric type and the metric object; ADR-0008 decides subjects, admission,
  and the types the quota resolver SDK owns; ADR-0012 decides when QE reads a metric type and what a wrong metric type
  stops. This document restates only what the feature needs.
- **Rollout / rollback**: every replica builds an entry from the same registry content, so replicas reach the same entries, also after a restart, and a new metric type needs no redeploy and no
  QE configuration.
- **Test layering**: the checks of a metric type and the admission checks get unit tests against catalogue fixtures;
  ingress validation, PDP authorization, and registry-fault behavior get integration tests; attribution spoofing on
  QE's API is an adversarial integration test.
- **Non-applicable review domains**: UX/accessibility is not applicable; there is no user-facing surface. No QE-side
  persistence is added, so no schema or retention concerns arise here. A metric type must not carry secrets.
