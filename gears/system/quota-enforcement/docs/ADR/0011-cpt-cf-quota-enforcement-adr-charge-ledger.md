---
status: accepted
date: 2026-11-18
---

# Charge Ledger for Returns

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Return](#return)
  - [Retention](#retention)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) A charge ledger](#a-a-charge-ledger)
  - [(b) Longer idempotency retention](#b-longer-idempotency-retention)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-charge-ledger`

## Context and Problem Statement

The quota resolver requires a **return**: a module gives back part of a **charge** (an allowed debit or a lease
commit), named by the charge's operation and idempotency key
([PRD](../../../quota-resolver/docs/PRD.md), `cpt-cf-quota-resolver-fr-return`). An allocation charge must stay
returnable until all of it is returned; a consumption charge for a window that the backend states.

A record of what a charge touched must therefore outlive the **replay window**: the time QE keeps idempotency records,
24 hours by default. It must be found by the tenant, the operation, and the key alone. Where does QE keep charges, and
how does a return apply?

## Decision Drivers

* A return needs only the tenant, the charge's operation and key, and the amount.
* An allocation charge is returnable for as long as it holds an amount on an active quota.
* A repeated return or rollback never gives back an amount twice.
* Storage grows only with charges that can still be returned.
* One rule says when two requests with one key are the same operation.

## Considered Options

* (a) A charge ledger, separate from idempotency records.
* (b) Keep charges in idempotency records with a longer retention.

## Decision Outcome

Chosen option: (a).

* Every allowed debit and every lease commit writes a **charge** in the same transaction as its counters: tenant,
  operation, idempotency key, request fingerprint, attribution digest, result, metric, amount, and per plan entry its
  chain, position, quota, period, charged amount, and returned amount. A charge is unique per
  `(tenant, operation, key)`.
* The **attribution digest** of a charge is a digest of the attribution that admission accepted for the charged
  operation: the tenant, the subjects, and the metric with its type and optional id. Its form is fixed, so that a
  rollback with the same attribution always gives the same digest: the tenant id; the subjects without the tenant
  subject, which the tenant id already covers, ordered by subject type id (a request has one subject per type); the
  metric type id; the metric id, or its absence. The metric `metadata` is not part of it. A lease takes the digest
  at its acquisition,
  and the charge of a commit takes it from its lease. A charge holds the digest and no subject
  identifier.
* A batch debit item writes a `debit` charge under the item's key. A batch is replayed only as a whole, by its own
  key: a batch with an item key that already names a record of a single debit, which is a `debit` charge or the
  idempotency record of a denied debit, is rejected as a whole as a payload mismatch and changes nothing. QE checks
  the item keys again in the transaction that writes the charges, so a single debit that commits first under an item
  key rejects the batch; a batch never gets the stored result of a single debit.
* The idempotency key of every operation is unique per `(tenant, operation, key)`. The subjects are part of the
  request fingerprint, not of the key's scope.
* The charge is the idempotency record of its debit or commit for as long as it lives. A repeated request with the
  same fingerprint gets the first result; with another fingerprint it fails as a payload mismatch.

### Return

* A return names the tenant, the charge's operation and key, the amount, and its own idempotency key. It unwinds the
  amount over the charge's entries (ADR-0010). The returned amount of each entry grows; the entry's quota takes the
  amount by its type:
    * allocation: the held amount decreases;
  * consumption: the counter of the charge's period decreases, inside the return window and while that period is not
    settled; otherwise the counter does not change;
    * rate: the counter does not change.
* A deactivated quota takes a return as an active one does: deactivation stops new debits and leases, not returns.
* A counter does not go below zero: an entry decreases it by the entry's amount, or to zero. The entry's returned
  amount grows by the whole amount, so repeated returns never exceed the charge.
* The return is all or nothing. It fails and changes nothing when the charge is unknown, the amount exceeds what
  remains of the charge, or no entry can take a part of the amount: every entry that the amount reaches is on a rate
  quota, or on a consumption quota outside the return window or in a settled period. Whether an entry can take an
  amount depends on the type of its quota and on the window, never on the current value of its counter: an allocation
  entry always takes a return, also when its counter is already at zero.
* The key of a return is unique per `(tenant, return, key)`, like the key of any operation; the charge it names and
  the amount are its fingerprint. QE keeps one **return record** per applied return: the key, the charge, and the
  amount. It is unique per tenant and key, and it is deleted with its charge. Two returns with one key never run at
  the same time: the second starts after the first has ended and finds its record (DESIGN, lock rank). A repeated
  return with the same charge and amount changes nothing and gets the first result; the same key with another charge
  or amount fails as a payload mismatch.
* Rollback returns everything that remains of a charge. It has no amount and no idempotency key of its own: on a
  charge with nothing left it succeeds and changes nothing.
* Rollback is an operation of QE's own API, where the PDP limits what a caller may charge. So a rollback names the
  charge and also carries the attribution of the charged operation. The PDP authorizes that attribution, for the action
  `rollback`, as it authorizes a debit's attribution for the action of a debit, and QE compares its digest with the
  charge's attribution digest. A charge with another digest gets the answer of an unknown charge. A caller can
  therefore roll back only a charge for whose attribution it has a `rollback` grant; knowing the key of a charge is
  not enough.
* A return comes only through the resolver plugin and names only the charge. That path is trusted for the whole tenant
  (ADR-0009), so a return compares no digest.

### Retention

* The **return window** of a charge is the replay window, counted from the charge's creation.
* A window has the length that is configured when its record is written: a charge takes its return window at its
  creation, and the time it is kept after a return or rollback when that one is applied. A later change of the
  configuration does not move them.
* An allocation entry **holds an amount** while its returned amount is less than its charged amount.
* A charge is deleted when the replay window has passed since its creation and since the last return or rollback
  applied to it, and no allocation entry of it holds an amount on an active quota. So a repeated return or rollback
  finds its charge for one replay window, like a repeat of any operation. From that moment the charge is unknown to
  every operation, whether or not the sweeper has removed it; a debit or commit under its key is a new operation and
  replaces it.
* An entry on a deactivated quota does not keep its charge. Deactivation is final, and a deactivated quota admits
  nothing, so its held amount limits nothing. The charges of a deactivated quota, and so of a tenant whose quotas
  are all deactivated, are deleted by the rule above.
* An allocation charge on an active quota has no time limit. An allocation lasts as long as its resource, and QE does
  not know when a resource is gone. So QE never frees a held amount because time has passed.
* QE does not compare held amounts with the resources that exist. When a module loses a return, for example because
  it deleted a resource and did not return its charge, the held amount stays. The operator then rolls the charge
  back on QE's API. A rollback returns what remains of the charge and records it in the charge, so a late return of
  the same charge fails with `RETURN_EXCEEDS_CHARGE`, or with `UNKNOWN_OPERATION` after the charge is deleted, and
  changes no counter. The operator does not use a credit for a lost return: a credit changes the counter and no
  charge, so a late return would lower the counter a second time.
* QE reports, per metric, the number of allocation charges that hold an amount on an active quota and are older than
  the replay window, and the age of the oldest one. A lost return shows there as a charge that never goes away.
* A background sweeper removes deleted charges in batches. Its delete carries the deletion rule itself, in the same
  statement, so it never removes a charge that replaced a deleted one under the same key.

### Consequences

* Storage holds one charge per debit or commit for the replay window, and one per allocation charge that still holds
  an amount on an active quota, plus one return record per applied return of a stored charge. DESIGN §4 states the
  capacity.
* A return of a charge whose allocation quotas are all deactivated fails as a return of an unknown charge once the
  replay window has passed. Nothing is lost: those quotas admit nothing.
* A lost return on an active quota is corrected by the operator, not by QE. Held capacity that no resource uses stays
  held until then.
* A consumption charge cannot be returned after the return window. Work whose result comes later holds a lease.
* A payload mismatch tells a caller that a key is in use in the tenant.
* A key is taken for the whole tenant. A caller that sends a key first takes it: another caller of the same tenant
  that sends the same key with another request gets a payload mismatch, which a retry does not pass, for as long as
  the charge lives, and an allocation charge on an active quota has no time limit. This is accepted. The callers are
  trusted platform services on the plugin path and callers that the PDP authorizes for the tenant on QE's API, and a
  module builds its keys from its own name, so it meets only its own keys. QE does not add the caller to the scope of
  a key: on the plugin path, in one process, the platform gives a caller no identity of its own.
* A rollback needs the attribution of the charged operation again. The caller of a debit or of a lease acquisition
  has it.

### Confirmation

Tests: partial and repeated returns, a repeated return after the replay window, a return after the return window,
after settlement, on a deactivated allocation and consumption quota, after a credit that took the counter to zero,
after a change of the configured replay window, on a rate quota alone and next to an allocation quota, above what
remains, of an unknown charge, rollback after a partial return and twice, rollback with another attribution, by a caller that the PDP does not
allow the attribution, and of a lease-commit charge, a repeat of the return that takes the last
amount of an old allocation charge, one return key on two charges, one after the other and at the same time, a batch
with an item key that names a charge or a denied debit, a
debit under the key of a deleted charge, one key with other subjects, deletion of charges, a charge whose allocation
quotas are all deactivated, before and after the replay window, a charge with one allocation entry on a deactivated
quota and one on an active quota, and a return after a credit that was given for the same charge.

## Pros and Cons of the Options

### (a) A charge ledger

* Good, because a return needs only the charge's name, and an allocation charge lives as long as it holds an amount on
  an active quota.
* Bad, because every allowed debit and commit writes one more record.
* Bad, because the charge of a lost return stays until the operator corrects it.

### (b) Longer idempotency retention

* Good, because no new table.
* Bad, because the records of all operations, also denied ones, get entries, returned amounts, and a life that
  depends on quota types.

## More Information

Quota resolver return: [PRD](../../../quota-resolver/docs/PRD.md), §5.1 Return.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-return`
* `cpt-cf-quota-enforcement-fr-rollback`
* `cpt-cf-quota-enforcement-fr-idempotency`
