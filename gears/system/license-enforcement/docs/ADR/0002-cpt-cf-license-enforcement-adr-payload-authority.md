---
status: proposed
date: 2026-09-12
---

# ADR-0002: Evaluation Reads Root Fields from the Plugin, the Store Is an Index

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Verify at import, trust rows](#a-verify-at-import-trust-rows)
  - [(b) Verify per check, read rows](#b-verify-per-check-read-rows)
  - [(c) Root fields from the plugin, rows as an index](#c-root-fields-from-the-plugin-rows-as-an-index)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-license-enforcement-adr-payload-authority`

## Context and Problem Statement

In an untrusted build, the operator can edit every row of the store. The PRD requires that every
input that the check reads comes from the signed payload, from a derivation of it, or from a holder decision that the
payload permits. It also requires that every evaluation verifies the evidence of the root again, against the trust
material that is current at that moment.

The evaluator is the same code under every admission plugin. Where does the evaluator take the fields of a root grant
from, and what is the role of the stored rows?

## Decision Drivers

* The operator edits rows. A stored column is not evidence (`cpt-cf-license-enforcement-fr-verification-fixed`).
* A revoked key, a rotated anchor, a lapsed TTL, or a new revocation entry must deny without a new import.
* The evaluator and the store schema are the same under every plugin
  (`cpt-cf-license-enforcement-adr-compile-time-admission`).
* In a trusted build, the store is the authority, as the PRD says.

## Considered Options

* (a) Verify at import, then trust the stored rows.
* (b) Verify the evidence at every check, and read grant fields from the stored rows.
* (c) At every check, take the root fields from `verify` of the linked plugin. Use the rows only to find candidates.

## Decision Outcome

Chosen option: **(c)**.

The evaluator first evaluates the candidates on the stored fields. For the first candidate that passes, the check
service calls `verify` of the linked plugin with the id and the evidence of its root. The plugin finds the root with
this id in the evidence. `verify` returns verified, a revocation state of the root, or a denial reason. A verified
result can add the evaluable fields of the root. These fields are resource, holder, subject type, binding
fixed at admission, validity, conditions, and permissions. A revocation state denies the candidate. When the candidate
is the root, the evaluator evaluates it again on these fields. When the candidate is a sub-grant, the check service
checks it against them (below). When the candidate fails, the check service continues with the next candidate. A
stored row can only make a candidate fail earlier, so a `verify` of other candidates adds nothing.

- A plugin that verifies evidence returns the fields from the verified evidence. Thus an edited root row can only cause
  a "not found" or a "no match".
- A plugin with a trusted store returns verified without root fields, and the evaluator uses the stored fields. Thus
  the store stays the authority in that binary.

If the evidence does not name the id of the root exactly once, the plugin has no fields to return and denies. Thus a
fabricated root row denies. The check service also checks that the store holds exactly one live row with the id of the
root. Thus a copied root row denies, and so does the original.

Sub-grants are derived. Delegation stores a sub-grant only if it is a subset of its parent. When `verify` returns
fields, the check service checks that a sub-grant candidate is a subset of the root fields. It also reads the chain of
the candidate and checks each link as the only live child whose parent binding names it. Thus a sub-grant row that is 
wider than the root denies, and a copied sub-grant row denies.

The check service does not check that each link of the chain is a subset of its parent. In an untrusted build, the
operator owns these rows (PRD threat model) and can edit all links together. Thus a check between links gives no
protection. An edit of a link moves a license only inside the root. In a trusted build, the store is trusted. There,
each grant has at most one live child, and each sub-grant is a subset of its parent.

The PRD forbids option (a), because the operator owns the store. Option (b) verifies the evidence but still evaluates
rows that the operator can edit.

### Consequences

* The plugin trait has a `verify` method that runs at every evaluation. How a plugin that verifies evidence keeps it
  within the latency budget is a decision of that plugin.
* The store schema for root grants is the same under every plugin. Under a plugin that verifies evidence, the root
  columns only find candidates.
* Holder decisions (acceptance, binding, delegation) stay in rows. The evaluator accepts them only where the fields
  from `verify` permit them.

### Confirmation

Tests with a plugin that verifies evidence:

* An edit of any column of a stored root row leaves the decision unchanged or changes it to a denial.
* A fabricated root row denies.
* A second row with the id of a root denies both rows as duplicated.
* A sub-grant row that is wider than the root denies.
* A copied sub-grant row denies.

## Pros and Cons of the Options

### (a) Verify at import, trust rows

* Good, because it gives the cheapest possible check path.
* Bad, because the operator can edit every stored column. After import, the signature protects nothing.
* Bad, because a new revocation list or trust bundle changes nothing until somebody imports again.

### (b) Verify per check, read rows

* Good, because trust material applies immediately.
* Bad, because the evaluated fields still come from rows that the operator can edit. The signature proves the
  evidence, not the row.

### (c) Root fields from the plugin, rows as an index

* Good, because the evaluated fields are the signed fields. Rows can only make the answer narrower or make it fail.
* Good, because one evaluator serves every plugin. Only `verify` differs.
* Neutral, because `verify` runs at every check for the selected candidate. Its cost is the concern of the plugin.

## More Information

DESIGN §3.1 contains the evaluation order and the reason vocabulary.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses these requirements and design elements:

* `cpt-cf-license-enforcement-fr-verification-fixed` — the evaluable root fields come from the verified payload, at
  every evaluation.
* `cpt-cf-license-enforcement-usecase-signed-onprem` — the alternative flows (edited payload, copied row, changed
  state) are the confirmation tests.
* `cpt-cf-license-enforcement-principle-index-not-authority` — this ADR is the rationale for that principle.
