---
description: "ADR of the Settings Service: activation is acknowledged per gear instance and the wait for confirmations is bounded by a report timeout that closes an unfinished activation as incomplete."
status: accepted
date: 2026-11-08
decision-makers: Constructor Fabric Steering Committee
---

# ADR-004: Activation Is Acknowledged per Instance, Waited for Within a Bound

**ID**: `cpt-cf-settings-service-adr-per-instance-acknowledgement-bounded-wait`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Per-instance subscriptions and reports with a report timeout](#per-instance-subscriptions-and-reports-with-a-report-timeout)
  - [Per-instance with an unbounded wait and an admin-initiated close](#per-instance-with-an-unbounded-wait-and-an-admin-initiated-close)
  - [Per-gear state with an SDK self-check](#per-gear-state-with-an-sdk-self-check)
  - [Per-instance with a heartbeat or lease](#per-instance-with-a-heartbeat-or-lease)
  - [Counter of subscribed instances without identity](#counter-of-subscribed-instances-without-identity)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Before this decision the activation state was kept per gear: one row per subscriber, key and tenant (DESIGN-activation §4.1). The first report settles the row and later reports are ignored (§4.2 *Activation Outcome Recorder*), and the wait is unbounded (`cpt-cf-settings-service-principle-unbounded-wait`, `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`).

For a gear that runs several instances this has four consequences. A `succeeded` means only that at least one instance applied the value. A live instance that missed the notification after another one reported is never caught up. A `failed` from a slower instance is masked by an earlier `succeeded`. An instance that died without deregistering leaves nothing visible. The administrator therefore cannot tell "all replicas run the value" from "one does".

`cpt-cf-settings-service-fr-consumer-activation` now requires the account to be kept per instance, with a bounded wait. How should the activation state be shaped so that it answers whether the whole gear runs the value, without a dead instance holding an account open forever?

The bound and the per-instance account are one decision, not two. Per-instance accounting is unusable without a bound: one dead instance would hold every account open forever. The option "per-instance with an unbounded wait and an admin-initiated close" was considered and rejected: after every ungraceful death an operator would have to act, and until then the account is wrong.

The decision rests on one premise: every instance of a gear receives each notification through its own consumer group, so a notification reaches all instances, not one of them.

## Decision Drivers

* The administrator's question is "does the whole gear run the value", not "did some instance run it".
* A dead instance must not keep an account open forever, nor make every later change look unfinished.
* No heartbeat or lease traffic between changes: the delivery loop re-publishes a notification until the change is confirmed or closed, and the absence of a report after that many accepted redeliveries is the only signal needed, at the only time it matters.
* The thin-event and pull protocol of ADR-003 stays unchanged.
* The Event Broker exposes no consumer-group membership through its API, so liveness cannot be read from it.
* The existing model (subscription, activation state, supersession, delivery loop) is kept, extended with the instance dimension.

## Considered Options

* **Per-instance subscriptions and reports with a report timeout** — every SDK process registers under an instance id; a change is complete when all expected instances confirmed it, and closes as `incomplete` after a bound.
* **Per-instance with an unbounded wait and an admin-initiated close** — per-instance accounts as above, but an operator closes an unfinished change by hand.
* **Per-gear state with an SDK self-check** — keep one row per gear and let the SDK periodically verify its own value.
* **Per-instance with a heartbeat or lease** — instances renew a lease; the expected set is the instances holding a live lease.
* **Counter of subscribed instances without identity** — keep per-gear state and compare reports against a count of registrations.

## Decision Outcome

Chosen option: **"Per-instance subscriptions and reports with a report timeout"**, because it is the only option that tells the administrator which instances run the value and which do not, and that recovers from an ungraceful instance death with no operator action, using only the signals the delivery loop already produces.

This ADR **refines** ADR-003 (`cpt-cf-settings-service-adr-thin-event-pull-report-activation`): the thin event and the pull are unchanged; registration and report gain an instance id; the "open until reported" consequence is replaced by the bounded wait.

The decided rules (the detailed rules are in `cpt-cf-settings-service-fr-consumer-activation`):

1. Every SDK process registers with an **instance id** it generates at start (a UUID), with an optional human-readable label taken from the host name. Registration and reports carry the instance id.
2. The account is kept per current change, per subscriber, at each tenant where the change affected the effective value: the target scope and, for a cascading change, each affected descendant. Its **expected set** is the instances registered for the key at that tenant when the change was made, less the silent ones (rule 4). An instance that registers later is not in that account, since it reads the value when it registers. The change is `succeeded` only when every expected instance confirmed, and `failed` as soon as any instance reports a failure, also after the timeout.
3. The wait is bounded by a configurable **report timeout** (default 10 minutes). It is counted in accepted redeliveries: K = ceil(timeout / redelivery interval), so an instance is called silent only after K deliveries the broker accepted without a report. This is exactly why broker downtime does not count: no delivery is accepted, so none is counted. The configuration refuses a timeout below twice the interval at init. When the bound passes with confirmations outstanding, the change closes as `incomplete`, naming the silent instances; it is neither discarded nor counted as failed.
4. Silence is kept per registered interest (instance and key). A **silent instance** is excluded from the expected set of later changes only while the subscriber has at least one non-silent instance at that tenant. When every instance of a subscriber at that tenant is silent, all of them are expected again and notified; if they stay silent the change closes `incomplete` listing all of them ("0 of N"). The expected set is therefore never empty for a registered subscriber, and a gear with no registration has no account.
5. A silent instance still receives the gear's notifications of later changes; that is how it re-enters. Any report by an instance for the key, a stale one included, and any new registration, ends its silence. A report that is not accounted for the current change, or a new registration, also re-arms the instance's own silent row of the current change, if it has one: delivery to it resumes and the change reads as waiting again, unless another instance keeps it failed or incomplete, until the instance confirms or is called silent anew. An instance silent for **a day** is forgotten: that one interest is removed. The day runs from the moment the interest was first called silent — being called silent again on a later change does not restart it — and only a report or a registration of that instance for that setting restarts it; it is unaffected by broker downtime. Forgetting alters no recorded account: the history keeps every report and every call of silence; the state of the current value is re-derived from the instances still registered.
6. A report naming a superseded change is accounted only when it carries the value currently expected (the instance runs the current value, so it confirms); otherwise it is ignored rather than failed (ADR-003 rule). Either way it ends the instance's silence (rule 5), and it changes no past account; when it is ignored it re-arms the instance's silent row of the current change, if it has one (rule 5), so the current change is delivered to that instance again. A **late report** for the current change is accounted, also on a silent instance: a confirmation can complete an `incomplete` change as `succeeded`, a failure makes it `failed`.
7. Delivery of a change stops when every expected instance has reported or been called silent; a failure reported by one instance makes the change `failed` but stops neither the delivery to the others nor the silence watch, so silent instances are still named. Redelivery also reaches instances that already confirmed, which is harmless because reports are idempotent.
8. On the gear's shutdown the SDK deregisters its own instance's interests, best effort.

### Consequences

* `succeeded` now means every expected instance confirmed the value; the previous meaning, one report from the gear, is gone.
* The change outcome is one of `awaiting`, `succeeded`, `failed`, `incomplete`; the per-instance state is one of `awaiting`, `succeeded`, `failed`, `silent`. `incomplete` is closed but not final: a late report can still change it, and an instance's repeat of a report already accounted changes no account.
* The activation read shows "N of M" with a per-instance list: `succeeded`, `failed`, `awaiting`, or `silent` since a given time.
* Forgetting alters no recorded account: the history keeps every report and every call of silence; the state of the current value is re-derived from the instances still registered, so forgetting the last silent instance of an `incomplete` change can make it read `succeeded`. Unsubscribing an instance, by hand or at a graceful stop, has the same effect on the current value.
* This ADR supersedes `cpt-cf-settings-service-principle-unbounded-wait` and `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`. DESIGN-activation was amended for every statement of the unbounded wait and of the per-gear row, including the two statements, the "first report settles the row" rule of the Activation Outcome Recorder, the open item "Replica registration (OPEN)" in §6.2 (resolved here), and the redelivery-interval description that calls the wait unbounded. Downstream artifacts citing the superseded ids (`features/consumer-activation.md`, `features/activation-read.md`, `DECOMPOSITION.md`) follow the same amendment.
* The SDK gains an instance id and label in its registration and reports. This is a major bump of `cpt-cf-settings-service-contract-consumer-activation` and changes the SDK's activation contract within the unreleased 0.3.0 (0.2.1 is the last released version), which is acceptable because it has no external consumer yet.
* A gear with several processes unsubscribes the key only when its last process does.
* Dead-instance cost: on an ungraceful death (kill, OOM) the instance cannot deregister, so a ghost registration remains. The first change after the death is `incomplete` after a full timeout; later changes are not held up, because the ghost is silent and excluded. This cost is accepted. The mitigation is the best-effort deregistration on graceful stop (rule 8), so ghosts arise only from ungraceful deaths.
* Not detectable: a live instance whose handler is stuck looks the same as a dead one. This is accepted. The account shows it silent, and finding the cause is the operator's task.
* Security: instance ids are trusted within a gear's authenticated identity. Instances of one gear cannot be told apart cryptographically, so a report claiming a sibling instance's id is accepted; this limitation is accepted. Across gears the identity check holds: a gear cannot register or report for another gear. Instance identities (id, host label) are visible to the platform administrator only; the administrator who made the change reads the account within that boundary, a tenant administrator sees counts and states.
* Data and scale: the tables are created by the same unmerged change, so there is no data migration. Rows multiply by the number of instances, and each ungraceful restart leaves one ghost registration per key and tenant. A ghost a change asked is called silent after the report timeout and forgotten a day later; a ghost never asked lives until a change asks it (then a day) or until a cap of 64 registrations per gear, key and tenant evicts it, oldest silent or unasked first. A live instance removed this way re-enters on its next report, which is answered as not subscribed so that the SDK registers it again.
* Operations: the configuration must satisfy timeout ≥ 2 × redelivery interval, checked at init. The activation read exposes the per-instance state and the N-of-M counts the operator monitors.
* Reversibility: moving later to a lease (option 4) only changes how the expected set is computed; the per-instance accounting stays.
* Compliance and user experience: not applicable beyond the identity visibility above; the account is operational data, and the administrator's view gains the N-of-M counts without a new workflow.

### Confirmation

* Integration test: a gear with two instances where both confirm yields `succeeded` and an activation read of "2 of 2".
* Integration test: two instances where one is silent; after the report timeout the change is `incomplete` and names the silent instance, and the next change expects only the other instance.
* Integration test: a failure from one instance fails the change even when the other confirmed first.
* Integration test: a late confirmation from the last silent instance after `incomplete` is accounted and completes the change as `succeeded`; a late failure makes it `failed`.
* Integration test: a broker outage is not counted: no instance is called silent for time during which nothing could be delivered.
* Integration test: an instance that registers after a change is outside that change's account.
* Integration test: a silent instance re-enters by a report for the key (a stale report included) and by a new registration.
* Integration test: a not-accounted report or a registration re-arms the instance's silent row of the current change; delivery resumes.
* Integration test: an instance silent for a day is forgotten and its registration removed; the history keeps its reports and calls of silence, and the state of the current value is re-derived from the instances still registered.
* Integration test (zero expected): every instance of a subscriber silent at a tenant; the next change expects and notifies all of them, and if they stay silent closes `incomplete` listing all, "0 of N".
* Integration test (supersession): a report naming a superseded change is ignored for the account but ends that instance's silence.
* Integration test: a repeat report from an instance changes nothing.
* Integration test: a cascading change affecting a descendant tenant keeps a separate account per affected tenant.
* Integration test: an instance is called silent after exactly K accepted deliveries without a report, not before.
* Integration test: a failure from one instance does not stop delivery to the others, and their silence is still recorded.
* Integration test: on graceful stop the SDK deregisters its instance's interests; a gear with two processes keeps the subscription until the last one stops.
* Contract test: the activation read exposes the per-instance list and the N-of-M counts; the identities appear for the platform administrator only and not for a tenant administrator.
* Contract test: a report or unsubscribe from another gear has no effect on that gear's account (the report is answered `not_subscribed` or `ignored`, the unsubscribe `404`).
* Configuration test: a report timeout below twice the redelivery interval is refused at init.
* Review: any proposal to settle an activation on a single report of a multi-instance gear amends this ADR.

## Pros and Cons of the Options

### Per-instance subscriptions and reports with a report timeout

* Good: `succeeded` means what the administrator assumes it means.
* Good: a failure or a silent instance cannot be masked by another instance's report.
* Good: no traffic between changes, and no operator action after an ungraceful death.
* Good: keeps the model and the ADR-003 protocol, extended with the instance dimension.
* Bad: a registration per process, and a per-instance row set to maintain and read.
* Bad: a stuck-but-alive instance is indistinguishable from a dead one.
* Bad: an instance that is only slow, past the timeout, makes a change `incomplete` until it reports.
* Bad: after an ungraceful death the first change is `incomplete` after a full timeout; graceful stop avoids it.
* Bad: sibling instances of one gear cannot be told apart cryptographically.

### Per-instance with an unbounded wait and an admin-initiated close

* Good: no timeout to configure and no judgement of silence by the service.
* Bad: after every ungraceful death an operator has to act, and until then the account is wrong.
* Bad: one dead instance holds every later account open, which is the failure the bound exists to prevent.

### Per-gear state with an SDK self-check

The SDK periodically compares its applied value with the effective value and re-applies on a mismatch.

* Good: cheap, and catches a replica that missed a signal.
* Bad: the server still cannot say which instance applied what.
* Bad: a dead instance stays invisible, and a failure from a slower instance is still masked.
* Bad: periodic reads from every instance for a question the service already knows how to ask at change time.

### Per-instance with a heartbeat or lease

The Kubernetes Lease pattern: every instance renews a lease; the expected set is the holders of a live lease.

* Good: exact liveness within seconds.
* Bad: a periodic call from every instance between changes, for a question that is only asked at change time.
* Bad: a second mechanism to keep alive, with its own failure modes (clock skew, a lease renewal path that is up while the handler is not).
* Neutral: a lease does not detect a stuck handler either.

### Counter of subscribed instances without identity

* Good: smallest change to the state model.
* Bad: breaks after the first ungraceful death: the count can never be corrected, so every later change shows N−1 of N forever.
* Bad: a repeated report from one instance is indistinguishable from a report of another.

## More Information

The delivery loop re-publishes a notification for an outstanding change every redelivery interval (DESIGN-activation §4.2). The report timeout is counted in those accepted redeliveries and must be at least twice the interval, so an instance gets at least two accepted deliveries before it is called silent. Per-instance accounting builds on ADR-003: the thin event and the pull are unchanged; the report now names who made it.

A later move to a lease (option 4) stays possible if a need arises to learn of a dead instance before a change is made.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-settings-service-fr-consumer-activation` — per-instance accounting, N-of-M confirmation, the report timeout, `incomplete`, and the exclusion and forgetting of silent instances.
* `cpt-cf-settings-service-contract-consumer-activation` — major bump: registration and report carry the instance id.
* `cpt-cf-settings-service-component-consumer-activation-sdk` — generates the instance id, registers per instance, deregisters on graceful stop.
* `cpt-cf-settings-service-actor-platform-admin` — reads the account including instance identities.
* `cpt-cf-settings-service-actor-tenant-admin` — reads counts and states, without identities.
* `cpt-cf-settings-service-principle-unbounded-wait` — superseded by this decision; the id and heading are retained in DESIGN-activation with the new text, so its citers stay valid.
* `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded` — superseded by this decision; retained the same way.
* `cpt-cf-settings-service-adr-thin-event-pull-report-activation` — ADR-003, refined by this decision: thin event and pull unchanged.
* [DESIGN-activation.md](../DESIGN-activation.md) §4.1 (ActivationState, Subscription), §4.2 (Activation Outcome Recorder), §6.2 "Replica registration (OPEN)" (resolved by this decision), §6 (unbounded wait) — amended with this decision.
