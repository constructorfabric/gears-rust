---
status: accepted
date: 2026-11-18
---

# Quota Subjects Are Quota Resolver Subject Types

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Subjects of a request](#subjects-of-a-request)
  - [GTS types](#gts-types)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Quota resolver subject types, admitted by the metric](#a-quota-resolver-subject-types-admitted-by-the-metric)
  - [(b) Keep subject projections, keyed by the quota resolver subject type](#b-keep-subject-projections-keyed-by-the-quota-resolver-subject-type)
  - [(c) Keep scopes, map quota resolver subject types in the plugin](#c-keep-scopes-map-quota-resolver-subject-types-in-the-plugin)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-quota-resolver-subjects`

## Context and Problem Statement

The quota resolver ([DESIGN](../../../quota-resolver/docs/DESIGN.md)) is the entry point through which modules charge
and read quotas; QE is its backend. The resolver SDK defines the GTS types that every backend shares:

* the quota subject base `gts.cf.core.quota.subj.v1~`: a subject is `{type, id}`, where `type` derives from the base;
* the final tenant subject type `gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~`, whose id is the tenant id;
* the metric base `gts.cf.core.quota.metric.v1~`: a metric is a type derived from it, with the traits `kind`,
  `admitted_subjects`, and `enforcement` (ADR-0013).

QE names subjects differently (ADR-0007): a caller sends a scope instance (`user` or `tenant`) and an id, and QE maps
each `(metric, scope)` pair to a subject projection that the metric owner publishes. QE also owns its own metric and
resource bases. Two vocabularies for one subject would need a translation in the plugin and a second configuration.
How does QE name the subject of a quota and of a request?

## Decision Drivers

* One subject vocabulary for the resolver and QE, with no translation table.
* An owner that knows which kinds of subject its metric counts can state them, as a license resource does in
  license-resolver (`admitted_subjects`). QE then rejects a Quota or a request for another kind at once; without the
  list, such a Quota is created and never applies.
* An owner that does not know them, because the operator defines the subject types, names no operator type.
* Operator-defined subject kinds (device, agent, service account) without a QE change.
* QE keeps evaluation concerns: the metadata of a request's metric, Quota metadata, policies.

## Considered Options

* (a) A quota subject is a quota resolver subject type and an id; the metric type admits subject types.
* (b) Keep subject projections, keyed by `(metric, quota resolver subject type)` instead of `(metric, scope)`.
* (c) Keep scopes; the resolver plugin maps quota resolver subject types to scopes from configuration.

## Decision Outcome

Chosen option: (a), because the subject a caller names is the subject a quota stores, and admission is one trait on
the metric type.

### Subjects of a request

* A Quota and a request name a subject as `{type, id}`, `type` derived from `gts.cf.core.quota.subj.v1~`. A Quota
  stores it as `(subject_type, subject_id)`.
* The metric type declares the trait `admitted_subjects`: the subject types the metric's quotas may count. The list is a guard against a wrong
  configuration, for an owner that knows which kinds of subject its metric counts. An owner that does not know them
  admits the subject base `gts.cf.core.quota.subj.v1~`: every subject type derives from it, so the metric then admits
  every subject type, the types that the operator registers included. A type that derives from an admitted type is
  admitted. QE checks admission when it creates a Quota (the subject of a Quota does not change afterwards) and on every evaluation and snapshot request; a
  type that is not admitted is `InvalidArgument`. The tenant subject type is always admitted, so the empty default
  admits tenant quotas only. One exception: a
  snapshot read through the resolver plugin, for a metric that does not admit the subject's type, returns only the
  tenant's Quotas and no error, so an end user of any mapped principal type can read the tenant's quota state
  (`cpt-cf-quota-enforcement-fr-quota-snapshot-read`).
* A request names at least one subject. Two subjects of one type are `InvalidArgument`. A subject of the tenant type
  must name the request's tenant, otherwise `InvalidArgument`; it is the same subject as the implicit tenant, which QE
  adds to every request. A request that charges only the tenant names that subject.
* The engine sees two tiers: `Tenant` for the tenant subject type, `Subject` for every other type. The default engine
  keeps its order: `Subject` before `Tenant`, then a quota with a cap before one without, then the smallest remaining
  amount. It still charges one quota with room, so a subject whose quota is exhausted spills into the tenant quota. A
  policy that must hold every cap, for example of both a user and a device, uses a `cel` Policy that charges each
  quota (ADR-0010).

### GTS types

| Type                                                         | Owner          |
|--------------------------------------------------------------|----------------|
| Quota subject base, tenant subject type, metric base with the traits `kind`, `admitted_subjects`, `enforcement` | Quota resolver SDK |
| A metric type                                                | The module that owns the metric |
| A subject type of a principal                                | The operator   |

QE owns no GTS type for subjects or metrics; its types for fields of a Quota, such as the quota type and the period
type, stay its own. It no longer defines `gts.cf.core.qe.subj.v1~`, `gts.cf.core.qe.scope.v1~` and its instances,
`gts.cf.core.qe.res.v1~`, `gts.cf.core.qe.metric_type.v1~`, `gts.cf.core.qe.request.v1~`, and
`gts.cf.core.qe.constraint.v1~` (ADR-0013). The `subject_projections` and `resource_projections` catalogue
configurations are removed.

### Consequences

* This record supersedes the subject projection, scope, resource base, and metric base parts of ADR-0007. ADR-0013
  removes the request contract, the constraint contract, and the resource; ADR-0012 decides when QE reads and checks a
  metric type.
* The QE SDK depends on the quota resolver SDK for these types; the resolver does not depend on QE.
* The engine contract replaces the `User` tier with `Subject`, and a `cel` Policy sees the subject type.
* Quota rows keep their shape; the subject column holds a quota resolver subject type.

### Confirmation

Tests: admission of an exact and of a derived type, a metric that admits the subject base, a type that is not
admitted, on a request and on a Quota create,
an empty subject list, two subjects of one type, a tenant subject of another tenant, and engine tiers for several
subject types.

## Pros and Cons of the Options

### (a) Quota resolver subject types, admitted by the metric

* Good, because a caller, a Quota, and a snapshot use the same subject.
* Good, because a new subject kind needs only a registered type and an admitted entry.
* Bad, because the empty default admits tenant quotas only: an owner that wants quotas for other subjects must set
  `admitted_subjects`, to the subject base when it does not know the subject types.

### (b) Keep subject projections, keyed by the quota resolver subject type

* Good, because ADR-0007's catalogue stays.
* Bad, because a projection adds a second type per subject and metric that names nothing new.

### (c) Keep scopes, map quota resolver subject types in the plugin

* Good, because QE does not change.
* Bad, because the operator states the subject kinds twice, and QE's own API keeps a vocabulary the resolver's callers
  do not use.

## More Information

Quota resolver subject model:
[ADR-0002](../../../quota-resolver/docs/ADR/0002-cpt-cf-quota-resolver-adr-principal-subject-mapping.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-projection-contracts`
* `cpt-cf-quota-enforcement-fr-subject-type-registry`
* `cpt-cf-quota-enforcement-fr-subject-resolution`
* `cpt-cf-quota-enforcement-fr-quota-resolution-engine`
* `cpt-cf-quota-enforcement-adr-projection-contracts`
