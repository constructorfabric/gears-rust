---
description: "Why a quota metric is a GTS type with traits and a metadata schema, and why a request carries no separate resource."
status: proposed
date: 2026-11-23
---

# ADR-0004: A Metric Is a Type; a Request Has No Separate Resource

**ID**: `cpt-cf-quota-resolver-adr-metric-types`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) A metric type with traits and metadata](#a-a-metric-type-with-traits-and-metadata)
  - [(b) A metric instance, operation metadata, and a resource](#b-a-metric-instance-operation-metadata-and-a-resource)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A request to the resolver named three things about what it charges: a metric, which was an instance of one metric
type with the single field `kind`; operation metadata for that metric; and an optional resource with its own type,
id, and metadata. The resolver passed all three to the backend unchecked.

The backend then needed more registered objects to check them: a schema for the operation metadata, a place for the
subject types that a metric admits, and a list of resource types. The operation metadata and the resource metadata
were two sets of attributes of one operation, and both were read only by the backend's policies.

license-resolver names what it checks as one object: a resource `{type, id?, metadata}`, whose type holds the metadata
schema and the admitted subjects. What does a request name, and where do the properties of a metric live?

## Decision Drivers

* A request names every object that a backend must read to check it.
* A metric has one registered object, owned by one module.
* The resolver stays a pass-through: it validates no metric and no metadata.
* Quota subjects and metrics follow the model of license-resolver.

## Considered Options

* (a) A metric is a type derived from a metric base; the type has traits and a metadata schema; a request has no
  resource.
* (b) Keep the metric instance, the operation metadata, and the optional resource.

## Decision Outcome

Chosen option: (a).

* `gts.cf.core.quota.metric.v1~` is the abstract **metric base**. A metric is a type derived from it. The module that
  owns the metric registers the type, named under that module.
* A request names a metric as `{type, id?, metadata}`: the metric type; an optional id of the one thing the amount is
  counted for, for example a model or a virtual machine; and the attributes of the operation that the backend's
  policies read. The metric type refines the schema of `metadata`.
* The metric base declares three traits that a metric type sets:
  * `kind`: `counter` or `gauge`;
  * `admitted_subjects`: the quota subject types the metric's quotas may count. A type derived from an admitted type
    is admitted, and the tenant subject type is always admitted. The list guards against a wrong configuration when
    the owner knows the metric's kinds of subject; an owner that does not know them admits the subject base
    `gts.cf.core.quota.subj.v1~`, which admits every subject type;
  * `enforcement`: `quota_gated` (the default) or `direct`. A `direct` metric is not checked against quotas: the backend rejects the operations that take capacity on it (debit, lease acquisition), and commit, release, and return still run.
* A **metric item** of a request is a metric object and its amount. A request has no separate operation metadata and
  no resource. The resource base `gts.cf.core.quota.res.v1~` is removed.
* The resolver passes a metric object to the backend as it is. The backend reads the metric type, checks the object
  against it, checks the subject types against `admitted_subjects`, and applies `kind` and `enforcement`.
* Another major version of a metric type is another metric. The own version of a metric type is major 1 or higher:
  types-registry admits any revision of a type of major 0, a narrower schema included, so a backend may refuse such
  a type.

### Consequences

* A module registers one type per metric and no resource type.
* A request with several metric items repeats in each item the attributes that are common to them.
* Whether the metric `id` selects a quota is the backend's decision. quota-enforcement passes it to its policies and
  does not use it in the counter key.
* The contract of the SDK is `unstable`, so this change needs no new major version.

### Confirmation

Contract tests: a metric object passes through the resolver unchanged; a request with no metric item, and with a
metric object that has no `metadata`, is rejected as malformed; the schemas of the metric base and of the example
metric types are valid GTS type schemas.

## Pros and Cons of the Options

### (a) A metric type with traits and metadata

* Good, because the backend reads everything about a metric from one type that the request names.
* Good, because only the owner of the metric's name can change how the metric is checked.
* Bad, because attributes common to several metrics of one request are repeated.

### (b) A metric instance, operation metadata, and a resource

* Good, because one resource describes a request with several metrics once.
* Bad, because the backend needs further registered objects to check the metadata and the admitted subjects, and a
  rule that ties each of them to its metric.

## More Information

Backend side: quota-enforcement
[ADR-0013](../../../quota-enforcement/docs/ADR/0013-cpt-cf-quota-enforcement-adr-metric-types.md). Subject and
resource contracts in license-resolver: [README](../../../license-resolver/license-resolver-sdk/README.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-quota-resolver-contract-backend-plugin` — the plugin contract carries metric objects and no resource.
* `cpt-cf-quota-resolver-fr-multi-metric` — a metric item is a metric object and its amount.
