---
status: accepted
date: 2026-11-20
---

# Metric Catalogue, Filled While Running

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) One catalogue built at bootstrap](#a-one-catalogue-built-at-bootstrap)
  - [(b) One entry per metric, filled while running](#b-one-entry-per-metric-filled-while-running)
  - [(c) Whole-catalogue refresh that keeps the last valid one](#c-whole-catalogue-refresh-that-keeps-the-last-valid-one)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-contract-catalogue`

## Context and Problem Statement

A metric is a GTS type that its owning module registers in types-registry
([ADR-0013](0013-cpt-cf-quota-enforcement-adr-metric-types.md)). The type holds the schema of the metric object of a
request and the traits `kind`, `admitted_subjects`, and `enforcement`. QE keeps what it has read in a process-local
**catalogue**: for each metric, the resolved schema and the traits. Other modules register the metric types, and
modules start in any order.

[ADR-0007](0007-cpt-cf-quota-enforcement-adr-projection-contracts.md) builds the catalogue once at bootstrap, keeps it
unchanged for the life of the process, and fails bootstrap on any inconsistency. A module that registers its metric
after QE has started is then not served until QE restarts, and one module's wrong registration stops QE for every
module. When does QE read a metric type, and what does one wrong metric type stop?

## Decision Drivers

* Modules start in any order; a new metric needs no QE restart.
* A wrong metric type of one module does not stop the quotas of other modules.
* A metric with a wrong type never gets an allowed decision.
* Every replica reaches the same catalogue, also after a restart.
* The evaluation path makes no registry call for a metric it already serves.

## Considered Options

* (a) One catalogue built at bootstrap; any inconsistency fails bootstrap.
* (b) One entry per metric, filled while running; an inconsistency makes only its metric unavailable.
* (c) A whole-catalogue refresh that keeps the last valid catalogue when the new one is inconsistent.

## Decision Outcome

Chosen option: (b).

* The catalogue holds one entry per metric type that QE has read. An entry is **served** or **unavailable**.
* An entry is unavailable when the type is not a valid metric type (it is abstract, it does not derive from the
  metric base, its own version is major 0, or a trait is wrong, for example an admitted subject type that is not a
  quota subject type). Nothing else makes an entry unavailable. A request or a write never reaches the check of the
  metric base: its id fails the check of the id form first and is `InvalidArgument`. That check applies only to a
  metric type that a stored Quota or Policy names, which QE reads at start.
* QE does not check a metric type against stored Quotas or stored Policies. ADR-0007 made that check once, at
  bootstrap, against a catalogue that the operator configured; a type that its owner can revise at any time has no
  such moment.
* `admitted_subjects` is checked when a Quota is created and when a request names a subject; a later change of such
  a Quota, for example of its cap, is not checked against it. A revision that stops
  admitting a subject type leaves the Quotas of that type stored. A request that names the type is rejected as
  invalid, so those Quotas match no request and nothing is allowed without a limit. They can be deactivated, and
  they match requests again when the type is admitted again.
* A Policy is checked against the schema of its metric type when the Policy is written, and each Policy version keeps
  the schema it was checked against. When a later revision of the type and an active Policy do not fit,
  the Engine reports an error at evaluation: QE rejects the request with `POLICY_EVALUATION_FAILED` (`FailedPrecondition`),
  changes no counter, and counts the rejection in telemetry. The operator then writes a Policy that fits the current schema.
* QE reads a metric type from the registry by its id. It keeps no listing and no index: a request and a Quota name
  the metric type, and no other registered object decides how a metric is served.
* QE reads through the platform-plane contract of types-registry, `PlatformTypesRegistryApi`, as other platform gears
  do. That contract has the conditional read; the older `TypesRegistryClient` does not. Every read asks the registry
  itself, not the cached copy of the registry client.
* The catalogue does not use the cache that types-registry designs for its SDK client
  (`cpt-cf-types-registry-tech-sdk-cache`), for two reasons. That cache is not implemented. And by its design it
  confirms an entry with the registry on the request path after a freshness window and fails the read when the
  registry does not answer, while QE must keep serving a metric it holds (below). The catalogue therefore has two
  parts. The entry that QE builds from a type document, with the compiled schema, the traits, and the served or
  unavailable state, stays in QE in any case. The storing of documents, the conditional reads, and the schedule are
  replaced by the SDK cache when it exists and can return the last content it holds while the registry does not
  answer.
* The registry returns a deleted type with a deleted status, not as an unknown type, and a deleted id cannot be
  registered again unless an operator purges it. QE treats a type with that status as deleted.
* Before any registry read QE checks the form of the id. An id that the `gts` library does not parse as a GTS type
  id, a metric type id that does not start with the id of the
  metric base, or a subject type id that does not start with the id of the subject base, is `InvalidArgument` with no
  registry call. A request that names several types that QE does not hold reads them with one batch read. The memory of unknown ids has a size limit and drops the entry that was used least recently. The
  catalogue and the cache of registered subject types hold only types that the registry holds, so a caller cannot
  grow them, and they have no limit: an entry that QE serves is never dropped and read again on the request path.
* QE reads at start the metric types of the active Quotas and Policies. It reads any other metric type when a request
  or a Quota or Policy write names it for the first time. That read has a time limit. A type that the registry does
  not know is `InvalidArgument`, and QE remembers the answer for a short time, so repeated requests for an unknown
  metric do not call the registry each time. A read that fails or reaches its time limit is `ServiceUnavailable`,
  which a caller may repeat. Requests that name the same type that QE does not hold make one read, not one each, and reads of different types do not
  wait for each other.
  After a read on first use that failed, the next ones fail at once with `ServiceUnavailable` for a short time, so
  a registry that does not answer costs one waiting request and not every request its time limit.
* A registered type can change. types-registry lets an owner replace the content of a type whose id has no minor
  version, such as `v1~`, under the same id (a **revision**), and delete a type. It admits a revision only when the
  new schema accepts every instance that the previous one accepted. It does not compare trait values, so a revision
  can change `enforcement` or `admitted_subjects`. Platform types under `gts.cf.*` must have ids without a minor
  version, so their owners can always revise them.
* types-registry does not apply that rule to a type whose own version is major 0: it admits any revision of such a
  type, a narrower schema included. QE therefore serves only a metric type whose own version is major 1 or higher.
* QE therefore reads again on a bounded schedule. A scheduled read asks the registry, for each entry and for each
  registered subject type that QE caches, whether the type changed since QE read it. It uses a conditional read, which
  returns a document only when it changed. The read builds an entry again when its type changed, removes the entry of a
  deleted metric type, and drops a deleted subject type. It reads nothing from storage.
* A request whose metric object does not fit the schema of a served entry makes QE read that metric type again at
  once, with a conditional read, before it rejects the request: the owner may have revised the type since the last
  scheduled read, and a caller does not repeat an `InvalidArgument`. When the type changed, QE builds the entry again
  and checks the request against it. QE makes this read at most once per metric in the time it remembers an unknown
  id, so requests that do not fit cannot become a stream of registry reads. A read that fails changes nothing, and
  the request is rejected by the entry that QE holds.
* A scheduled read that fails or reaches its time limit changes nothing: a served entry stays served, and an
  unavailable entry stays unavailable. A failure of the registry therefore never stops a metric that QE serves, and
  a revision then takes effect later than one read interval. QE reports this: it counts the registry reads that
  fail, by the kind of read (at start, on first use, scheduled), it reports the time of the last scheduled read
  that succeeded, and it reports the number of unavailable entries that a replica holds now.
* An entry is built from the current registry content only, never from stored state and never from the entry it
  replaces. So a running replica and a restarted replica build the same entry, and a revision takes effect on every
  replica within one read interval. QE has no rule that compares a revision with the content before it, because a
  restarted replica does not know that content.
* A request for a served metric uses the entry and makes no registry call, except for the read above when its metric
  object does not fit the entry's schema. A request for an unavailable metric fails
  with `FailedPrecondition` and changes nothing; it is never an allowed or a denied decision.
* An unavailable metric stops the operations that QE checks against the metric type: debit, lease acquisition, batch
  debit, preview, and the snapshot read. A snapshot read that names several metrics fails when one of them is
  unavailable. Credit, lease commit, lease release, return, and rollback name an existing Quota, lease, or charge;
  they run while the metric is unavailable, and also after a revision changed `enforcement` to `direct`, so held
  capacity can always be given back.
* Quota and Policy writes use the same entry. A Quota create or change and a Policy create or change for a metric
  with no served entry are rejected, because QE cannot check the write against a type that is not valid. A Quota
  deactivation, a Policy rollback, and a Policy delete make no catalogue check and always run.
* An admitted subject type is the quota subject base or a type derived from it; QE checks that a subject type is
  registered when a request names it (the cache of registered subject types).
* A wrong metric type does not fail bootstrap and does not change readiness. The start itself needs types-registry:
  the platform does not start the process until the registry has admitted the GTS types that QE publishes, and
  bootstrap finds the storage plugin and the notification sinks through it. The read at start is one more such
  step: when it fails or reaches its time limit, the bootstrap attempt fails, the replica stays not ready, and QE
  repeats bootstrap, with a growing pause, until the registry answers. Nothing restarts a gear whose start has
  failed, so QE does not give up on a dependency that does not answer. So a
  replica never takes requests with an empty catalogue, and a rolling update stops while the registry does not
  answer, instead of replacing replicas that hold their catalogue with replicas that hold none.
* After the start, readiness does not depend on types-registry. While the registry does not answer, a replica
  serves the metrics it holds and every operation that needs no entry, and a request for a metric it does not hold
  is `ServiceUnavailable`.
* QE reports each unavailable metric with its reason in a log entry and in telemetry.

### Consequences

* A new metric is served on its first request on every replica, with no restart.
* A wrong metric type stops only its metric, on every replica and after every restart.
* An operator learns of a wrong metric type from telemetry, not from a failed start.
* The first request for a metric on a replica makes one registry read. Later requests make none.
* A revision of a served metric type, a change of `enforcement` or of `admitted_subjects` included, takes effect on
  every replica within one read interval. A revision that makes the schema accept more takes effect on a replica
  with the first request that needs it. A revision never makes a metric unavailable because of a stored Quota or
  Policy. An unavailable metric is served again when its owner revises the type into a valid metric type.
* QE cannot reject a revision: types-registry admits it without asking QE. types-registry plans a contract through
  which an owning gear checks a revision before it is admitted (a Validation Hook in its PRD). Until that contract
  exists, QE applies a changed trait to new writes and new requests only.
* A deleted metric type removes its metric from the catalogue: a new request for the metric gets `InvalidArgument`,
  and commit, release, return, and rollback still run.
* This replaces, in ADR-0007, the catalogue that is built once, the bootstrap failure on a contract inconsistency,
  and the check of the catalogue against stored Quotas.

### Confirmation

Tests: a metric registered after start is served on its first request without a restart; a metric whose type is
invalid is unavailable while other metrics are served; a metric type whose
own version is major 0 is unavailable; a request for a metric
that is not registered, twice inside the short time QE remembers the answer; a registry that does not answer on the
first request for a metric, and for a served metric; a revision of a served metric type is served by a running
replica within one read interval, with the same entry as on a restarted replica; a request that fits only the
revised schema is accepted before the scheduled read, and a second read of the same metric in the remember time is
not made; a revision that stops
admitting the subject type of an active Quota leaves the metric served, and a request that names that type is
rejected; a revision that an active Policy does not fit leaves the metric served, and a request that the Policy
cannot evaluate is rejected with no counter change; an unavailable metric is served after its type becomes valid; a deleted
metric type and a deleted subject type; commit, release, return, and rollback of a lease or charge whose metric is
unavailable; a snapshot read that names an unavailable metric; a Quota and a Policy write for a metric with no served
entry; a Quota deactivation, a Policy rollback, and a Policy delete for an unavailable metric; bootstrap with a wrong metric type; a start while the registry does not answer fails; requests that miss the same
metric together make one read, while a read of another type does not wait for it; after a failed read on first use the next one fails at once and a later one reads
again.

## Pros and Cons of the Options

### (a) One catalogue built at bootstrap

* Good, because the catalogue never changes under a running request.
* Bad, because a metric registered after start waits for a restart, and one wrong metric type stops QE for all
  modules.

### (b) One entry per metric, filled while running

* Good, because modules start in any order and a wrong metric type stops only its metric.
* Good, because an entry is built from the current registry content only, so a running and a
  restarted replica build the same entry.
* Bad, because a wrong metric type no longer shows as a failed start; it needs an alert on telemetry.
* Bad, because the first request for a metric depends on the registry.

### (c) Whole-catalogue refresh that keeps the last valid one

* Good, because a served metric keeps working when its type becomes wrong.
* Bad, because a restarted replica has no last valid catalogue, so replicas disagree after a restart.

## More Information

Metric types and their traits: [ADR-0013](0013-cpt-cf-quota-enforcement-adr-metric-types.md). Subject types and
`admitted_subjects`: [ADR-0008](0008-cpt-cf-quota-enforcement-adr-quota-resolver-subjects.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-contract-validation`
* `cpt-cf-quota-enforcement-fr-subject-type-registry`
