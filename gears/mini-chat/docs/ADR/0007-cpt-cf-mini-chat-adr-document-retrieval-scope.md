---
status: accepted
date: 2026-09-26
---

# P1 scope of document processing and retrieval

**ID**: `cpt-cf-mini-chat-adr-document-retrieval-scope`

## Context and Problem Statement

PRD §5.2 and DESIGN specify several document-related capabilities. Some were implemented differently and some were not implemented. This ADR records the P1 state so the documents, the running system and the E2E suite agree.

## Decision Drivers

* The documents must match what the upload and retrieval paths actually do.
* No large new features in the P1 alignment.
* Every gap must name the requirement it affects and when to revisit it.

## Considered Options

* Implement the missing capabilities.
* Record the implemented behaviour and mark the gaps "Not implemented" / "Future".

## Decision Outcome

Chosen option: "Record the implemented behaviour and mark the gaps".

| Capability | Requirement | Status | Current behaviour |
|---|---|---|---|
| Document summary on upload | `cpt-cf-mini-chat-fr-doc-summary` | Not implemented | `doc_summary` and `summary_updated_at` are always `null`. No background task exists and the ContextPlan has no document-summary tier. |
| Synchronous upload, unless indexing is still running at the request deadline | `cpt-cf-mini-chat-fr-file-upload`, `cpt-cf-mini-chat-fr-image-upload` | Accepted | `POST /attachments` uploads to the provider, indexes the document and builds the thumbnail within the request. For a document it waits until the vector store reports the file `completed` (polling until 25 s after the upload started, inside the api-gateway 30 s request timeout; a transient status read error keeps polling, and when the background wait times out the failure is logged with the last such error); `failed`, `cancelled` or any other status read error before the deadline make the attachment `failed` with `error_code = indexing_failed`, the provider file is deleted (best effort, not retried) and the upload returns 503; a client retry is a new upload with a new provider file id. A response without `status` counts as `in_progress`. It returns 201 with `status: ready`. When the file is still `in_progress` at the deadline, it returns 201 with `status: uploaded`, and a background task keeps polling for up to 10 minutes, refreshing `updated_at` every 20 s so the upload reaper leaves the row alone: `completed` makes the attachment `ready` (a failed write is retried 3 times over 7 s; if it still fails the row stays `uploaded` and the upload reaper later fails it with `upload_abandoned`); `failed`, `cancelled` or the timeout make it `failed` with `error_code = indexing_failed` and `cleanup_status = pending`, in the same transaction as an attachment cleanup outbox event (`attachment_indexing_failed`), and the outbox attachment cleanup deletes the provider file with retries. The wait stops without changes when the chat is deleted (such a row never becomes `ready`) and on gear stop (the row stays `uploaded` and the upload reaper finishes it). A message that references the attachment before it is `ready` gets 400 `invalid_attachment`. On failure the upload returns an HTTP error, and the row stays visible via GET with `status: failed` and `error_code`. When the request is dropped (client disconnect, api-gateway timeout) or the process dies mid-upload or during the background indexing wait, the service records no outcome and the row stays `pending` or `uploaded`. Because the 25 s indexing deadline ends a document upload request before the gateway timeout, in practice this is a client disconnect or a process crash. The leader-elected upload reaper marks such a row `failed` with `error_code = upload_abandoned` after `upload_reaper.stale_after_secs` (default 300 s) and, when the row has a `provider_file_id`, enqueues the attachment cleanup event that deletes the provider file. Rows with a `cleanup_status` already set (the chat was deleted and chat cleanup owns the provider file) are skipped. A failed attachment counts toward neither per-chat limit (`document_limit`, `storage_limit`). A provider file stored for a `pending` row and an Anthropic secondary copy are not deleted (DESIGN.md B.9.5). A client polls GET after an upload that returned `status: uploaded`. |
| Max indexed chunks per chat | `cpt-cf-mini-chat-fr-per-chat-doc-limits` | Not implemented | Only the document count and total size per chat are enforced (429 `document_limit` / `storage_limit`). |
| Per-turn `file_search` call limit | `cpt-cf-mini-chat-fr-file-search` | Accepted (different) | Bounded by the model's `max_tool_calls`, which covers all built-in tools together (default 2). |
| Per-user daily `file_search` limit | PRD §4.1 | Not implemented | `quota_usage.file_search_calls` is not counted. |
| Immediate exclusion of a deleted document from `file_search` | `cpt-cf-mini-chat-fr-attachment-deletion` | Not implemented | Deletion removes the provider file asynchronously. `file_search` is called without attribute filters, so chunks may be returned until the provider file is gone. Citations never reference a deleted attachment. Attachments referenced by a sent message cannot be deleted (409 `attachment_locked`). |
| Anthropic chats: document search (`search_files`, `load_files`) | `features/anthropic-provider-support.md` §1.2 | Not implemented | Documents are indexed in the RAG provider, but the Anthropic adapter drops the `file_search` tool, so Claude cannot search them. Knowledge search (`search_knowledge`), when enabled, is the only retrieval path. |
| Historical messages list deleted attachments | PRD §9 | Accepted (different) | `attachments[]` on messages lists only non-deleted attachments. |

### Consequences

* Good, because every documented behaviour is now testable against the running system.
* Bad, because document summaries, the chunk cap and deletion-time retrieval exclusion remain open product gaps.

### Confirmation

* E2E: a document upload reaches `ready`. `doc_summary` is asserted null/absent only for an XLSX (code interpreter) upload that reaches `ready`; no test asserts it for a `file_search` document.
* E2E scenario 10-04: deleting a referenced attachment gives 409 `attachment_locked`.

## More Information

* Re-evaluate the document summary when the context budget for document-heavy chats becomes a support issue.
* Re-evaluate retrieval exclusion when `file_search_filters` (P4-6) is wired.

## Traceability

* **PRD**: [PRD.md](../PRD.md) §5.2, §9
* **DESIGN**: [DESIGN.md](../DESIGN.md) §3.3 (attachments), §3.6 (file upload sequence), §4 (P1 Scope Boundaries)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-fr-doc-summary`
* `cpt-cf-mini-chat-fr-file-upload`
* `cpt-cf-mini-chat-fr-image-upload`
* `cpt-cf-mini-chat-fr-file-search`
* `cpt-cf-mini-chat-fr-per-chat-doc-limits`
* `cpt-cf-mini-chat-fr-attachment-deletion`
* `cpt-cf-mini-chat-seq-file-upload`
