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
  - [(a) The metric is a type; the Engine checks Quota metadata](#a-the-metric-is-a-type-the-engine-checks-quota-metadata)
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
* The metric owner publishes the constraint contract, but only the operator's Policy reads Quota metadata. The owner
  does not know that Policy.

license-resolver has the same needs and no such objects: a request names a resource type, the resolver reads that
type by its id, and the type holds the metadata schema and the admitted subjects. The evaluator of license-enforcement
defines the form of the conditions of a grant and checks them when a grant is written.

Which registered objects does a metric need, and who checks Quota metadata?

## Decision Drivers

* A request names every object that QE must read. QE keeps no index from one registered object to another.
* A registration by another module cannot change how a metric is served.
* The party that knows a piece of data defines its form.
* Quota subjects and metrics work as subjects and resources work in license-resolver.
* A metric needs as few registered objects as possible.

## Considered Options

* (a) The metric is a type with traits and a metadata schema. There is no request contract, no constraint contract,
  and no resource. The Engine checks Quota metadata.
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
  The metric `id` is not part of the key in P1; QE gives it to the Engine with the request.
* There is no request contract and no resource. An attribute that a resource carried goes into the metadata of the
  metric. QE has no `resource_projections` configuration.
* Another major version of a metric type is another type id, so it is another metric with its own Quotas. The own
  version of a metric type is major 1 or higher: types-registry admits any change of a type of major 0, so QE does
  not serve such a type (ADR-0012).
* QE owns no GTS type for subjects or metrics. Its own types describe fields of a Quota, for example the quota type
  and the period type, and they do not change.

### Quota metadata

* There is no constraint contract. QE core checks only that Quota metadata is a JSON object of at most 4 KB, and
  stores it. Its meaning belongs to the Policy that reads it.
* When a Quota is created or its metadata changes, QE gives the metadata to the Engine of the Policy that is active
  for the Quota's metric. The Engine answers that it accepts the metadata, or that it rejects it, with a list of what
  is wrong: for each item, the place in the metadata and the reason.
* On a rejection QE writes nothing and answers `InvalidArgument` with the Engine's list.
* The `most-restrictive-wins` Engine reads no Quota metadata and accepts only an empty object. What another Engine
  checks is the decision of that Engine.
* A change of a Policy is the operator's responsibility. QE does not check existing Quotas when a Policy is created,
  changed, or replaced, and does not check their metadata again later. When the active Policy cannot read the metadata
  of a Quota, the Engine reports an error at evaluation: QE rejects the request with `POLICY_EVALUATION_FAILED`
  (`FailedPrecondition`) and changes no counter. The operator
  makes sure, before it changes a Policy, that the metadata of the existing Quotas fits the new Policy.

### Consequences

* This record supersedes the request contract, the constraint contract, the resource projection, and their envelope
  rules in ADR-0007. Two rules of ADR-0007 stay: QE validates a whole document against the resolved schema of its
  type, and evaluation reads the process-local catalogue, not the registry.
* The catalogue of ADR-0012 holds metric types that QE reads by id. It has no listing, no rule of one contract per
  metric, and no state for a metric with two contracts.
* A policy no longer sees a `resource`. For the `cel` Engine, `request` is the metadata of the request's metric, and
  `arbitration` is the Quota metadata, which has no registered schema. The `cel` Engine gives `arbitration` no type and accepts any
  JSON object as Quota metadata; what it reads and a Quota lacks is found at evaluation.
* The Engine contract of the SDK gets one more operation, the check of Quota metadata. This breaks existing Engine
  implementations.
* A mistake in Quota metadata is found when the Quota is written only as far as the Engine checks it. The rest is
  found at evaluation, as a rejected request.
* A request with several metrics repeats in each metric the attributes that one resource carried before.
* The attribution of a charge (ADR-0011) is the tenant, the subjects, and the metric with its type and id.
* The quota resolver's request loses the resource and the separate operation metadata of a metric item; the metric
  item is the metric object and its amount.

### Confirmation

Tests: a request whose metric object does not fit the metric type; a metric type that is not registered, is abstract,
or does not derive from the metric base; a subject type that the metric does not admit; a metric with `enforcement`
`direct`; a Quota write whose metadata the Engine rejects, with the Engine's list in the answer, and one that it
accepts; a Quota with non-empty metadata under the `most-restrictive-wins` Engine; a Policy change that leaves a Quota
with metadata the new Policy cannot read, and the evaluation that follows; a registration by another module that names
an existing metric in any of its fields and changes nothing.

## Pros and Cons of the Options

### (a) The metric is a type; the Engine checks Quota metadata

* Good, because QE reads every object by the id that the request or the Quota names.
* Good, because only the owner of the metric's name can change the metric.
* Good, because subjects and metrics follow the model of license-resolver, and the Engine has the role of its
  evaluator.
* Good, because a metric needs one registered object.
* Bad, because the form of Quota metadata is checked only by the Engine, and a Policy change can leave Quotas that the
  new Policy cannot read.
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
[README](../../../license-resolver/license-resolver-sdk/README.md). Conditions of a grant and their evaluator in
license-enforcement: [DESIGN](../../../license-enforcement/docs/DESIGN.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-projection-contracts`
* `cpt-cf-quota-enforcement-fr-contract-validation`
* `cpt-cf-quota-enforcement-fr-subject-type-registry`
* `cpt-cf-quota-enforcement-fr-quota-metadata`
