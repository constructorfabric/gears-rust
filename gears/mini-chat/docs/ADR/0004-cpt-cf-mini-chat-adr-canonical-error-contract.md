---
status: accepted
date: 2026-09-26
---

# Canonical error contract for the Mini Chat REST API

**ID**: `cpt-cf-mini-chat-adr-canonical-error-contract`

## Context and Problem Statement

The original Mini Chat contract (PRD §7.2, DESIGN §3.3 "Error Codes") defined its own JSON error envelope `{code, message}`. It also defined per-error HTTP statuses such as 413 `file_too_large`, 415 `unsupported_file_type` and 502 `provider_error`, and required the SSE `error` event to reuse that envelope.

The platform later moved every gear to the canonical error model in `toolkit-canonical-errors`. That model uses an RFC 9457 `Problem` with a fixed set of categories, each with a fixed HTTP status. Mini Chat was migrated in the same change (`dc9519b3c`, "canonical-error-aware extractors"; mapping in `mini-chat/src/api/rest/error.rs`). The gear's documents were never updated, so they described a wire format that clients no longer receive.

This ADR records the contract that is actually served.

## Decision Drivers

* One error shape across all platform gears, so clients and gateways parse errors the same way.
* HTTP status must follow from the error category, not from per-gear tables.
* Machine-readable reasons must survive the migration: clients still need to tell `NOT_LATEST_TURN` from `request_id_conflict`.
* The SSE stream is a separate channel: once the stream is open, errors cannot change the HTTP status.

## Considered Options

* Keep the gear-specific `{code, message}` envelope and statuses.
* Adopt the canonical `Problem` for REST, and keep `{code, message}` for the SSE `error` event.

## Decision Outcome

Chosen option: "Adopt the canonical `Problem` for REST, and keep `{code, message}` for the SSE `error` event".

**REST (pre-stream) errors** are `Problem` objects with the fields `type`, `title`, `status`, `detail`, `instance`, `trace_id` and `context`. There is **no** top-level `code` field. The machine-readable reason is in one of three places:

* `context.reason` (`aborted`, `permission_denied`);
* `context.field_violations[].reason` (`invalid_argument`, `out_of_range`);
* `context.violations[]` (`failed_precondition`: `{subject, description, type}`; `resource_exhausted`: `{subject, description}`).

| Condition | Category | HTTP | Reason / violation |
|---|---|---|---|
| Chat, message, turn, attachment or model not found (including another user's resource, an attachment uploaded by another user in the caller's chat on `GET` or `DELETE`, or a soft-deleted one) | `not_found` | 404 | `context.resource_type` names the missing resource: `gts.cf.core.mini_chat.{chat,message,turn,attachment,model}.v1~`. A missing attachment reports the attachment type; an upload into an unknown chat reports the chat type. Exception: a repeated `DELETE` of an attachment returns 204 (idempotent) |
| Unknown or disabled model on `POST /chats` | `invalid_argument` | 400 | `field_violations[model].reason = INVALID_MODEL` |
| The chat's model is no longer in the catalog (`messages:stream`, retry, edit, attachment upload) | `invalid_argument` | 400 | `field_violations[model].reason = INVALID_MODEL`. The upload checks it before reading the body |
| Empty or whitespace-only `content` on `messages:stream` or turn edit | `invalid_argument` | 400 | `field_violations[content].reason = EMPTY_CONTENT` |
| Invalid chat title on `POST /chats` or `PATCH /chats/{id}` (empty or whitespace-only after trim, or longer than 255 characters) | `invalid_argument` | 400 | `detail`; the same message is also in `context.format` |
| Invalid reaction value (not `like` or `dislike`); checked before authorization. A body that does not match the schema (e.g. no `reaction` field) is 422, see below | `invalid_argument` | 400 | `detail`; the same message is also in `context.format` |
| Bad OData query on a list endpoint (`GET /chats`, `GET /chats/{id}/messages`: `$filter`, `$orderby`, `$select`, page size, cursor, unsupported query option) | `invalid_argument` | 400 | `context.resource_type = gts.cf.core.odata.query.v1~` (not the chat type, not a `format` violation), for errors raised by the query extractor and by the repository while paginating. `field_violations[].reason` from `toolkit-odata`: `INVALID_FILTER` (`$filter`), `INVALID_ORDERBY_FIELD` (`$orderby`), `INVALID_LIMIT` (field `$top`, `limit=0`), `INVALID_CURSOR` (malformed cursor), `ORDER_MISMATCH` / `FILTER_MISMATCH` (cursor does not match the query), `ORDER_WITH_CURSOR` (`cursor` combined with `$orderby`); from the platform OData extractor (`toolkit::api::odata`): `FILTER_TOO_LONG`, `FILTER_TOO_COMPLEX` (`$filter`), `INVALID_SELECT` (`$select`), `UNSUPPORTED_QUERY_PARAM` (a `$` option the extractor does not bind, e.g. `$skip`, `$count`), `INVALID_QUERY_PARAMS` (unparsable query string). A `limit` above 100 is clamped to 100, not rejected |
| Request body does not match the schema (missing required field, wrong type, e.g. a non-UUID `attachment_ids` entry); malformed JSON is 400 | `invalid_argument` | 422 | `field_violations[body].reason = invalid_json_body` (platform JSON extractor `toolkit::api::rest::extract::Json`) |
| Malformed JSON body | `invalid_argument` | 400 | `field_violations[body].reason = json_syntax_error` (platform JSON extractor) |
| JSON body without a JSON `Content-Type` (`POST /chats`, `PATCH /chats/{id}`, `messages:stream`, turn edit, reaction `PUT`) | `invalid_argument` | 415 | `field_violations[body].reason = missing_json_content_type` (platform JSON extractor). Not declared in the OpenAPI document |
| Path parameter that is not a UUID (chat, message, turn `request_id`, attachment id) | `invalid_argument` | 400 | `field_violations[].reason = invalid_path_params` (platform path extractor) |
| Unsupported upload MIME type | `invalid_argument` | 400 | `UNSUPPORTED_CONTENT_TYPE` (was 415) |
| Code-interpreter-only upload (XLSX) while code interpreter is unavailable (kill switch, or the chat's model lacks `tool_support.code_interpreter`) | `invalid_argument` | 400 | `detail`; the same message is also in `context.format` |
| Upload request is not valid multipart: no boundary in `Content-Type`, unreadable multipart body, no `file` field, `file` part without a content type | `invalid_argument` | 400 | `field_violations[].reason`: `BOUNDARY_REQUIRED` (`content_type`), `MULTIPART_ERROR` (`multipart`), `MISSING_FILE` (`file`), `MISSING_CONTENT_TYPE` (`content_type`) |
| `DELETE /chats/{id}`: the chat-cleanup outbox payload exceeds the outbox size limit (`OutboxError::PayloadTooLarge`) | `invalid_argument` | 400 | `detail`; the same message is also in `context.format`. The same failure on attachment `DELETE` and on turn retry, edit and delete is returned as 500 `internal` |
| Image on a model without vision | `invalid_argument` | 400 | `VISION_NOT_SUPPORTED` (was 415) |
| Invalid, duplicate, foreign or not-ready `attachment_ids`, or more than `rag.max_documents_per_chat + rag.max_images_per_message` of them | `invalid_argument` | 400 | `field_violations[attachment].reason = invalid_attachment` |
| Upload larger than the limit | `out_of_range` | 400 | `FILE_TOO_LARGE` (was 413). A body above api-gateway `defaults.body_limit_bytes` (default 16 MiB) gets 413 from the gateway before it reaches mini-chat |
| Too many images in one message | `out_of_range` | 400 | `TOO_MANY_IMAGES` |
| Message exceeds `max_input_tokens` | `out_of_range` | 400 | `INPUT_TOO_LONG` |
| Mandatory context does not fit the budget | `out_of_range` | 400 | `CONTEXT_BUDGET_EXCEEDED` |
| Kill switch (web search, images) | `failed_precondition` | 400 | `violations[{subject: web_search\|images, type: FEATURE_DISABLED}]` |
| Retry/edit/delete of a non-terminal turn | `failed_precondition` | 400 | `violations[{subject: turn_state, type: STATE}]` |
| Reaction (`PUT` or `DELETE`) on a non-assistant message | `failed_precondition` | 400 | `violations[{subject: reaction_target, type: STATE}]` |
| Missing, invalid or expired bearer token | `unauthenticated` | 401 | `context.reason`: `MISSING_BEARER` / `AUTHN_FAILED` (api-gateway) |
| AuthZ denied (fail-closed) | `permission_denied` | 403 | `AUTHZ_DENIED` |
| The PDP could not evaluate the request (unreachable, timeout, evaluation error); access is still refused (fail-closed) | `service_unavailable` | 503 + `Retry-After` | `Retry-After: 5` (`context.retry_after_seconds = 5`); generic detail, the cause is only logged |
| Retry, edit or delete of a turn whose `requester_user_id` is not the caller | `permission_denied` | 403 | `AUTHZ_DENIED` (`MutationError::Forbidden`) |
| Tenant lacks the required license feature (platform base license feature `CORE_GLOBAL_BASE_LICENSE_FEATURE`; `ai_chat` is the target, ADR-0008) | `permission_denied` | 403 | `LICENSE_FEATURE_REQUIRED` (api-gateway license middleware) |
| Another turn is running in the chat (stream, including the insert race) | `aborted` | 409 | `context.reason = turn_already_running`; `detail = "Another turn is running in this chat"` |
| `request_id` reused for a non-completed or deleted turn | `aborted` | 409 | `context.reason = request_id_conflict`; `detail = "request_id is already used by another turn in this chat"`. The `detail` of both reasons is fixed; the internal message (turn ids, driver text) is only logged |
| Mutation of a turn that is not the latest (including an already deleted turn) | `aborted` | 409 | `NOT_LATEST_TURN` |
| Concurrent mutation lost the running-turn race | `aborted` | 409 | `GENERATION_IN_PROGRESS` |
| Deleting an attachment referenced by a message | `already_exists` | 409 | `resource_name = attachment_locked`; `detail = "Attachment is referenced by one or more messages and cannot be deleted"` |
| Upload into a chat whose vector store was created for another provider backend | `already_exists` | 409 | `resource_name = provider_mismatch`; `detail = "chat vector store belongs to another provider"` |
| Any other unique-constraint violation that the caller does not handle (`DomainError::Conflict` from the DB layer) | `already_exists` | 409 | `resource_name = unique_violation`; `detail = "resource already exists"` (also for any other conflict code). The `detail` of every 409 `already_exists` is a fixed string per code; the driver or backend message is only logged |
| Quota exhausted (tokens, daily web search, daily code interpreter) | `resource_exhausted` | 429 | `violations[{subject: <quota_scope>, description: "quota_exceeded"}]`; `quota_scope` is `tokens`, `web_search` or `code_interpreter` |
| Per-chat document count or storage limit | `resource_exhausted` | 429 | `document_limit` / `storage_limit` (was 400) |
| Storage backend (provider Files / vector store API) failure on attachment upload | `service_unavailable` | 503 + `Retry-After` | `Retry-After: 10` (`context.retry_after_seconds = 10`) (was 502/504) |
| Provider or policy resolution failure before streaming (`messages:stream`, retry, edit) or before an upload reads the body | `internal` | 500 | provider failures after the stream opens are SSE `error` events |
| Upload concurrency limit | `service_unavailable` | 503 + `Retry-After` | `Retry-After: 5` (`context.retry_after_seconds = 5`) |
| Internal / database error | `internal` | 500 | |

`StreamError::Replay` maps to 409 `aborted` with reason `REPLAY` in `api/rest/error.rs`. The arm is defensive: the `messages:stream` handler intercepts `Replay` and serves the buffered SSE replay of the completed turn (`api/rest/handlers/messages.rs`), so clients do not receive this error.

**SSE `error` event.** Once the stream is open, a terminal failure is sent as `event: error` with `data: {code, message}`. This envelope is independent of `Problem`. The codes are listed in DESIGN §3.3 "Streaming error codes".

### Consequences

* Good, because every platform gear returns errors in the same shape and statuses follow the category.
* Good, because reasons are still machine-readable and have stable names.
* Bad, because this is a breaking change for clients written against the old contract: there is no `code` field, and some statuses changed (413→400, 415→400, 502/504→503, document limit 400→429).
* Neutral, because JSON errors and the SSE `error` event now use different shapes.

### Confirmation

* `mini-chat/src/api/rest/error.rs` unit tests pin the category, status and reason of the mappings they cover (not every variant has a dedicated test).
* The E2E suite (`testing/e2e/suites/mini_chat`) asserts `Problem.type` and the reason fields through a shared `assert_problem` helper.
* The generated OpenAPI (`docs/api/api.json`) is the reference for the response schemas.

## Pros and Cons of the Options

### Keep the gear-specific envelope

* Good, because existing clients keep working.
* Bad, because Mini Chat would be the only gear with a private error format. That would need a per-gear transport override in `toolkit-canonical-errors`, which the platform does not provide for 413/415.

### Canonical `Problem` for REST, `{code, message}` for SSE

* Good, because it matches the platform and the implemented code.
* Bad, because it is a documented breaking change.

## More Information

* Supersedes the "Error Codes" table in DESIGN §3.3 and PRD §7.2 as they were before 2026-09.
* The old `docs/openapi.json` was hand-written, described the superseded contract and was removed. The generated `docs/api/api.json` at the repository root is the source of truth.

## Traceability

* **PRD**: [PRD.md](../PRD.md)
* **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-interface-public-api` — error responses of every REST operation.
* `cpt-cf-mini-chat-contract-sse-streaming` — the SSE `error` event envelope.
* `cpt-cf-mini-chat-fr-chat-streaming` — pre-stream errors are normal JSON errors.
* `cpt-cf-mini-chat-nfr-authz-alignment` — denied and failed authorization is 403, a hidden resource is 404.
