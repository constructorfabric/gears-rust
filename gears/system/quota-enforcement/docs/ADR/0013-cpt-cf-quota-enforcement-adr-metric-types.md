---
status: accepted
date: 2026-11-23
---

# Metric Types Replace Request Contracts, Constraint Contracts, and Resources

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The metric of a request](#the-metric-of-a-request)
  - [Quota metadata](#quota-metadata)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) The metric is a type; Quota metadata are conditions over its metadata](#a-the-metric-is-a-type-quota-metadata-are-conditions-over-its-metadata)
  - [(b) Keep the request contract, found by a name derived from the metric](#b-keep-the-request-contract-found-by-a-name-derived-from-the-metric)
  - [(c) Keep the request contract, found by listing, with an owner rule](#c-keep-the-request-contract-found-by-listing-with-an-owner-rule)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-metric-types`

## Context and Problem Statement

[ADR-0007](0007-cpt-cf-quota-enforcement-adr-projection-contracts.md) gives every metric three registered objects next
to the metric itself:

* a **request contract**: a type that points at the metric and holds the schema of the request's operation metadata,
  the admitted subject types, the `enforcement` mode, and a pointer to the constraint contract;
* a **constraint contract**: a type with the schema of the metadata that an operator writes into a Quota;
* an optional **resource**: a type with the identity and properties of what the operation targets.

A request names a metric, not a request contract. So QE must find the contract from the metric: it lists all request
contracts in types-registry and builds an index from metric to contract. That index needs a rule that a metric has
exactly one contract, and a state for a metric that has two. A contract that another module registers by mistake for a
metric it does not own then stops that metric for every tenant.

The reasons for these objects have gone:

* ADR-0007 forbids two contracts because two subject projections for one metric would split one budget into two
  counters. [ADR-0008](0008-cpt-cf-quota-enforcement-adr-quota-resolver-subjects.md) removed subject projections. A
  counter is now keyed by subject type, subject id, and metric, and no contract is part of the key.
* The request metadata and the resource properties are two sets of attributes of one operation. Both come from the
  metric owner, both are read only by policies, and neither is part of the counter key.
* The constraint contract is a second schema of the same attributes: a Quota that limits one region names the region
  that the request carries. The schema of the request's metadata can serve both.

license-resolver has the same needs and no such objects: a request names a resource type, the resolver reads that
type by its id, and the type holds the metadata schema and the admitted subjects.

Which registered objects does a metric need, and what defines and checks Quota metadata?

## Decision Drivers

* A request names every object that QE must read. QE keeps no index from one registered object to another.
* A registration by another module cannot change how a metric is served.
* The party that knows a piece of data defines its form.
* Quota subjects and metrics work as subjects and resources work in license-resolver.
* A metric needs as few registered objects as possible.

## Considered Options

* (a) The metric is a type with traits and a metadata schema. There is no request contract, no constraint contract,
  and no resource. Quota metadata are conditions over the metadata of the request, checked against the metric type.
* (b) Keep the request contract, and find it by a type id that is derived from the metric's id.
* (c) Keep the request contract and the listing. Count only a contract of the metric's owner, and choose one contract
  by a fixed rule when the owner has several.

## Decision Outcome

Chosen option: (a).

### The metric of a request

* A metric is a GTS type derived from the quota resolver's metric base `gts.cf.core.quota.metric.v1~`
  ([quota resolver ADR-0004](../../../quota-resolver/docs/ADR/0004-cpt-cf-quota-resolver-adr-metric-types.md)). The
  module that owns the metric registers the type, named under that module.
* A request names its metric as `{type, id?, metadata}`:
  * `type` is the metric type;
  * `id` is optional and names the one thing the amount is counted for, for example a model or a virtual machine;
  * `metadata` holds the attributes of the operation that policies read. The metric type refines its schema.
* The metric type has three traits: `kind` (`counter` or `gauge`), `admitted_subjects` (ADR-0008), and `enforcement`
  (`quota_gated` or `direct`).
* QE reads a metric type from types-registry by its id
  ([ADR-0012](0012-cpt-cf-quota-enforcement-adr-contract-catalogue.md)). It checks the whole metric object of a
  request against the resolved schema of the type, checks each subject type against `admitted_subjects`, and takes
  `kind` and `enforcement` from the traits.
* A Quota names a subject and a metric type. The counter key is the subject type, the subject id, and the metric type.
  The metric `id` is not part of the key in P1. The PDP and the Engine receive it, so a Policy can filter by it,
  and the attribution of a charge includes it. No counter exists per `id`, so a limit per `id` is not expressible.
* There is no request contract and no resource. An attribute that a resource carried goes into the metadata of the
  metric. QE has no `resource_projections` configuration.
* Another major version of a metric type is another type id, so it is another metric with its own Quotas. The own
  version of a metric type is major 1 or higher: types-registry admits any change of a type of major 0, so QE does
  not serve such a type (ADR-0012).
* QE owns no GTS type for subjects or metrics. Its own types describe fields of a Quota, for example the quota type
  and the period type, and they do not change.

### Quota metadata

* There is no constraint contract. The metadata of a Quota is a set of **conditions** over the `metadata` of the
  request's metric object. The metric type defines the attributes, so a Quota and a request use one schema.
* The metadata is a JSON object of at most 4 KB. Each key names a property of the metric type's `metadata` whose type
  is a string, an integer, or a boolean. Its value is a value of that type, or a non-empty array of such values. The
  empty object has no conditions.
* QE core supports only these conditions: equality, and membership in a list. Selecting the Quotas that apply is the
  same kind of step as matching the subject and the metric, so it is the same for every Engine and runs on the locked
  rows. A selection that needs more, for example a range, a negation, or a priority among Quotas, belongs to a Policy:
  the `cel` Engine reads the same request metadata.
* QE brings the `metadata` of a request to one canonical form when it admits the request: a number with no fractional
  part is an integer. JSON Schema accepts `5.0` for an integer property, and without this step neither the condition
  `5` nor a `cel` comparison would equal it. The conditions, the Engine, and the request fingerprint use this form.
* A Quota applies to a request only when every condition holds: the request's `metadata` has the property, and its
  value equals the condition's value or one of its values. When the request lacks the property, the Quota does not
  apply. QE core decides this together with the subject and the metric, before the Engine runs, so every Engine gets
  only the Quotas that apply.
* When a Quota is created or its metadata changes, QE core checks the conditions against the metric type's schema from
  the catalogue (ADR-0012). On a violation QE writes nothing and answers `InvalidArgument` with a list of what is
  wrong: for each item, the place in the metadata and the reason.
* The owner can revise a metric type after a Quota is written. types-registry admits a revision when the new schema
  accepts every instance that the previous one accepted; such a revision can still widen the type of a property,
  and a stored condition then no longer fits. QE does not fail evaluation for this. It checks the
  conditions of the active Quotas against the current type on a schedule and reports the Quotas that no longer fit
  through the `quota_conditions_unfit_total` gauge and a log entry. A change of a Policy does not touch Quota
  metadata.
* A snapshot read may carry the `metadata` of a request for a target. It then returns only the Quotas whose
  conditions hold on it, so a caller can see which Quotas a request would meet.
* An Engine does not check Quota metadata: QE core has already applied its conditions. The snapshot of a Quota that
  the Engine receives still includes the metadata.

### Consequences

* This record supersedes the request contract, the constraint contract, the resource projection, and their envelope
  rules in ADR-0007. Two rules of ADR-0007 stay: QE validates a whole document against the resolved schema of its
  type, and evaluation reads the process-local catalogue, not the registry.
* The catalogue of ADR-0012 holds metric types that QE reads by id. It has no listing, no rule of one contract per
  metric, and no state for a metric with two contracts.
* A policy no longer sees a `resource`. For the `cel` Engine, `request` is the metadata of the request's metric. A
  Quota in the `cel` environment carries no metadata: the Quotas it sees already apply to the request.
* Quota metadata hold only attributes of the request. An attribute that only a Policy knows, such as a priority among
  Quotas, cannot be stored on a Quota; a Policy derives it from the Quota's subject type or from the request.
* A mistake in Quota metadata is found when the Quota is written.
* A request with several metrics repeats in each metric the attributes that one resource carried before.
* The attribution of a charge (ADR-0011) is the tenant, the subjects, and the metric with its type and id.
* The quota resolver's request loses the resource and the separate operation metadata of a metric item; the metric
  item is the metric object and its amount.

### Confirmation

Tests: a request whose metric object does not fit the metric type; a metric type that is not registered, is abstract,
or does not derive from the metric base; a subject type that the metric does not admit; a metric with `enforcement`
`direct`; a Quota write whose metadata names a property the metric type does not declare, a property that is not a
scalar, or a value of another type, with the list in the answer, and one that passes; a request that a Quota's
conditions admit, one they do not, and one that lacks the property, under each Engine; a registration by another module that names
an existing metric in any of its fields and changes nothing.

## Pros and Cons of the Options

### (a) The metric is a type; Quota metadata are conditions over its metadata

* Good, because QE reads every object by the id that the request or the Quota names.
* Good, because only the owner of the metric's name can change the metric.
* Good, because subjects and metrics follow the model of license-resolver.
* Good, because a metric needs one registered object.
* Good, because Quota metadata are checked in full when they are written, and no change of a Policy or of a metric
  type makes a stored Quota fail at evaluation.
* Bad, because a Quota cannot carry an attribute that the request does not carry.
* Bad, because the request shape, the Engine contract, and the stored metric ids change.

### (b) Keep the request contract, found by a name derived from the metric

* Good, because the lookup is a read by id, and a second contract cannot exist.
* Bad, because a metric still needs two objects whose ids must agree, and a constraint contract and a resource beside
  them.

### (c) Keep the request contract, found by listing, with an owner rule

* Good, because the request shape does not change.
* Bad, because the listing, the index, and the choice between contracts stay, for an object that no longer decides
  which counter is charged.

## More Information

Subject and resource contracts in license-resolver:
[README](../../../license-resolver/license-resolver-sdk/README.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-projection-contracts`
* `cpt-cf-quota-enforcement-fr-contract-validation`
* `cpt-cf-quota-enforcement-fr-subject-type-registry`
* `cpt-cf-quota-enforcement-fr-quota-metadata`
