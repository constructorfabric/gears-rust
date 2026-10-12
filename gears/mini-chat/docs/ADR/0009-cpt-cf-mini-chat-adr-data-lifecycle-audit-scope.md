---
status: accepted
date: 2026-09-26
---

# P1 scope of data retention, chat deletion and audit content

**ID**: `cpt-cf-mini-chat-adr-data-lifecycle-audit-scope`

## Context and Problem Statement

PRD §5.4 (audit), §5.5 (data lifecycle) and the data-retention NFR describe:
* hard-purging soft-deleted data after a grace period;
* audit events that carry prompt, response, attachments and policy decisions with redaction;
* an audit event for chat deletion.

The implementation provides a subset. This ADR records the P1 behaviour.

## Decision Drivers

* Retention and audit statements have compliance weight and must be exact.
* No large new features in the P1 alignment.

## Considered Options

* Implement purge, full audit content and redaction.
* Record the implemented subset and mark the rest "Not implemented".

## Decision Outcome

Chosen option: "Record the implemented subset and mark the rest Not implemented".

| Capability | Requirement | Status | Current behaviour |
|---|---|---|---|
| Hard-purge of soft-deleted rows after a grace period | `cpt-cf-mini-chat-fr-chat-deletion-cleanup`, `cpt-cf-mini-chat-nfr-data-retention` | Not implemented | Soft-deleted chats, turns, messages, attachments (including thumbnails) and reactions stay in the database indefinitely. Provider files and vector stores are deleted by the outbox cleanup workers. |
| Deleting a chat soft-deletes its messages | PRD §5.5 | Accepted (different) | `DELETE /chats/{id}` soft-deletes the chat row only. Child rows become unreachable because every read goes through the chat. |
| Deleting a chat cancels its running turn | — | Future | A running generation continues and is finalized normally; its usage is billed. A second `DELETE` returns 404. |
| Audit transport | `cpt-cf-mini-chat-fr-audit` | Accepted (different) | Audit events are enqueued in the finalization or mutation transaction to the outbox queue `mini-chat.audit`. They are delivered to the audit plugin selected through types-registry (`MiniChatAuditPluginClientV1`); the bundled `static_audit` plugin logs them. When no plugin is registered, events are acknowledged and dropped: audit is optional per deployment (the bundled `static_audit` plugin is on by default, so no plugin means a deployment choice or a misconfiguration), and blocking chat traffic on it would turn a missing plugin into an outage. Drops are counted in `mini_chat_audit_emit_total{result="dropped"}`. "No plugin" is not cached: every delivery looks the plugin up again (a plugin registered later is used), and the warning is logged once per period without a plugin. An instance that resolves in types-registry but has no client in ClientHub makes the outbox handler return `Retry`, not acknowledge. The handler deserializes the payload before it resolves the plugin, so a corrupt payload is dead-lettered (`Reject`) even when no plugin is available. |
| Audit content: prompt, response, attachment metadata, license and quota-scope decisions, redaction and 8 KiB truncation | `cpt-cf-mini-chat-fr-audit` | Not implemented | Turn audit events carry identities, model, token usage, latency, tool-call counts and the quota decision. `prompt`, `response`, `attachments`, `license` and `quota_scope` are empty, so no redaction is needed. |
| Turn audit event types | `cpt-cf-mini-chat-fr-audit` | Accepted (different) | A finalized turn emits `event_type` `turn_completed` (state `completed`) or `turn_failed` (any other terminal state). Cancelled turns and orphan-watchdog turns emit `turn_failed`; there is no separate cancelled type. Turn mutations emit `turn_retry`, `turn_edit` and `turn_delete`. A retry/edit turn that fails before the reserve emits no audit event. |
| Audit event for chat deletion | UC-004 | Not implemented | Turn mutations (retry, edit, delete) and turn finalization are audited; chat deletion is not. |

### Consequences

* Good, because compliance statements now match the system.
* Bad, because retention compliance (purge) and full audit content remain open gaps and must be scheduled before a deployment that requires them.

### Confirmation

* E2E cleanup scenarios: after `DELETE /chats/{id}`, the chat and its sub-resources return 404 and cleanup outbox rows are written.
* Finalization unit tests pin the audit fields that are populated.

## More Information

* Revisit purge before a deployment with a contractual retention period.
* Adding prompt or response to audit events requires implementing redaction at the same time.

## Traceability

* **PRD**: [PRD.md](../PRD.md) §5.4, §5.5, §6.1
* **DESIGN**: [DESIGN.md](../DESIGN.md) §3.4, §4 "Audit content handling", §4 "Cleanup on Chat Deletion"

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-fr-audit`
* `cpt-cf-mini-chat-fr-chat-deletion-cleanup`
* `cpt-cf-mini-chat-nfr-data-retention`
* `cpt-cf-mini-chat-usecase-delete-chat`
* `cpt-cf-mini-chat-seq-chat-deletion-cleanup`
