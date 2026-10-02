---
status: accepted
date: 2026-09-26
---

# P1 scope of quota, policy and licensing controls

**ID**: `cpt-cf-mini-chat-adr-quota-policy-scope`

## Context and Problem Statement

PRD §5.4 and §5.6, DESIGN §5 and ADR-0003 specify several cost-control and policy mechanisms beyond the implemented credit quotas, tool quotas, downgrade cascade and kill switches. This ADR records which of them P1 does not provide and how the implementation behaves instead.

## Decision Drivers

* Quota documentation is read by billing and support. It must describe what is enforced.
* No large new features in the P1 alignment.
* Limitations that are harmless with the bundled static policy plugin but risky with a remote CCM plugin must be explicit.

## Considered Options

* Implement the missing mechanisms.
* Record the gaps with status and the conditions for revisiting them.

## Decision Outcome

Chosen option: "Record the gaps with status and the conditions for revisiting them".

| Capability | Requirement / design | Status | Current behaviour |
|---|---|---|---|
| Per-user daily image-input quota (default 50) and `image_inputs` / `image_upload_bytes` counters | `cpt-cf-mini-chat-fr-quota-enforcement`, PRD §5.2, §9 | Not implemented | Only `max_images_per_message` (default 4) is enforced. The counter columns exist in `quota_usage` and stay 0. |
| Per-message image bytes cap (`image_bytes_exceeded`) | PRD §5.4 | Not implemented | The per-file upload size limit applies. |
| PolicySnapshot in-memory cache, DB persistence, `POST /internal/policy:notify` | DESIGN §5.2.3, §5.2.7, §5.2.8, Appendix B.3 | Future | Every preflight asks the policy plugin for the current version, the snapshot and the user limits. Settlement asks only for the snapshot of the turn's `policy_version_applied`, inside the finalization transaction. With the bundled in-process static plugin (fixed version 1) this is cheap and cannot fail. A remote CCM plugin needs the cache and a pre-fetched snapshot first. |
| Per-model estimation budgets from the policy snapshot | DESIGN §5.2.1, A.1, B.3 | Implemented | Every estimate uses `ModelCatalogEntry.estimation_budgets`: each cascade candidate's entry for its availability check, the effective model's entry for the booked reserve, the `INPUT_TOO_LONG` check and context assembly. `minimal_generation_floor` comes from the gear configuration (`> 0`, `<= streaming.max_output_tokens`, validated at startup; applied as `min(floor, max_output_tokens_applied)`). The other fields of the gear section `estimation_budgets` are deprecated: parsed, not validated, and `init()` warns for each one set to a non-default value. |
| Quota check and reserve write in one transaction (TOCTOU) | DESIGN §5.4.2 "TOCTOU" | Resolved | The availability check (`QuotaService::preflight_evaluate`) and the reserve write (`reserve_and_create_turn`, or the reserve transaction of retry/edit) are still separate transactions. The reserve transaction re-reads the bucket rows after the increments (`verify_reserve_within_limits`) and rolls back when any bucket is over its limit in any period. The increments hold the row locks (PostgreSQL) or the write lock (SQLite), so the re-check sees every reserve committed before it. The request gets 429 `quota_exceeded` (`quota_scope = tokens`), the same as a preflight reject. On retry/edit the new turn was already committed by the mutation transaction and is marked `failed` (`error_code = quota_exceeded`). |
| Availability checked with the booked reserve | DESIGN §5.4.1, §5.4.2 | Resolved | For each tier the cascade first picks the candidate model, then checks the tier's buckets with the reserve that model would book: its catalog `estimation_budgets`, its multipliers and `min(catalog max_output_tokens, streaming.max_output_tokens)`. The reserve booked on the turn is computed with the same formula for the effective model, so it equals the reserve that passed the check. |
| Billing of agentic knowledge-search iterations | DESIGN §4 "Knowledge Search" | Not implemented | Each `search_knowledge` iteration is a provider call, but only the final iteration's usage is settled. The feature is off by default (`knowledge_search.enabled = false`). |
| License gate on the `ai_chat` feature | `cpt-cf-mini-chat-fr-license-gate`, `cpt-cf-mini-chat-constraint-license-gate` | Accepted interim | Routes require the platform base license feature (`CORE_GLOBAL_BASE_LICENSE_FEATURE`) until the license plugin exposes `ai_chat` (TODO in `mini-chat/src/api/rest/routes/mod.rs`). |
| System tasks charged to a tenant operational bucket, audited and subject to kill switches | ADR-0003, DESIGN §3.2 "System Task Attribution Rules" | Future (P2+) | The thread-summary task emits a usage event with `billing_outcome = system_task`, `settlement_method = none`, `actual_credits_micro = 0` and `requester_type = system`. It emits no audit event and does not check kill switches. |

The daily web-search and code-interpreter quotas **are** implemented. They reject only requests that use the tool (`web_search.enabled`, or ready XLSX attachments for code interpreter).

### Consequences

* Good, because billing and support documentation matches enforcement.
* Bad, because the image quota and the system-task billing remain open product gaps.
* Bad, because switching to a remote CCM policy plugin requires implementing the snapshot cache first.
* Bad, because two concurrent turns that both pass preflight can end with one of them rejected at the reserve write; on retry/edit that turn is left `failed`.

### Confirmation

* Unit tests in `mini-chat/src/domain/service/quota_service.rs` cover tool-quota gating and the downgrade cascade, including per-candidate reserves (`preflight_cascade_checks_candidate_reserve_*`). Unit tests in `mini-chat/src/domain/service/stream_service/mod.rs` cover the reserve re-check on send and retry/edit (`*_rechecks_limits_after_concurrent_reserve`).
* E2E `testing/e2e/suites/mini_chat/test_quota_policy.py` covers 429, premium downgrade, `model_disabled`, the daily web-search and code-interpreter quotas and the quota status flags (usage is seeded per test user). Kill switches are fixed configuration of the E2E rig and are covered by unit tests (`quota_service.rs`, `attachment_service_test.rs`).

## More Information

* Revisit the PolicySnapshot items before any non-static policy plugin is deployed.
* Revisit the license gate when the license plugin exposes `ai_chat`.

## Traceability

* **PRD**: [PRD.md](../PRD.md) §5.2, §5.4, §5.6, §9
* **DESIGN**: [DESIGN.md](../DESIGN.md) §3.2, §5.2, Appendix B

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-fr-quota-enforcement`
* `cpt-cf-mini-chat-fr-license-gate`
* `cpt-cf-mini-chat-fr-quota-billing-architecture`
* `cpt-cf-mini-chat-nfr-cost-control`
* `cpt-cf-mini-chat-constraint-license-gate`
* `cpt-cf-mini-chat-adr-group-chat-usage-attribution`
