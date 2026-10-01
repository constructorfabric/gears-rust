---
status: proposed
date: 2026-10-01
description: Each outcome category of a seed or uninstall maps to one HTTP status, and a partial failure is reported as an error, never as success
---
# Map Each Outcome Category to One HTTP Status, With Partial Failure Reported as an Error


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [One HTTP status per outcome category, partial failure reported as an error](#one-http-status-per-outcome-category-partial-failure-reported-as-an-error)
  - [Multi-Status (207) for partial success](#multi-status-207-for-partial-success)
  - [Success status (200) with per-section outcomes in the body](#success-status-200-with-per-section-outcomes-in-the-body)
  - [One generic error status for every failure](#one-generic-error-status-for-every-failure)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-outcome-http-mapping`

## Context and Problem Statement

Every seed and uninstall request ends in exactly one outcome category (`cpt-cf-application-manager-fr-outcome-categories`), chosen by a fixed precedence when several apply (`cpt-cf-application-manager-fr-outcome-precedence`). Because dispatch continues after failures ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), a request can end with some sections applied and others failed. How should each category, including a partial failure, be reported over HTTP so that the caller gets one clear answer per request?

## Decision Drivers

* Callers must tell "fix my manifest" from "ask for access" from "try again later" from "the platform failed", and the same situation must always give the same answer (`cpt-cf-application-manager-fr-outcome-categories`, `cpt-cf-application-manager-fr-outcome-precedence`).
* Failures must be actionable from the response alone (`cpt-cf-application-manager-fr-response-content`).
* The status mapping must fit the platform's standard error model and be usable by generic HTTP clients, deploy jobs and monitoring, which act on the status class.
* A partial failure must not be read as success by a client that looks only at the status.

## Considered Options

* One HTTP status per outcome category, partial failure reported as an error
* Multi-Status (207) for partial success
* Success status (200) with per-section outcomes in the body
* One generic error status for every failure

## Decision Outcome

Chosen option: "One HTTP status per outcome category, partial failure reported as an error", because it is the only option that lets a caller act on the status alone, keeps the fixed precedence visible in the status, and never lets a partial failure pass as success.

Every seed and uninstall request ends in exactly one outcome category, mapped as follows. The response always carries the category and the outcome of each section (or of each section plugin for uninstall), and the audit id whenever the audit plugin accepted the request hand-off (see [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)).

| Outcome category | HTTP status | Meaning |
|---|---|---|
| Succeeded | 200 | Every section, or every section plugin for uninstall, succeeded |
| Plugin rejected | 400 | At least one section plugin refused its section as unacceptable |
| Not permitted | 403 | Platform access control refused the caller before dispatch, or a section plugin refused for lack of an extra permission after dispatch |
| Not found | 404 | Uninstall named an unknown application; no section plugin was called |
| Invalid manifest | 422 | Validation failed; nothing was dispatched |
| Internal failure | 500 | A section plugin failed unexpectedly or exceeded the time limit, or the registry could not be updated after dispatch, or, after an accepted uninstall, the system could not delete the registry entry |
| Temporarily unavailable | 503 | Platform access control, registry storage, the audit plugin (request hand-off) or a schema could not be reached; nothing was dispatched; retryable |

Errors are returned in the platform's standard problem format. Invalid manifest is 422, kept apart from 400, so that "the manifest is malformed against the schemas" is never confused with "a section plugin refused a well-formed section". Requests the platform rejects before they reach the Application Manager (request protection, authentication, scope and license checks) get none of these outcomes. The gateway's own deadline-exceeded response is likewise outside the mapping of the gear and does not mean the request was abandoned: once the request hand-off is accepted, the request runs to its end, and its outcome is in the audit trail and in the registry listing.

Precedence is set by `cpt-cf-application-manager-fr-outcome-precedence` over the categories of `cpt-cf-application-manager-fr-outcome-categories`; this ADR gives only the status for each rank. Before dispatch: rank 1 (platform access control cannot answer, or registry storage cannot answer for a permitted caller) is 503, so a denied caller is 403 even when storage is down. Rank 2 is 403, rank 3 is 404, rank 4 is 503, rank 5 is 422. After dispatch: 403, then 400, then 500.

A partial failure is never reported as success: 200 means every section succeeded. The per-section outcomes in the body carry the detail. The registry records the same distinction ("partially failed" when at least one section succeeded and at least one failed) as decided in [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md).

Decision scope: this ADR covers how each outcome category is reported over HTTP, including partial failure, and what every response carries. It does not decide the categories or their precedence, which the PRD sets, when validation and dispatch run ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), the audit hand-offs and the audit id ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)), or the endpoints themselves, which the [DESIGN](../DESIGN.md) defines.

### Consequences

* The status mapping is part of the public contract. Adding a new outcome category or changing a status would supersede this ADR with a new one and would need an update of the [DESIGN](../DESIGN.md).
* When a section schema is temporarily unavailable, the report names the section key with a reason: `unreachable`, `timed_out` or `unusable`. For `unusable` (a schema that declares another dialect, uses an external reference, or does not compile), retrying helps only after the section plugin is fixed.
* Generic HTTP clients, deploy jobs and monitoring can treat any status that is not 2xx as a failure, so a partial failure stops a deploy job without body parsing.
* When several sections fail in different ways, the status names only the category that ranks highest after dispatch; callers that need every failure read the per-section outcomes in the body.
* The endpoint table of the [DESIGN](../DESIGN.md) must use exactly this mapping and keep 422 and 400 apart.
* Carrying the outcome report in a failed response depends on the platform's problem format keeping the gear's extension member; the PRD lists it as a p1 dependency with a matching risk, and the [DESIGN](../DESIGN.md) records it as a pending platform dependency. Until it lands, a failed response carries the status and the problem detail only, and `cpt-cf-application-manager-fr-response-content` is not yet met for failed responses.

Impact by concern:

* Performance: no added cost; the status follows from the category already decided.
* Security: a platform access control failure fails closed with 503. Response and error text must not carry manifest content.
* Reliability: 503 marks exactly the retryable cases, so callers retry where a retry can help and escalate the rest.
* Data: nothing is stored by this decision; the registry's last result keeps the same partial-failure distinction ([ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md)).
* Integration: fits the platform's standard problem format and generic HTTP clients.
* Operations: the outcome category is an operator signal; monitoring can alert on 500 and 503 separately.
* Testing: see Confirmation.
* Compliance: the deterministic precedence and per-section outcomes give reviewers a reproducible account of each request. No personal data is added.
* UX: this is a machine-facing interface with no screens. Callers act on the status: 422 to fix the manifest, 400 to fix a section, 403 to request access, 503 to retry, 500 to escalate.
* Business: partial installs do not pass unnoticed through deploy jobs that check only the status.

### Confirmation

* Table-driven tests cover each outcome category and each precedence case, before and after dispatch, and assert the HTTP status and the presence of per-section outcomes.
* A test in which at least one section succeeds and at least one fails asserts a status that is not 2xx.
* Review of record: the endpoint table of the [DESIGN](../DESIGN.md) matches the mapping above.

## Pros and Cons of the Options

### One HTTP status per outcome category, partial failure reported as an error

* Good, because callers can act on the category: 422 to fix the manifest, 400 to fix a section, 403 to request access, 503 to retry, 500 to escalate.
* Good, because a status class that is not 2xx makes deploy jobs and monitoring notice partial failures without parsing bodies.
* Good, because the fixed precedence gives the same status in the same situation.
* Neutral, because the per-section detail lives in the body, as in every other option.
* Bad, because a mixed failure is headlined by one category, and the others are visible only in the body.

### Multi-Status (207) for partial success

Return 207 when some sections succeed and others fail, with the details in the body.

* Good, because it reports the per-section detail without choosing one failure to headline.
* Bad, because a 2xx status is read by generic clients, deploy jobs and monitoring as success, so a partial failure would pass unnoticed.
* Bad, because 207 is defined for multi-resource operations in a different protocol family, is unevenly supported by generic clients, and needs callers to inspect the body to learn whether anything failed.

### Success status (200) with per-section outcomes in the body

Return 200 for every request that was dispatched, and report section failures only in the body.

* Good, because it is the simplest mapping for a client that always reads the body.
* Bad, because a 2xx status is read by generic clients, deploy jobs and monitoring as success, so a partial failure would pass unnoticed.
* Bad, because it would make the caller-visible category ambiguous, against the rule that every request has exactly one outcome category with a fixed precedence.

### One generic error status for every failure

Return one error status for every outcome other than succeeded, and carry the category only in the body.

* Good, because the mapping is trivial and needs one rule in every client.
* Bad, because callers could not tell from the status whether to fix the manifest, request access, retry or escalate.
* Bad, because deploy jobs could not tell retryable from non-retryable failures, so they would either retry everything or nothing.
* Bad, because monitoring could not separate platform outages from caller mistakes.

## More Information

* This decision depends on [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md) for the schema-unavailable outcome, [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md) for the section outcomes of ordered dispatch, [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md) for the audit id and the audit-unavailable outcome, and [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md) for the matching last result values.
* Review and supersession: revisit this ADR when a new outcome category is proposed or the platform's standard error model changes. Any such change would supersede this ADR.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-outcome-categories` — the seven categories and their HTTP mapping.
* `cpt-cf-application-manager-fr-outcome-precedence` — the HTTP status for each rank, before and after dispatch.
* `cpt-cf-application-manager-fr-response-content` — every response carries the category, per-section outcomes and the audit id when available.
* `cpt-cf-application-manager-fr-dependency-failures` — a platform access control or registry storage failure before dispatch is 503; a registry failure after dispatch is 500.
* `cpt-cf-application-manager-actor-seeding-caller` — receives one outcome category and per-section outcomes.
* `cpt-cf-application-manager-actor-operator` — monitors outcome categories by status.
