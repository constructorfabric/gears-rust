# E2E Test Scenario Map

Maps DESIGN.md requirements to the E2E tests in `testing/e2e/suites/mini_chat/`.

**Convention:** Scenario IDs use `{area}-{number}` format (e.g., `10-01`). Some tests
name the scenario in a docstring or comment (e.g., `# 10-01, 10-02: Upload and get attachment`).

**Columns:**

- **Test File** — the file of the tests in "Covered by", or `—` when no E2E test covers the scenario.
- **Covered by** — one of:
  - `TestClass::test_function` — the E2E test that covers the scenario. A class name alone
    means every test in the class contributes. A reference in another file is prefixed with
    that file name (`test_web_search.py TestWebSearchEventOrdering`).
  - `GAP — <reason>` — no automated test covers the scenario.
  - `N/A — not implemented (ADR-00xx)` — the capability is not part of P1; see the ADR.
  - `unit test only — <reason>` — covered only by unit tests of the gear; the reason says why
    no E2E test can cover it.
  - `(<reason>)` — not testable as a separate scenario.

**Modes:** the suite runs `--mode offline` (mock provider, default) or `--mode online`
(real providers). Tests marked `online_only` are skipped offline; they are marked
"(online only)" below. Tests that drive or inspect the mock provider are skipped in online
mode (the `offline_only` fixture or a `pytest.skip` at the start of the test): online, the
mock is a no-op that records nothing. Rows marked "mock only" cover a defence-in-depth
limit that a real provider never reaches (it stops at the model's `max_tool_calls`, 2 in
config/base.yaml); only the mock can exceed it. `provider_chat` is parameterized over OpenAI
(`gpt-5.2`, standard tier) and Azure (`azure-gpt-4.1`, premium tier). In online mode every
test timeout is raised to at least 300 s (conftest.py `ONLINE_MIN_TIMEOUT_SECS`).

**Best-effort online tests** skip themselves when the real provider does not produce the
feature (no citations in the answer, the provider timed out or rate-limited). A pass says
the feature works when it shows up; a skip says nothing. They are listed as "(online only,
best-effort)" and never as the only coverage of a scenario.

**Daily periods:** usage is kept per UTC day. Tests that compare daily usage before and
after a turn, or seed today's usage, use the `same_utc_day` fixture: it skips the test when
less than its timeout plus 10 s is left before UTC midnight.

**`test_live_smoke.py` is online only** (module-level `online_only` marker). It sends real
LLM requests to the backend at `BASE_URL` (the rig in `--mode online`, or a backend started
by hand with `E2E_BINARY=skip`) and repeats paths the offline suite covers with the mock
provider. It is best-effort: it skips when the backend is not reachable, when the provider
times out or rate-limits, and when the small-context model is not in the catalog. No row
below uses it as coverage.

**Error contract (ADR-0004):** pre-stream errors are canonical Problem JSON (`type` names the
category, e.g. `invalid_argument`, `not_found`, `aborted`, `resource_exhausted`). Post-stream
errors are an SSE `error` event with `{code, message}`. A body that fails schema
deserialization is 422; malformed JSON and semantic validation failures are 400; size limits
are 400 `out_of_range` (not 413); unsupported upload types are 400 (not 415). A JSON body
without a JSON `Content-Type` is 415 `invalid_argument` (17-17). Every Problem is served as
`application/problem+json` (conftest.py `assert_problem` checks it on every error test).
Requests without a valid token are 401. Resources of another user or tenant are 404.

**Outbox checks** read the messages the gear enqueued from `e2e_outbox_capture`, a copy made
by a SQLite trigger inside the enqueuing transaction (conftest.py "Outbox capture"): the
outbox vacuum deletes a handled message's `toolkit_outbox_body` row within about a second, so
that table cannot show that a message was never enqueued.

**Mock provider paths:** the mock answers 404 to a path that neither configured provider uses
(OpenAI under `/v1` without a query; Azure Files and Vector Stores under `/openai` with
`api-version`; Azure Responses on `/openai/v1/responses`), and every test fails if such a
request was made (conftest.py `reset_mock_provider_state`). It answers like the real API:
every Responses event carries `type` and `sequence_number`; adding a file to a vector store
answers `in_progress` and the next status read `completed` (a test can set other statuses);
the uploaded file records the form's `purpose` and filename. It rejects what the real API
rejects: an upload without a purpose or a filename (400); adding an unknown file to a vector
store (404); a Responses request naming an `input_image` file, a code_interpreter container
file or a `file_search` vector store it does not hold (400 / 404; `check_references` turns
this off per test, no test needs it now); and it answers 404 to the delete of an unknown file
or store. Deleting a file also removes it from every vector store, as the real Files API
does. The mock records the `response.usage` it sends (`get_sent_usages`), so credit checks
compute the expected charge from what the provider reported, not from the `done` event
(conftest.py `provider_usage`). `set_fault` matches a regex against the whole path without the query. Thread summary requests
(non-streaming) are kept apart from the turn requests, so a summary sent in the background
during a later test is not in that test's captured requests.

---

## 01 — Principles & Constraints

| ID    | Scenario                          | Test File             | Covered by                                                              |
|-------|-----------------------------------|-----------------------|-------------------------------------------------------------------------|
| 01-01 | Tenant-Scoped Isolation           | test_isolation.py     | TestIsolation (parameter `other_tenant`)                                |
| 01-02 | Owner-Only Content Access         | test_isolation.py     | TestIsolation (parameter `same_tenant`)                                 |
| 01-03 | Streaming-First Delivery          | —                     | (architectural, not directly testable)                                  |
| 01-04 | Linear Conversation Model         | —                     | (enforced by schema)                                                    |
| 01-05 | OpenAI-Compatible Provider        | —                     | (architectural)                                                         |
| 01-06 | Image on a Model Without Vision → 400 `invalid_argument` (`VISION_NOT_SUPPORTED`), no turn, provider not called | test_attachments.py | TestImageInProviderRequest::test_image_on_model_without_vision_400 |
| 01-07 | No Credential Storage             | —                     | (architectural)                                                         |
| 01-08 | Context Window Budget: message over `max_input_tokens` → 400 `out_of_range` (`INPUT_TOO_LONG`); mandatory context (system prompt + message) over the budget `min(max_input_tokens, context_window - max_output_tokens_applied) - fixed_overhead_tokens` (minus tool surcharges; 2500 on the tiny model, not the uncapped 2572) → 400 `out_of_range` (`CONTEXT_BUDGET_EXCEEDED`) | test_streaming.py | TestStreamInputLimits::test_message_over_max_input_tokens_400, TestStreamInputLimits::test_mandatory_context_over_budget_400, TestStreamInputLimits::test_mandatory_context_capped_by_max_input_tokens_400 |
| 01-09 | License Gate                      | —                     | GAP — the rig always grants the base license feature (interim gate, ADR-0008); rejection is tested by the api-gateway license middleware tests |
| 01-10 | No Buffering Constraint           | test_principles.py    | TestPrinciples::test_no_buffering                                       |
| 01-11 | Model Locked Per Chat             | test_principles.py    | TestPrinciples::test_model_locked_per_chat                              |
| 01-12 | Quota Before Outbound             | test_quota_policy.py  | TestQuotaExhaustion::test_all_tiers_exhausted_429 (provider not called) |
| 01-13 | Kill Switch: disable_premium_tier | —                     | unit test only — kill switches are fixed plugin configuration in the rig (all off) |
| 01-14 | Kill Switch: force_standard_tier  | —                     | unit test only — kill switches are fixed plugin configuration in the rig (all off) |
| 01-15 | Kill Switch: disable_file_search  | —                     | unit test only — kill switches are fixed plugin configuration in the rig (all off) |
| 01-16 | Kill Switch: disable_web_search   | —                     | unit test only — kill switches are fixed plugin configuration in the rig (all off) |
| 01-17 | Kill Switch: disable_images       | —                     | unit test only — kill switches are fixed plugin configuration in the rig (all off) |
| 01-18 | `max_input_tokens: 0` = No Separate Input Limit: no `INPUT_TOO_LONG`, budget `context_window - max_output_tokens_applied - fixed_overhead_tokens` | test_streaming.py | TestStreamInputLimits::test_max_input_tokens_zero_is_no_limit, TestStreamInputLimits::test_max_input_tokens_zero_skips_input_too_long |
| 01-19 | Vision Check on the Effective Model: a downgrade to a model without `VISION_INPUT` reports no vision support | — | unit test only — the rig's downgrade target (gpt-5.2) has vision |
| 01-20 | Partial `kill_switches` Object in the Static Model Policy Config: the switches it omits default to off | — | unit test only — startup configuration, fixed for the whole rig |

Kill switches are fixed plugin configuration in the E2E rig (all off), so they cannot be
toggled per test.

## 02 — Chat CRUD

| ID    | Scenario                                         | Test File             | Covered by                                                   |
|-------|--------------------------------------------------|-----------------------|--------------------------------------------------------------|
| 02-01 | Create Chat with Default Model → 201             | test_chat_crud.py     | TestCreateChat::test_create_chat_default_model               |
| 02-02 | Create Chat with Custom Model → 201              | test_chat_crud.py     | TestCreateChat::test_create_chat_with_model                  |
| 02-03 | Create Chat with Title → 201                     | test_chat_crud.py     | TestCreateChat::test_create_chat_with_title                  |
| 02-04 | Create Chat with Unknown Model → 400 `invalid_argument` (`INVALID_MODEL`) | test_chat_crud.py | TestCreateChat::test_create_chat_invalid_model |
| 02-05 | Get Chat → 200                                   | test_chat_crud.py     | TestGetChat::test_get_chat                                   |
| 02-06 | Get Chat Not Found → 404 `not_found`             | test_chat_crud.py     | TestGetChat::test_get_chat_not_found                         |
| 02-07 | List Chats with Cursor Pagination                | test_chat_crud.py     | TestListChats::test_list_chats, TestListChats::test_list_chats_pagination |
| 02-08 | Update Chat Title → 200, `updated_at` bumped     | test_chat_crud.py     | TestUpdateChat::test_update_title                            |
| 02-09 | Update Chat Not Found → 404 `not_found` | test_chat_crud.py | TestUpdateChat::test_update_not_found |
| 02-10 | Delete Chat → 204, then GET/DELETE → 404, not listed | test_chat_crud.py | TestDeleteChat::test_delete_chat                             |
| 02-11 | Delete Chat Not Found → 404 `not_found`          | test_chat_crud.py     | TestDeleteChat::test_delete_not_found                        |
| 02-12 | Update Title — Whitespace-Only → 400 `invalid_argument` (`INVALID_TITLE`) | test_chat_crud.py | TestUpdateChat::test_update_whitespace_title_rejected     |
| 02-13 | Update Title — 255 chars → 200, 256 → 400 (`INVALID_TITLE`) | test_chat_crud.py     | TestUpdateChat::test_update_title_length_boundary            |
| 02-14 | Create Chat with Disabled Model → 400 `invalid_argument` (`INVALID_MODEL`) | test_chat_crud.py | TestCreateChat::test_create_chat_disabled_model |
| 02-15 | Create Title — 255 chars → 201, 256 → 400 (`INVALID_TITLE`) | test_chat_crud.py     | TestCreateChat::test_create_chat_title_length_boundary       |
| 02-16 | List Chats — Unknown `$filter` Field → 400 `invalid_argument` (`INVALID_FILTER`) | test_chat_crud.py | TestListChats::test_list_chats_unknown_filter_field_400 |
| 02-17 | List Chats — Malformed Cursor → 400 (`INVALID_CURSOR`) | test_chat_crud.py | TestListChats::test_list_chats_malformed_cursor_400          |
| 02-18 | List Ordered by Activity (send moves chat to top) | test_chat_crud.py    | TestListChats::test_send_moves_older_chat_to_top             |
| 02-19 | Update Without Title (schema-invalid) → 422 `invalid_argument` | test_chat_crud.py | TestUpdateChat::test_update_without_title_is_422     |
| 02-20 | Update with Malformed JSON → 400 `invalid_argument` | test_chat_crud.py  | TestUpdateChat::test_update_malformed_json_is_400            |
| 02-21 | Full Conversation Lifecycle (3 turns, history, `message_count`, turn status, total daily usage grows by the sum of the three turns' costs, replay charges nothing, delete) | test_full_scenario.py | TestFullConversationScenario::test_full_conversation |
| 02-22 | Create Title — Whitespace-Only → 400 `invalid_argument` (`INVALID_TITLE`) | test_chat_crud.py | TestCreateChat::test_create_chat_whitespace_title_rejected |
| 02-23 | Create with Schema-Invalid Body (`model: 123`) → 422 `invalid_argument` | test_chat_crud.py | TestCreateChat::test_create_chat_schema_invalid_is_422 |
| 02-24 | Create with Malformed JSON → 400 `invalid_argument` | test_chat_crud.py | TestCreateChat::test_create_chat_malformed_json_is_400 |
| 02-25 | List Chats — Unknown `$orderby` Field → 400 `invalid_argument` (`INVALID_ORDERBY_FIELD`) | test_chat_crud.py | TestListChats::test_list_chats_unknown_orderby_field_400 |
| 02-26 | List Chats — Cursor Continued with Another or Without Its `$filter` → 400 `invalid_argument` (`FILTER_MISMATCH`); the cursor carries only the filter hash | test_chat_crud.py | TestListChats::test_list_chats_cursor_with_other_filter_400 |
| 02-27 | List Chats — `limit=0` → 400 `invalid_argument` (`INVALID_LIMIT`) | test_chat_crud.py | TestListChats::test_list_chats_zero_limit_400 |
| 02-28 | List Chats — `cursor` with `$orderby` → 400 `invalid_argument` (`ORDER_WITH_CURSOR`) | test_chat_crud.py | TestListChats::test_list_chats_orderby_with_cursor_400 |
| 02-29 | Create Chat → 201 with `Location: /mini-chat/v1/chats/{id}` (the path the gear router sees, without the api-gateway `prefix_path`) | test_chat_crud.py | TestCreateChat::test_create_chat_location_header |
| 02-30 | List Chats — `limit` Above 100 → Clamped to 100 (`page_info.limit` 100), not 400 | test_chat_crud.py | TestListChats::test_list_chats_limit_above_100_is_clamped |
| 02-31 | List Chats — Invalid OData Options → 400 `invalid_argument` (`odata` resource, one violation on the option): duplicate `$select` (`INVALID_SELECT`), `$skip` (`UNSUPPORTED_QUERY_PARAM`), `$filter` over 8 KiB (`FILTER_TOO_LONG`), over 2000 nodes (`FILTER_TOO_COMPLEX`), `limit=abc` (`INVALID_QUERY_PARAMS` on `query`) | test_chat_crud.py | TestListChats::test_list_chats_invalid_query_400 |
| 02-32 | List Chats — Valid `$filter` on Each Field: `title eq`, `contains(title, ...)`, `id eq`, `updated_at ge` → exactly the matching chats, newest first | test_chat_crud.py | TestListChatsQuery::test_filter_on_each_field |
| 02-33 | List Chats — `$orderby` on Each Field (`title` asc/desc, `updated_at asc`, `id` asc/desc) sorts the page by that field | test_chat_crud.py | TestListChatsQuery::test_orderby_each_field |
| 02-34 | List Chats — `page_info.prev_cursor`: absent on the first page, set on the next; following it returns the first page (with `next_cursor`, without `prev_cursor`) | test_chat_crud.py | TestListChatsQuery::test_prev_cursor_pages_back |
| 02-35 | List Chats — `$top` / `$skiptoken` Are Aliases of `limit` / `cursor` (same pages); both spellings of one option in a request → 400 `invalid_argument` (`odata` resource) | test_chat_crud.py | TestListChatsQuery::test_top_and_skiptoken_aliases |
| 02-36 | Chat Title Trimmed on Create and Update (leading and trailing whitespace) | test_chat_crud.py | TestCreateChat::test_create_chat_title_trimmed, TestUpdateChat::test_update_title_trimmed |

## 03 — Messages API

| ID    | Scenario                                   | Test File         | Covered by                                         |
|-------|--------------------------------------------|-------------------|----------------------------------------------------|
| 03-01 | List Messages — Cursor Pagination          | test_messages.py  | TestMessages::test_cursor_pagination               |
| 03-02 | OData `$select`: accepted and ignored (the page equals the one without it); an invalid value (duplicate field) → 400 `invalid_argument` (`INVALID_SELECT`) | test_messages.py | TestMessages::test_select_accepted_and_ignored, TestMessages::test_invalid_select_400 |
| 03-03 | OData `$orderby=created_at desc`: exactly the reversed default order (two turns) | test_messages.py  | TestMessages::test_odata_orderby                   |
| 03-04 | OData `$filter=role eq 'assistant'`: exactly the answers of two turns, in order | test_messages.py  | TestMessages::test_odata_filter_role               |
| 03-05 | Message request_id Always Non-Null         | test_messages.py  | TestMessages::test_request_id_non_null             |
| 03-06 | Attachments Array Always Present           | test_messages.py  | TestMessages::test_attachments_array_present       |
| 03-07 | `my_reaction` Present (required) on Every Message, null Without a Reaction | test_messages.py | TestMessages::test_my_reaction_field |
| 03-08 | User + Assistant Messages Share request_id (every turn pair; each turn its own) | test_messages.py | TestMessages::test_request_id_shared_per_turn |
| 03-09 | Unknown `$filter` Field → 400 `invalid_argument` (`INVALID_FILTER`) | test_messages.py | TestMessages::test_unknown_filter_field_400    |
| 03-10 | Messages of Nonexistent Chat → 404 `not_found` | test_messages.py | TestMessages::test_messages_of_nonexistent_chat_404 |
| 03-11 | Chat `message_count`: 0 for a new chat, +2 per turn (two turns) | test_multi_turn.py | TestMultiTurn::test_message_count_increments |
| 03-12 | Messages Ordered Chronologically (two completed turns: content and request_id in order) | test_multi_turn.py | TestMultiTurn::test_messages_ordered_chronologically |
| 03-13 | Unknown `$orderby` Field → 400 `invalid_argument` (`INVALID_ORDERBY_FIELD`) | test_messages.py | TestMessages::test_unknown_orderby_field_400 |
| 03-14 | Malformed Cursor → 400 `invalid_argument` (`INVALID_CURSOR`) | test_messages.py | TestMessages::test_malformed_cursor_400 |
| 03-15 | Cursor Continued with Another or Without Its `$filter` → 400 `invalid_argument` (`FILTER_MISMATCH`) | test_messages.py | TestMessages::test_cursor_with_other_filter_400 |
| 03-16 | `limit=0` → 400 `invalid_argument` (`INVALID_LIMIT`) | test_messages.py | TestMessages::test_zero_limit_400 |
| 03-17 | `cursor` with `$orderby` → 400 `invalid_argument` (`ORDER_WITH_CURSOR`) | test_messages.py | TestMessages::test_orderby_with_cursor_400 |
| 03-18 | `limit` Above 100 → Clamped to 100 (`page_info.limit` 100), not 400 | test_messages.py | TestMessages::test_limit_above_100_is_clamped |
| 03-19 | Unsupported `$` Query Option (`$skip`) → 400 `invalid_argument` (`UNSUPPORTED_QUERY_PARAM`) | test_messages.py | TestMessages::test_unsupported_query_option_400 |
| 03-20 | `$filter` Longer Than 8 KiB → 400 `invalid_argument` (`FILTER_TOO_LONG`) | test_messages.py | TestMessages::test_filter_too_long_400 |
| 03-21 | `$filter` Over 2000 Nodes (within 8 KiB) → 400 `invalid_argument` (`FILTER_TOO_COMPLEX`) | test_messages.py | TestMessages::test_filter_too_complex_400 |
| 03-22 | `limit=abc` → 400 `invalid_argument` (`INVALID_QUERY_PARAMS` on `query`) | test_messages.py | TestMessages::test_limit_not_a_number_400 |
| 03-23 | `page_info.prev_cursor` Pages Back (limit 2 over three turns); `$top` / `$skiptoken` accepted as `limit` / `cursor` | test_messages.py | TestMessages::test_prev_cursor_pages_back |
| 03-24 | Attachment Summaries on the User Message after a Send: `attachment_id`, `kind`, `filename`, `status`; an image also has `img_thumbnail` (the same as the attachment detail), a document none; the answer has none | test_attachments.py | TestSendMessageWithAttachments::test_send_message_with_attachments, TestImageUploadAndSend::test_image_upload_and_send |

## 04 — Streaming: Send Message

| ID    | Scenario                                   | Test File              | Covered by                                                    |
|-------|--------------------------------------------|------------------------|---------------------------------------------------------------|
| 04-01 | Send Message → 200 `Content-Type: text/event-stream` (also retry, edit and a replay) | test_streaming.py | TestStreamBasic::test_stream_returns_200_sse; test_stream_started.py TestStreamStartedOnMutation, TestStreamStartedOnReplay::test_replay_emits_stream_started_with_is_new_turn_false |
| 04-02 | Server Generates request_id if Omitted | test_stream_started.py | TestStreamStartedOnSend::test_stream_started_is_first_event |
| 04-03 | Client request_id Echoed in stream_started | test_stream_started.py | TestStreamStartedOnSend::test_stream_started_request_id_matches_client_id |
| 04-04 | Attachment ID Not a UUID → 422 `invalid_argument` | test_streaming.py | TestStreamPreflightErrors::test_malformed_attachment_id_rejected |
| 04-05 | Unknown Attachment ID → 400 `invalid_argument` (`invalid_attachment`), provider not called | test_streaming.py | TestStreamPreflightErrors::test_nonexistent_attachment_id_rejected |
| 04-06 | Empty Content → 400 `invalid_argument` (`EMPTY_CONTENT`) | test_streaming.py | TestStreamPreflightErrors::test_empty_content_rejected  |
| 04-07 | Missing Content (schema-invalid) → 422 `invalid_argument` | test_streaming.py | TestStreamPreflightErrors::test_missing_content_rejected |
| 04-08 | Chat Not Found → 404 `not_found` (JSON)    | test_streaming.py      | TestStreamPreflightErrors::test_chat_not_found                |
| 04-09 | Messages Persisted After Stream: exactly the user message and the answer, in order, with the sent content, the `delta` text, the turn's request_id and an empty attachments array | test_streaming.py | TestMessages::test_messages_persisted_after_stream, TestMessages::test_user_message_content_matches |
| 04-10 | Assistant Message Stores the Token Counts of `done` | test_streaming.py | TestMessages::test_assistant_message_has_tokens            |
| 04-11 | Too Many Images per Message → 400 `out_of_range` | test_attachments.py | TestTooManyImages::test_too_many_images_rejected            |
| 04-12 | Attachment of Another Chat, of a Failed Upload, Deleted, Still Uploading (`pending`), Stored but Still Being Indexed (`uploaded`), Listed Twice, or More IDs than `max_documents_per_chat + max_images_per_message` → 400 `invalid_argument` (`invalid_attachment`), no turn, provider not called | test_streaming.py | TestStreamInvalidAttachments::test_attachment_of_other_chat_rejected, TestStreamInvalidAttachments::test_failed_attachment_rejected, TestStreamInvalidAttachments::test_deleted_attachment_rejected, TestStreamInvalidAttachments::test_pending_attachment_rejected, TestStreamInvalidAttachments::test_uploaded_attachment_rejected, TestStreamInvalidAttachments::test_duplicate_attachment_ids_rejected, TestStreamInvalidAttachments::test_too_many_attachment_ids_rejected |
| 04-13 | Malformed JSON Body → 400 `invalid_argument` (JSON, not SSE) | test_streaming.py | TestStreamPreflightErrors::test_malformed_json_rejected |
| 04-14 | Whitespace-Only Content → 400 `invalid_argument` (`EMPTY_CONTENT` on `content`: the content is trimmed), no turn | test_streaming.py | TestStreamPreflightErrors::test_whitespace_only_content_rejected |
| 04-15 | Provider SSE Events Without `event:` Lines Are Dispatched by `data.type`: a plain answer and a web search answer (tool events, citations) complete with `done` | test_streaming.py | TestProviderEventsWithoutEventLine; also unit-tested |

## 05 — SSE Event Contract

| ID    | Scenario                                    | Test File              | Covered by                                                   |
|-------|---------------------------------------------|------------------------|--------------------------------------------------------------|
| 05-01 | stream_started: First Event with Fields (`request_id`, `message_id`) | test_stream_started.py | TestStreamStartedOnSend::test_stream_started_is_first_event |
| 05-02 | stream_started: is_new_turn=true on Send | test_stream_started.py | TestStreamStartedOnSend::test_stream_started_is_first_event |
| 05-03 | stream_started: is_new_turn=false on Replay | test_stream_started.py | TestStreamStartedOnReplay::test_replay_emits_stream_started_with_is_new_turn_false |
| 05-04 | Delta Events: type=text, content=string     | test_streaming.py      | TestStreamBasic::test_stream_has_delta_events, TestStreamBasic::test_stream_assembled_text_nonempty |
| 05-05 | Tool Events: phase/name/details (web search: `web_search` `start` then `done`, `details` `{}` on both; code interpreter: `start`, then `done` with the logs output from `response.output_item.done`; file search: `file_search` `start` (`details` `{}`) then `done` (`details.files_searched`: the number of results in `response.file_search_call.completed`, 0 because OpenAI sends none there); exact values, offline) | test_web_search.py | TestWebSearchBasic::test_web_search_tool_events_name_and_phases; test_code_interpreter.py TestCodeInterpreterToolEvents::test_code_interpreter_has_start_and_done, TestCodeInterpreterToolEvents::test_code_interpreter_done_has_output; test_attachments.py TestFileSearchToolEvents::test_file_search_tool_events_and_counter |
| 05-06 | Citations Event: items Array | test_web_search.py | TestWebSearchCitations; TestWebSearchOnline::test_citations_structure_if_present (online only, best-effort: skips when the answer has no citations) |
| 05-07 | File Citation (OpenAI shape: `file_id`, `filename`, `index`) → `attachment_id` and Filename, Empty `snippet`, No `span`, No Provider File ID; Unknown File Dropped | test_attachments.py | TestFileCitationMapping::test_file_citation_maps_to_attachment_id; TestUploadSearchCitationFlow::test_upload_search_citation_flow (online only) |
| 05-08 | Done Event: Core Fields                     | test_streaming.py      | TestStreamDoneEvent::test_done_event_contract                |
| 05-09 | Done Event: Usage Tokens (no internal token fields) | test_streaming.py | TestStreamDoneEvent::test_done_event_contract               |
| 05-10 | Done Event: quota_warnings Array            | test_quota_status.py   | TestQuotaWarningsInDoneEvent::test_done_event_has_quota_warnings |
| 05-11 | Done Event: Downgrade Fields                | test_quota_policy.py   | TestDowngrade                                                |
| 05-12 | Done Event: message_id NOT in done          | test_streaming.py      | TestStreamDoneEvent::test_done_event_contract                |
| 05-13 | Error Event: Terminal with Code and the Provider Message (`response.failed` with `response.error`; flat SSE `error` event) | test_error_mapping.py | TestErrorMapping::test_post_stream_sse_error_event, TestErrorMapping::test_error_event_keeps_provider_message |
| 05-14 | Error: Provider Details Sanitized           | test_error_mapping.py  | TestErrorMapping::test_error_message_no_provider_ids         |
| 05-15 | Ping Events only before the first content (ADR-0010): no content for about 8 s (two provider gaps of 4 s, each under the OAGW idle timeout of 8 s) with `sse_ping_interval_seconds` 5 | test_streaming.py | TestStreamPing::test_ping_only_before_content |
| 05-16 | Event Ordering Grammar: one `stream_started` first, then `ping`/`delta`/`tool`, one terminal `done` last; `citations` right before `done`; `tool` events relayed in the provider's order among the deltas (exact sequences for the mock web search answer, delta-tool-deltas, and the code interpreter answer, tool-deltas) | test_stream_started.py | TestStreamStartedOrdering::test_stream_started_before_deltas_before_done; test_web_search.py TestWebSearchEventOrdering; test_code_interpreter.py TestCodeInterpreterEventOrdering::test_tool_events_before_done |
| 05-17 | Server Closes After Terminal                | test_stream_started.py | TestStreamStartedOrdering::test_stream_started_before_deltas_before_done (the body is complete only when the server closes the connection, and `done` is its last event) |
| 05-18 | Replay `done` Byte-Identical to Original    | —                      | N/A — not implemented (ADR-0010): replay rebuilds `done`, omits `downgrade_reason` and citations |
| 05-19 | stream_started `request_id` Resolves in the Turn Status API (`done`) | test_stream_started.py | TestStreamStartedOnSend::test_stream_started_request_id_matches_turn_status |

## 06 — Idempotency & Replay

| ID    | Scenario                                   | Test File              | Covered by                                                        |
|-------|--------------------------------------------|------------------------|-------------------------------------------------------------------|
| 06-01 | Replay Completed Turn → 200 (same deltas; `done` rebuilt from the stored turn with the same `usage`, `effective_model`, `selected_model`, `quota_decision`); a replay of a downgraded turn keeps `quota_decision` `downgrade` and `downgrade_from` (`downgrade_reason` is not stored) and does not call the provider | test_stream_started.py | TestStreamStartedOnReplay::test_replay_emits_stream_started_with_is_new_turn_false; test_quota_policy.py TestDowngrade::test_premium_exhausted_downgrades_to_standard |
| 06-02 | Replay: is_new_turn=false, Same message_id | test_stream_started.py | TestStreamStartedOnReplay::test_replay_emits_stream_started_with_is_new_turn_false |
| 06-03 | Replay: No LLM Call, No Quota, No Outbox | test_idempotency.py | TestIdempotency::test_replay_does_not_modify_quota_or_call_provider; test_settlement.py TestSettlement::test_one_usage_outbox_event_per_turn (the replay itself: `done`, `is_new_turn` false) |
| 06-04 | Multiple Replays Side-Effect-Free          | test_idempotency.py    | TestIdempotency::test_replay_does_not_modify_quota_or_call_provider (3 replays) |
| 06-05 | Running Turn + Same request_id → 409 `aborted` (`request_id_conflict`) | test_idempotency.py | TestIdempotency::test_running_turn_same_request_id_409 |
| 06-06 | Failed Turn + Same request_id → 409 `aborted` (`request_id_conflict`) | test_idempotency.py | TestIdempotency::test_failed_turn_same_request_id_409 |
| 06-07 | Cancelled Turn + Same request_id → 409 `aborted` (`request_id_conflict`) | test_idempotency.py | TestIdempotency::test_cancelled_turn_same_request_id_409 |
| 06-08 | Replay Priority Over Parallel Turn Check   | test_idempotency.py    | TestIdempotency::test_replay_priority_over_parallel_check         |
| 06-09 | Replay Does Not Modify Quota               | test_idempotency.py    | TestIdempotency::test_replay_does_not_modify_quota_or_call_provider |
| 06-10 | request_id of a Turn Replaced by Retry → 409 `aborted` (`request_id_conflict`) | test_idempotency.py | TestIdempotency::test_request_id_replaced_by_retry_409 |
| 06-11 | request_id of a Turn Removed by DELETE /turns → 409 `aborted` (`request_id_conflict`), no new turn | test_idempotency.py | TestIdempotency::test_request_id_of_deleted_turn_409 |
| 06-12 | request_id of a Completed Turn in Another Chat of the Same User → a New Turn in This Chat (`is_new_turn` true, `done`), not a replay or a conflict: the key is `(chat_id, request_id)` | test_idempotency.py | TestIdempotency::test_request_id_of_another_chat_starts_a_new_turn |
| 06-13 | `request_id` Not a UUID → 422 `invalid_argument`, no turn, provider not called | test_idempotency.py | TestIdempotency::test_request_id_not_a_uuid_422 |

## 07 — Parallel Turn Enforcement

| ID    | Scenario                                    | Test File             | Covered by                                                  |
|-------|---------------------------------------------|-----------------------|-------------------------------------------------------------|
| 07-01 | Partial Unique Index                        | —                     | (DB-level, not e2e testable)                                |
| 07-02 | Second Stream → 409 `aborted` (`turn_already_running`) | test_parallel_turn.py | TestParallelTurn::test_second_stream_409_turn_already_running |
| 07-03 | New Stream Succeeds After Previous Terminal | test_parallel_turn.py | TestParallelTurn::test_new_stream_succeeds_after_terminal   |
| 07-04 | Send While a Retry or Edit Streams → 409 `aborted` (`turn_already_running`); the mutation completes and is the only turn | test_parallel_turn.py | TestParallelTurn::test_send_while_mutation_streams_409 |

## 08 — Turn Mutations

| ID    | Scenario                                    | Test File              | Covered by                                                    |
|-------|---------------------------------------------|------------------------|---------------------------------------------------------------|
| 08-01 | Retry Latest Terminal Turn                  | test_turn_mutations.py | TestTurnRetry::test_retry_replaces_the_answer                 |
| 08-02 | Retry Running Turn → 400 `failed_precondition` (`turn_state`/`STATE`) | test_turn_mutations.py | TestTurnRetry::test_retry_running_turn_400 |
| 08-03 | Retry Non-Latest Turn → 409 `aborted` (`NOT_LATEST_TURN`) | test_turn_mutations.py | TestTurnRetry::test_retry_non_latest_turn_409   |
| 08-04 | Retry Generates New request_id              | test_stream_started.py | TestStreamStartedOnMutation::test_retry_emits_stream_started_with_new_request_id |
| 08-05 | Edit: Replace Content + Regenerate          | test_turn_mutations.py | TestTurnEdit::test_edit_replaces_user_message_and_answer      |
| 08-06 | Edit Stream Has the Send Contract (`stream_started` with a new request_id, `ping`/`delta`, `done` with models and quota decision) | test_stream_started.py | TestStreamStartedOnMutation::test_edit_emits_stream_started_with_new_request_id |
| 08-07 | Delete Last Turn → 204                      | test_turn_mutations.py | TestTurnDelete::test_delete_last_turn_204                     |
| 08-08 | Delete Running Turn → 400 `failed_precondition` (`turn_state`/`STATE`) | test_turn_mutations.py | TestTurnDelete::test_delete_running_turn_400 |
| 08-09 | Delete Non-Latest Turn → 409 `aborted` (`NOT_LATEST_TURN`) | test_turn_mutations.py | TestTurnDelete::test_delete_non_latest_turn_409 |
| 08-10 | Soft-Deleted Turn Not in Messages           | test_turn_mutations.py | TestTurnDelete::test_soft_deleted_turn_excluded_from_messages |
| 08-11 | Concurrent Retries: one 200, the other 409 `aborted` (`NOT_LATEST_TURN`: mutations are serialized on the SQLite rig; `GENERATION_IN_PROGRESS` needs two mutation transactions in flight and only its error mapping is unit-tested) | test_turn_mutations.py | TestConcurrentRetries::test_concurrent_retries_one_wins |
| 08-12 | Retry Failed or Cancelled Turn              | test_turn_mutations.py | TestTurnRetry::test_retry_failed_turn, TestTurnRetry::test_retry_cancelled_turn; test_stream_started.py TestCancelledMessagePersistence::test_retry_cancelled_turn_produces_new_message |
| 08-13 | Old Turn Marked with replaced_by_request_id | test_turn_mutations.py | TestReplacedByRequestId::test_replaced_by_request_id_set      |
| 08-14 | Edit with Empty Content → 400 `invalid_argument` (`EMPTY_CONTENT`) | test_turn_mutations.py | TestTurnEdit::test_edit_empty_content_400 |
| 08-15 | Edit Non-Latest Turn → 409 `aborted` (`NOT_LATEST_TURN`) | test_turn_mutations.py | TestTurnEdit::test_edit_non_latest_turn_409     |
| 08-16 | GET Deleted Turn → 404 `not_found`          | test_turn_mutations.py | TestTurnDelete::test_get_deleted_turn_404                     |
| 08-17 | Second Delete of a Turn → 409 `aborted` (`NOT_LATEST_TURN`) | test_turn_mutations.py | TestTurnDelete::test_second_delete_turn_409_not_latest |
| 08-18 | Deleted Turn Not Sent to Provider           | test_turn_mutations.py | TestTurnDelete::test_deleted_turn_not_sent_to_provider        |
| 08-19 | Retry of Old Turn While Its Retry Streams → 409 `aborted` (`NOT_LATEST_TURN`) | test_turn_mutations.py | TestConcurrentRetries::test_retry_while_retry_running_409_not_latest |
| 08-20 | Retry, Edit or Delete of an Unknown request_id → 404 `not_found` | test_turn_mutations.py | TestUnknownTurn |
| 08-21 | Edit Running Turn → 400 `failed_precondition` (`turn_state`/`STATE`), the turn keeps streaming | test_turn_mutations.py | TestTurnEdit::test_edit_running_turn_400 |
| 08-22 | Edit Without `content` → 422, Malformed JSON → 400 `invalid_argument`; the turn is kept | test_turn_mutations.py | TestTurnEdit::test_edit_body_errors |
| 08-23 | Edit Content over `max_input_tokens` → 400 `out_of_range` (`INPUT_TOO_LONG`), turn kept, provider not called | test_turn_mutations.py | TestTurnEdit::test_edit_content_over_max_input_tokens_400 |
| 08-24 | Edit Content over the Context Budget (tiny-context model) → 400 `out_of_range` (`CONTEXT_BUDGET_EXCEEDED`), provider not called; context assembly runs after the edit committed, so the old turn is replaced and the new turn is `error` with `context_length_exceeded`, no answer, no usage event, no reserve left (DESIGN §3.9) | test_turn_mutations.py | TestTurnEdit::test_edit_content_over_context_budget_400 |
| 08-25 | Edit with Whitespace-Only Content → 400 `invalid_argument` (`EMPTY_CONTENT` on `content`), turn kept | test_turn_mutations.py | TestTurnEdit::test_edit_whitespace_only_content_400 |
| 08-26 | Retry or Edit in a Chat Whose Model Left the Catalog (DB seed) → 400 `invalid_argument` (`INVALID_MODEL` on `model`, chat resource), turn kept, provider not called | test_turn_mutations.py | TestMutationChatModelLeftCatalog::test_mutation_with_model_missing_from_catalog_400 |
| 08-27 | Retry or Edit Copy the Replaced Message's Attachments Except Soft-Deleted Ones (DB seed of `deleted_at`); the new request carries the image as `input_image` and `file_search` | test_turn_mutations.py | TestMutationAttachments::test_mutation_copies_attachments_except_deleted |
| 08-28 | Retry or Edit Re-Run the Image Checks Before the Turn Is Replaced: image turn after the chat model is switched to one without vision (DB seed) → 400 `VISION_NOT_SUPPORTED`; a fifth image linked in the DB → 400 `out_of_range` `TOO_MANY_IMAGES`; turn kept, provider not called | test_turn_mutations.py | TestMutationAttachments::test_image_turn_on_model_without_vision_400, TestMutationAttachments::test_more_images_than_allowed_400 |
| 08-29 | Retry over the Context Budget: a 6000-byte question sent on gpt-5.2, then the chat switched to the tiny-context model (DB seed) → 400 `out_of_range` (`CONTEXT_BUDGET_EXCEEDED`), provider not called; the old turn is replaced, the new turn is `error` with `context_length_exceeded`, no answer, no usage event, no reserve left | test_turn_mutations.py | TestTurnRetry::test_retry_over_context_budget_400 |
| 08-30 | Retry or Edit Sends the (New) Question Once, After the History Before the Turn, With and Without Earlier Turns (regression: the only live turn was sent twice when the snapshot boundary was missing) | test_turn_mutations.py | TestTurnRetry::test_mutation_sends_the_question_once |
| 08-31 | Tool Quotas on Retry and Edit: at the daily `web_search` quota, a retry or edit of a turn that used web search (the flag is kept) → 429 `resource_exhausted` (`web_search`); at the daily `code_interpreter` quota, a retry or edit in a chat with a ready XLSX → 429 (`code_interpreter`); the old turn is kept, provider not called. Control: at the same quotas, a retry or edit of a turn without web search, or in a chat without an XLSX, runs | test_quota_policy.py | TestWebSearchDailyQuota::test_web_search_quota_blocks_mutation, TestWebSearchDailyQuota::test_web_search_quota_allows_mutation_of_plain_turn, TestCodeInterpreterDailyQuota::test_code_interpreter_quota_blocks_mutation, TestCodeInterpreterDailyQuota::test_code_interpreter_quota_allows_mutation_without_xlsx |

## 09 — Turn Lifecycle

| ID    | Scenario                                    | Test File              | Covered by                                                   |
|-------|---------------------------------------------|------------------------|--------------------------------------------------------------|
| 09-01 | Turn Is `running` Right After `stream_started` (GET turn while the stream is open), `done` After It | test_turn_lifecycle.py | TestTurnLifecycle::test_turn_running_then_done; test_turn_mutations.py TestTurnRetry::test_retry_running_turn_400 |
| 09-02 | Preflight Rejection → JSON Error, No Turn Row | test_quota_policy.py | TestQuotaExhaustion::test_all_tiers_exhausted_429; test_streaming.py TestStreamInvalidAttachments, TestStreamInputLimits |
| 09-03 | Atomic User Message + Turn                  | —                      | unit test only — the rollback (on a running-turn conflict or a duplicate request_id) needs the conflict to occur inside the turn-creation transaction, which cannot be timed from the client |
| 09-04 | Quota Fields Persisted at Preflight (`reserve_tokens`, `reserved_credits_micro`, `max_output_tokens_applied`, `minimal_generation_floor_applied`: literal values while the turn runs, unchanged after completion) | test_settlement.py | TestSettlement::test_reservation_snapshot_persisted; test_full_scenario.py TestTurnDetailsInDb::test_max_output_tokens_applied |
| 09-05 | Completed → assistant_message_id Set        | test_turns.py          | TestTurnStatus::test_turn_completed_after_stream             |
| 09-06 | Cancelled With Content → Partial Message    | test_stream_started.py | TestCancelledMessagePersistence::test_cancelled_turn_has_assistant_message_id |
| 09-07 | Cancelled Without Content → message_id NULL | test_turn_lifecycle.py | TestTurnLifecycle::test_cancelled_without_content_null_message_id |
| 09-08 | Failed Turn → the stream ends with one SSE `error` (`provider_error`, the provider message), no `done`; turn `error` with `provider_error`, message_id NULL, no answer | test_turn_lifecycle.py | TestTurnLifecycle::test_failed_turn_null_message_id |
| 09-09 | Cancelled Message in GET /messages (starts with the deltas the client received, a prefix of the full answer) | test_stream_started.py | TestCancelledMessagePersistence::test_cancelled_message_content_starts_with_received_deltas |
| 09-10 | CAS Prevents Double Finalization            | —                      | unit test only — two finalizers racing on one turn cannot be triggered deterministically through the API |
| 09-11 | Turn State Machine: `running` → `done` / `cancelled` / `error` | test_turn_lifecycle.py | TestTurnLifecycle |
| 09-12 | GET Unknown Turn → 404 `not_found`          | test_turns.py          | TestTurnStatus::test_turn_not_found                          |
| 09-13 | Client Disconnect Under Backpressure → `cancelled` | —               | unit test only (both the failed-send path and the cancel-token path) — backpressure on the SSE channel cannot be produced deterministically from the client; a plain disconnect is E2E-tested in test_stream_started.py TestCancelledMessagePersistence |

## 10 — Attachments

Upload is synchronous in P1: `POST /attachments` returns 201 with `status: ready`; polling
still works but is not required (ADR-0007). A document is `ready` only after the chat's
vector store reports it indexed (`completed`).

| ID    | Scenario                                     | Test File                | Covered by                                                  |
|-------|----------------------------------------------|--------------------------|-------------------------------------------------------------|
| 10-01 | Upload Attachment → 201 `ready`              | test_attachments.py      | TestUploadAndGet::test_upload_and_get_attachment            |
| 10-02 | GET Attachment — Status `ready`              | test_attachments.py      | TestUploadAndGet::test_upload_and_get_attachment            |
| 10-03 | DELETE Attachment → 204, GET → 404 `not_found` (attachment `resource_type`) | test_attachments.py | TestDeleteAndVerifyGone::test_delete_and_verify_gone |
| 10-04 | DELETE Referenced Attachment → 409 `already_exists` (`attachment_locked`) | test_attachments.py | TestDeleteReferencedAttachment::test_delete_referenced_attachment_409 |
| 10-05 | Unsupported MIME → 400 `invalid_argument` (`UNSUPPORTED_CONTENT_TYPE`) | test_attachments.py | TestUploadInvalidType::test_upload_invalid_type_rejected |
| 10-06 | Oversize Image → 400 `out_of_range` (`FILE_TOO_LARGE`) | test_attachments.py | TestUploadSizeEnforcement::test_oversize_image_rejected |
| 10-07 | Oversize Document → 400 `out_of_range` (`FILE_TOO_LARGE`) | test_attachments.py | TestUploadSizeEnforcement::test_oversize_document_rejected |
| 10-08 | Document Within Limit → 201 + Ready          | test_attachments.py      | TestUploadSizeEnforcement::test_document_within_limit_succeeds |
| 10-09 | size_bytes Matches Actual                    | test_attachments.py      | TestUploadSizeBytesAccuracy::test_size_bytes_matches_actual |
| 10-10 | MIME Inference from Extension                | test_code_interpreter.py | TestXlsxOctetStreamInference::test_xlsx_octet_stream_accepted |
| 10-11 | Kind Routing: XLSX→code_interpreter, TXT→file_search | test_code_interpreter.py | TestXlsxPurposeRouting                          |
| 10-12 | Image Upload: kind=image                     | test_attachments.py      | TestImageUploadAndSend::test_image_upload_and_send          |
| 10-13 | Multi-Provider Upload: each chat's upload reaches its own provider's Files API (`/v1/files` OpenAI, `/openai/files` Azure) | test_attachments.py | TestDualProviderUpload::test_dual_provider_upload; TestDualProviderRAGStream::test_dual_provider_rag_stream (online only) |
| 10-14 | doc_summary: Async for Docs, Null for Images | —                        | N/A — not implemented (ADR-0007): `doc_summary` is always `null` |
| 10-15 | img_thumbnail of a Ready Image: `content_type` `image/webp`, fitted into 128x128 keeping the aspect ratio (200x100 → 128x64), `data_base64` a WebP (RIFF/WEBP header); the image is sent as `input_image` with its provider file id | test_attachments.py | TestImageUploadAndSend::test_image_upload_and_send |
| 10-16 | Provider Upload Failure → 503 `service_unavailable` with `Retry-After: 10` (every provider error; the upload concurrency limit answers 5, 10-40); the attachment is `failed` with `error_code` `upload_failed` | test_attachments.py | TestUploadProviderFailure::test_upload_failure_marks_attachment_failed |
| 10-17 | Streaming Size Counter (chunked upload, no Content-Length) over the Limit → 400 `out_of_range` (`FILE_TOO_LARGE`), nothing sent to the provider; the attachment row, inserted before the body is read, is `failed` with `file_too_large` | test_attachments.py | TestChunkedUpload::test_chunked_oversize_image_rejected |
| 10-18 | Upload with Content-Length over the Limit → 400 `out_of_range` (`FILE_TOO_LARGE` on field `content_length`): the Content-Length pre-check rejects it before any attachment row is inserted and before the streaming counter reads the part; nothing sent to the provider | test_attachments.py | TestUploadSizeEnforcement::test_oversize_image_rejected, TestUploadSizeEnforcement::test_oversize_document_rejected |
| 10-19 | Chunked-Encoding Streaming Counter           | test_attachments.py      | TestChunkedUpload::test_chunked_upload_within_limit_ready   |
| 10-20 | Images Not Added to Vector Store             | test_attachments.py      | TestImageUploadAndSend::test_image_upload_and_send          |
| 10-21 | provider_file_id Never Exposed               | test_attachments.py      | TestUploadAndGet::test_provider_storage_fields_not_exposed  |
| 10-22 | Stream with Document → `file_search` Tool in the Provider Request | test_provider_request.py | TestFileSearchMaxNumResults::test_file_search_has_max_num_results; test_code_interpreter.py TestXlsxPurposeRouting::test_txt_triggers_file_search_not_code_interpreter; test_attachments.py TestUploadSearchCitationFlow::test_upload_search_citation_flow (online only) |
| 10-23 | Mixed XLSX + TXT → Both Tools                | test_code_interpreter.py | TestMixedAttachments::test_mixed_xlsx_and_txt_both_tools_in_request |
| 10-24 | Image + Document Combined: one request with `input_image` (the image's provider file id) in the user message and `file_search` on the chat's vector store; the model uses both (online) | test_attachments.py      | TestAttachmentsInProviderRequest::test_document_and_image_in_one_request, TestDocumentAndImageTogether::test_document_and_image_combined (online only) |
| 10-25 | GET Nonexistent Attachment → 404 `not_found` (attachment `resource_type`) | test_attachments.py      | TestUploadAndGet::test_get_nonexistent_attachment_404       |
| 10-26 | Documents per Chat Exceeded → 429 `resource_exhausted` (`document_limit`) | test_attachments.py | TestPerChatLimits::test_document_limit_exceeded |
| 10-27 | Storage per Chat Exceeded → 429 `resource_exhausted` (`storage_limit`): documents 1 KiB under the 100 MB chat limit, each within the per-file limit; a 100-byte upload fits, a 2 KiB upload is rejected, provider not called | test_attachments.py | TestPerChatLimits::test_storage_limit_exceeded |
| 10-28 | Max Indexed Chunks per Chat                  | —                        | N/A — not implemented (ADR-0007)                            |
| 10-29 | Deleted Document: its citations are dropped | test_attachments.py | TestFileCitationMapping::test_citation_of_deleted_attachment_dropped; exclusion from `file_search`: N/A — not implemented (ADR-0007): `file_search` runs without attribute filters, so a deleted document stays searchable until the cleanup handler deletes its provider file |
| 10-30 | XLSX Upload Accepted and Ready               | test_code_interpreter.py | TestXlsxUploadAccepted::test_xlsx_upload_accepted, TestXlsxUploadAccepted::test_xlsx_reaches_ready |
| 10-31 | Code Interpreter Tool Events in Stream (`start`, `done`, after `stream_started` and before the deltas that follow the call) | test_code_interpreter.py | TestCodeInterpreterToolEvents, TestCodeInterpreterEventOrdering::test_tool_events_before_done |
| 10-32 | code_interpreter Tool with container.file_ids (the XLSX provider file) and `include: ["code_interpreter_call.outputs"]` in Provider Request (no `include` without code_interpreter: TestXlsxPurposeRouting::test_txt_triggers_file_search_not_code_interpreter) | test_code_interpreter.py | TestCodeInterpreterProviderRequest::test_code_interpreter_tool_in_request |
| 10-33 | Code Interpreter Real Answer                 | test_code_interpreter.py | TestCodeInterpreterOnline::test_xlsx_code_interpreter_produces_answer (online only) |
| 10-34 | Image Sent to the Model as `input_image`; Recognized by the Model | test_attachments.py | TestImageInProviderRequest::test_image_sent_as_input_image; TestImageRecognition::test_image_recognition_cat (online only) |
| 10-35 | Per-Provider Send with Attachment; Medium File Pipeline (~500 KB: 201 `ready` with its exact size, the provider file added to the chat's vector store, `file_search` on it in the next request) | test_attachments.py | TestAttachmentsInProviderRequest::test_medium_document_upload_and_stream; TestProviderSendMessageWithAttachment::test_send_message_with_attachment (online only), TestUploadStreamingPipeline::test_medium_file_upload_and_stream (online only) |
| 10-36 | DELETE Unknown Attachment → 404 `not_found` (attachment `resource_type`) | test_attachments.py      | TestDeleteMissingAttachment::test_delete_unknown_attachment_404 |
| 10-37 | XLSX Upload While Code Interpreter Is Unavailable (model without it) → 400 `invalid_argument` (`CODE_INTERPRETER_UNAVAILABLE` on field `file`), nothing stored | test_code_interpreter.py | TestXlsxUploadAccepted::test_xlsx_rejected_without_code_interpreter |
| 10-38 | Send Message Referencing Two Ready Documents → 200, `done` | test_attachments.py | TestSendMessageWithAttachments::test_send_message_with_attachments |
| 10-39 | Repeated DELETE of an Attachment → 204 (idempotent), then GET → 404 | test_attachments.py | TestDeleteMissingAttachment::test_second_delete_attachment_is_idempotent |
| 10-40 | Upload While All `max_concurrent_uploads` (10) Permits Are Taken → 503 `service_unavailable`, `Retry-After: 5`, nothing stored; the held uploads then complete `ready` | test_attachments.py | TestUploadConcurrencyLimit::test_upload_over_concurrency_limit_503 |
| 10-41 | Upload to an Unknown Chat → 404 `not_found` (chat `resource_type`) | test_attachments.py | TestUploadUnknownChat::test_upload_to_unknown_chat_404 |
| 10-42 | Malformed Upload Request → 400 `invalid_argument` (attachment `resource_type`): multipart without a boundary (`BOUNDARY_REQUIRED`), unparsable multipart body (`MULTIPART_ERROR`), no `file` field (`MISSING_FILE`), `file` part without a Content-Type (`MISSING_CONTENT_TYPE`); nothing stored, provider not called | test_attachments.py | TestUploadMultipartErrors::test_malformed_upload_400 |
| 10-43 | Provider-Native file_search Counted: `file_search` tool events (`details` as in 05-05), `chat_turns.file_search_completed_count` 1, usage event `file_search_calls` 1 | test_attachments.py | TestFileSearchToolEvents::test_file_search_tool_events_and_counter |
| 10-44 | More Than `code_interpreter_max_calls_per_message` (10) Code Interpreter Calls in One Answer → tool events of the 10 allowed calls, SSE `error` `code_interpreter_calls_exceeded`, turn `error` with that code (mock only) | test_code_interpreter.py | TestCodeInterpreterPerMessageLimit::test_eleventh_code_interpreter_call_fails_the_turn |
| 10-45 | Azure File Storage Without `api_version` → Gear Fails to Start | — | unit test only — startup configuration, not reachable from a running rig |
| 10-46 | DELETE of an Attachment Uploaded by Another User in the Caller's Chat → 404 `not_found` (attachment `resource_type`, same as an unknown id), row not deleted, no cleanup enqueued | — | unit test only — not reachable: a chat has one owner, and only the owner can upload to it |
| 10-47 | Upload to a Chat Whose Model Left the Catalog (DB seed) → 400 `invalid_argument` (chat `resource_type`, field `model`, `INVALID_MODEL`), nothing stored, provider not called; other model-resolution failures are returned as is, no fallback storage provider | test_attachments.py | TestUploadChatModelLeftCatalog::test_upload_to_chat_with_model_missing_from_catalog_400; the propagation of other model-resolution failures is unit-tested only |
| 10-48 | Ready Document in a Chat on a Model Without `tool_support.file_search` → No file_search Tool in the Provider Request | test_attachments.py | TestFileSearchModelSupport::test_no_file_search_tool_on_model_without_support |
| 10-49 | Upload Filename: a part without `filename=` is stored as `upload`; a name over 255 characters is cut to 255 keeping the extension | test_attachments.py | TestUploadFilename::test_part_without_filename_is_named_upload, TestUploadFilename::test_long_filename_truncated_keeping_extension |
| 10-50 | `application/octet-stream` with an Unknown Extension → 400 `invalid_argument` (`UNSUPPORTED_CONTENT_TYPE` on `content_type`), nothing stored, provider not called | test_attachments.py | TestUploadFilename::test_octet_stream_with_unknown_extension_400 |
| 10-51 | Upload When the Chat's Vector Store Belongs to Another Storage Backend (DB seed: an Azure chat's store, then `chats.model` switched to an OpenAI model) → 409 `already_exists` (chat resource, `resource_name` `provider_mismatch`); attachment `failed` (`vector_store_failed`), store kept, the file just stored at the provider deleted | test_attachments.py | TestUploadVectorStoreProviderMismatch::test_upload_after_switch_to_other_provider_409 |
| 10-52 | Document Ready Only After Indexing: the vector store answers `in_progress` twice, then `completed` → 201 `ready` after two status reads of the vector store file (`GET /vector_stores/{id}/files/{file_id}`, the route registered for it in OAGW); the upload form's `purpose` is `assistants` (images: not asserted, issue #5022) | test_attachments.py | TestUploadVectorStoreIndexing::test_upload_ready_after_indexing_completes; also unit-tested (indexing wait, OAGW route for the status poll) |
| 10-53 | Indexing `failed` (on the add or after a poll) or `cancelled`, with `last_error` → 503 `service_unavailable` (`Retry-After: 10`, the error code not in the body); the attachment is `failed` with `indexing_failed`; the stored provider file is deleted. Indexing still running at the 25 s deadline is 10-59; a transient status read error is unit-tested only | test_attachments.py | TestUploadVectorStoreIndexing::test_indexing_failure_marks_attachment_failed; also unit-tested |
| 10-54 | Vector Store Creation Fails at the Provider → 503 `service_unavailable` (`Retry-After: 10`, the error code not in the body); the attachment is `failed` with `vector_store_failed`, no store recorded, the stored provider file deleted | test_attachments.py | TestUploadVectorStoreIndexing::test_vector_store_create_failure_503 |
| 10-55 | Request with a Content-Length over the api-gateway `body_limit_bytes` (64 000 000) → 413 from the gateway, answered from the header (the test sends only the start of the body): `application/problem+json`, but not a canonical Problem (`type` `about:blank`), no attachment row, nothing sent to the provider | test_attachments.py | TestUploadSizeEnforcement::test_body_over_gateway_limit_413 |
| 10-56 | Attachment `uploaded` While the Vector Store Indexes It: GET reports `uploaded` during the status polls; it cannot be sent (04-12); once indexing completes the upload returns `ready` | test_streaming.py | TestStreamInvalidAttachments::test_uploaded_attachment_rejected |
| 10-57 | Upload Reaper (slow, about 70 s): in a chat with `max_documents_per_chat` - 1 documents, the client drops the upload while the vector store is still indexing (indexing held, client read timeout 3 s) → the server stops polling (after at least one status read) and the row stays `uploaded` and counts against the limit (one more upload → 429 `document_limit`); once not updated for `stale_after_secs` (60 s in config/base.yaml, the minimum) GET reports `failed` with `error_code` `upload_abandoned`, the provider file is deleted (so gone from the chat's vector store) and `cleanup_status` ends `done`; the failed row no longer counts (a new document upload is `ready`). The api-gateway 30 s timeout is not the trigger in the rig: the upload's own indexing deadline (25 s) fails it first with `indexing_failed` (10-53). A stale `pending` row without a provider file is failed with no cleanup, and a row updated within the window is left alone (a live upload never exceeds 25 s, 10-56): unit tests only | test_attachments.py | TestUploadReaper::test_abandoned_upload_failed_and_provider_file_deleted; the stale-window lower bound is also unit-tested |
| 10-58 | CSV Upload (`rag.allow_csv_upload`, on by default) → 201 `ready`, stored and returned as `text/plain`, kind `document`; the switch off (CSV rejected) is unit-tested only: the rig runs with the default | test_attachments.py | TestCsvUpload::test_csv_stored_as_text_plain_document; also unit-tested |
| 10-59 | Indexing Past the Request Deadline (slow, about 27 s): the vector store stays `in_progress` past the 25 s upload deadline → 201 with `status: uploaded`; a send with it is rejected (400 `invalid_attachment`); once indexing completes the background wait sets `ready`. The background failure and the 10-minute limit are unit-tested only | test_streaming.py | TestStreamInvalidAttachments::test_indexing_past_request_deadline_returns_uploaded_then_ready |

## 11 — Models API

| ID    | Scenario                    | Test File      | Covered by                                        |
|-------|-----------------------------|----------------|---------------------------------------------------|
| 11-01 | List Models                 | test_models.py | TestListModels::test_list_models                  |
| 11-02 | Catalog Models Listed (exactly the enabled `model_catalog` entries of config/base.yaml) | test_models.py | TestListModels::test_catalog_models_present |
| 11-03 | Model Has Required Fields; `multiplier_display` Is the Catalog Value of config/base.yaml | test_models.py | TestListModels::test_model_has_required_fields |
| 11-04 | Get Existing Model → 200 (the requested `model_id`) | test_models.py | TestGetModel::test_internal_fields_not_exposed, TestGetModel::test_extended_response_fields |
| 11-05 | Get Nonexistent Model → 404 `not_found` (`resource_type` model, `resource_name` the model id) | test_models.py | TestGetModel::test_get_nonexistent_model  |
| 11-06 | Internal Fields Not Exposed | test_models.py | TestGetModel::test_internal_fields_not_exposed    |
| 11-07 | Disabled Model Not Listed   | test_models.py | TestDisabledModel::test_disabled_model_not_listed |
| 11-08 | Extended Response Fields    | test_models.py | TestGetModel::test_extended_response_fields       |
| 11-09 | Get Disabled Model → 404    | test_models.py | TestDisabledModel::test_get_disabled_model_404    |

## 12 — Reactions API

| ID    | Scenario                       | Test File         | Covered by                                             |
|-------|--------------------------------|-------------------|--------------------------------------------------------|
| 12-01 | Set Reaction (like) → 200      | test_reactions.py | TestReactions::test_set_reaction_like                  |
| 12-02 | Reaction Upsert Idempotent     | test_reactions.py | TestReactions::test_put_same_reaction_twice_is_idempotent |
| 12-03 | Reaction on User Message (PUT and DELETE) → 400 `failed_precondition` (`reaction_target`/`STATE`) | test_reactions.py | TestReactions::test_reaction_on_user_message_400 |
| 12-04 | Remove Reaction → 204          | test_reactions.py | TestReactions::test_remove_reaction_204                |
| 12-05 | Remove Reaction Idempotent → 204 (a second DELETE after the removal; a DELETE with no reaction set), nothing stored | test_reactions.py | TestReactions::test_remove_reaction_idempotent       |
| 12-06 | Switch Reaction like → dislike | test_reactions.py | TestReactions::test_switch_reaction_like_to_dislike    |
| 12-07 | Reaction on Nonexistent Message → 404 `not_found` (PUT and DELETE; `resource_type` message, `resource_name` the message id) | test_reactions.py | TestReactions::test_reaction_on_nonexistent_message_404 |
| 12-08 | Reaction Other Than like/dislike → 400 `invalid_argument` (`INVALID_REACTION` on field `reaction`), nothing stored | test_reactions.py | TestReactions::test_invalid_reaction_value_400 |
| 12-09 | Reaction Without `reaction` → 422, Malformed JSON → 400 `invalid_argument`; nothing stored | test_reactions.py | TestReactions::test_reaction_body_errors |
| 12-10 | Reaction on the Answer of a Deleted Turn (PUT and DELETE) → 404 `not_found` (message resource), nothing stored | test_reactions.py | TestReactions::test_reaction_on_answer_of_deleted_turn_404 |

## 13 — Quota Status API

| ID    | Scenario                                  | Test File            | Covered by                                                   |
|-------|-------------------------------------------|----------------------|--------------------------------------------------------------|
| 13-01 | Quota Status Endpoint Structure (`warning_threshold_pct` = configured 80) | test_quota_status.py | TestQuotaStatusEndpoint::test_returns_200_with_tiers_and_threshold |
| 13-02 | Each Tier Has Periods                     | test_quota_status.py | TestQuotaStatusEndpoint::test_each_tier_has_periods          |
| 13-03 | remaining_percentage in [0, 100]          | test_quota_status.py | TestQuotaStatusEndpoint::test_remaining_percentage_is_valid  |
| 13-04 | next_reset Is Future                      | test_quota_status.py | TestQuotaStatusEndpoint::test_next_reset_is_future           |
| 13-05 | Credits Increase by Each Turn's Cost (two turns) | test_quota_status.py | TestQuotaUsageTracking::test_used_credits_increase_after_send |
| 13-06 | remaining_credits_micro Decreases After Send | test_quota_status.py | TestQuotaUsageTracking::test_remaining_credits_decrease_after_send |
| 13-07 | SSE quota_warnings Equal REST             | test_quota_status.py | TestQuotaWarningsInDoneEvent::test_quota_warnings_consistent_with_endpoint; test_quota_policy.py TestQuotaStatusFlags::test_done_quota_warnings_match_status |
| 13-08 | Warning Fires at Threshold Boundary       | test_quota_policy.py | TestQuotaStatusFlags::test_total_daily_flags                 |
| 13-09 | Exhausted Flag at Zero                    | test_quota_policy.py | TestQuotaStatusFlags::test_total_daily_flags                 |
| 13-10 | Usage Accounted per User (owner charged the turn's cost, other user unchanged) | test_isolation.py | TestQuotaIsolation::test_other_user_usage_is_not_charged |
| 13-11 | SSE `quota_warnings[].next_reset` Only on an Entry with `warning` or `exhausted`, then Equal to the Status Endpoint's | test_quota_policy.py | TestQuotaStatusFlags::test_done_quota_warnings_match_status |

## 14 — Quota Enforcement

| ID    | Scenario                                  | Test File                 | Covered by                                                   |
|-------|-------------------------------------------|---------------------------|--------------------------------------------------------------|
| 14-01 | Preflight Reserve Persisted (while the turn runs; unchanged after completion) | test_settlement.py | TestSettlement::test_reservation_snapshot_persisted; test_full_scenario.py TestTurnDetailsInDb::test_max_output_tokens_applied |
| 14-02 | Tier Downgrade: Premium Exhausted → Standard | test_quota_policy.py   | TestDowngrade::test_premium_exhausted_downgrades_to_standard |
| 14-03 | Bucket Model: a premium turn charges `total` and `tier:premium`, a standard turn `total` only, each by the `done` usage times the model multipliers | test_quota_enforcement.py | TestQuotaEnforcement::test_bucket_model_premium_counts_total, TestQuotaEnforcement::test_bucket_model_standard_counts_total |
| 14-04 | Daily + Monthly Periods Both Checked      | test_quota_policy.py      | TestPeriodsCheckedSeparately::test_single_exhausted_period_rejects |
| 14-05 | All Tiers Exhausted → 429 `resource_exhausted`, one violation: subject `tokens`, description `quota_exceeded` | test_quota_policy.py | TestQuotaExhaustion::test_all_tiers_exhausted_429 |
| 14-06 | Reserve Before Provider Call: the reserve is persisted while the provider streams; a reserve that does not fit is rejected before the provider is called | test_settlement.py | TestSettlement::test_reservation_snapshot_persisted; test_quota_policy.py TestQuotaExhaustion::test_all_tiers_exhausted_429 |
| 14-07 | Credits Formula: tokens × per-token multiplier, exact integer credits_micro (computed from the `done` usage) | test_quota_enforcement.py | TestQuotaEnforcement::test_bucket_model_premium_counts_total, TestQuotaEnforcement::test_bucket_model_standard_counts_total; test_quota_status.py TestQuotaUsageTracking::test_used_credits_increase_after_send; rounding of fractional multipliers is unit-tested only |
| 14-08 | max_output_tokens Hard Cap (`max_output_tokens_applied` 8192 on the turn, `max_output_tokens` 8192 in the provider request) | test_full_scenario.py | TestTurnDetailsInDb::test_max_output_tokens_applied; test_provider_request.py TestMaxOutputTokens::test_max_output_tokens_in_request |
| 14-09 | policy_version_applied Persisted (1, the version of the static model policy plugin) | test_quota_enforcement.py | TestQuotaEnforcement::test_policy_version_persisted_per_turn |
| 14-10 | Settlement Uses Persisted Policy          | —                         | unit test only — needs the policy version to change during a turn; the rig's model policy is fixed startup configuration |
| 14-11 | No Stuck Reserves After Completion        | test_settlement.py        | TestSettlement::test_completed_turn_releases_reserve         |
| 14-12 | Web Search Surcharge in Reserve: exactly `web_search_surcharge_tokens` (500) more reserve tokens and 500 × 3 more credits for the same first message (literal values) | test_settlement.py | TestSettlement::test_web_search_surcharge_in_reserve |
| 14-13 | warning_threshold_pct Configurable        | —                         | unit test only — the threshold is startup configuration, fixed for the whole rig |
| 14-14 | Invalid Threshold → Gear Fails to Start   | —                         | unit test only — startup configuration (validated at gear init), not reachable from a running rig |
| 14-15 | Retry While Exhausted → 429, Old Turn Kept | test_quota_policy.py     | TestQuotaExhaustion::test_retry_while_exhausted_keeps_old_turn |
| 14-16 | Disabled Chat Model → Downgrade (`model_disabled`) | test_quota_policy.py | TestDowngrade::test_disabled_chat_model_downgrades     |
| 14-17 | Web Search Daily Quota → 429 `resource_exhausted` (`web_search`) only for web-search requests; one call below the quota the turn runs with the `web_search` tool and its search is counted (the daily row reaches the quota) | test_quota_policy.py | TestWebSearchDailyQuota |
| 14-18 | Web Search Turn Credits and Tokens        | test_web_search_usage.py  | TestWebSearchUsageAccounting                                 |
| 14-19 | Code Interpreter Turn Credits (from the usage the provider reported) and `code_interpreter_calls`; the same `CODEINTERP:` prompt in a chat without an XLSX offers no code_interpreter tool and counts no call | test_code_interpreter_usage.py | TestCodeInterpreterUsageAccounting |
| 14-20 | Code Interpreter Daily Quota → 429        | test_quota_policy.py      | TestCodeInterpreterDailyQuota::test_code_interpreter_quota_only_blocks_ci_chats |
| 14-21 | Per-User Daily Image-Input Quota          | —                         | N/A — not implemented (ADR-0008)                             |
| 14-22 | Per-User Daily file_search Limit          | —                         | N/A — not implemented (ADR-0007)                             |
| 14-23 | Edit While Exhausted → 429 `resource_exhausted` (`tokens`), Old Turn Kept, Provider Not Called | test_quota_policy.py | TestQuotaExhaustion::test_edit_while_exhausted_keeps_old_turn |
| 14-24 | Each Cascade Candidate Checked with Its Own Reserve: 60 000 credits_micro left in `total` — below the premium reserve (8192 output tokens × 15), above the standard one — → downgrade to gpt-5.2 (`premium_quota_exhausted`), not 429 | test_quota_policy.py | TestDowngrade::test_standard_reserve_fits_where_premium_does_not; also unit-tested (expensive premium, expensive standard) |
| 14-25 | Limits Re-Checked in the Reserve Transaction (a concurrent reserve committed after preflight read the usage) → 429, nothing reserved; for send and for retry/edit, where the new turn is `failed` with `quota_exceeded` | — | unit test only (send and retry/edit) — the race window between preflight and the reserve transaction cannot be hit deterministically, and SQLite serializes the writers |
| 14-26 | A Downgraded Turn Is Sent to the Effective Model's Provider: a premium chat (azure-gpt-4.1, Azure) downgraded to gpt-5.2 sends the request to OpenAI `/v1/responses`, not to Azure | test_quota_policy.py | TestDowngrade::test_premium_exhausted_downgrades_to_standard, TestDowngrade::test_standard_reserve_fits_where_premium_does_not |

## 15 — Settlement & Finalization

| ID    | Scenario                                 | Test File          | Covered by                                                      |
|-------|------------------------------------------|--------------------|-----------------------------------------------------------------|
| 15-01 | CAS Guard: First Terminal Wins           | —                  | unit test only — two finalizers racing on one turn cannot be triggered deterministically through the API |
| 15-02 | CAS Loser Exits Without Side Effects     | —                  | unit test only — two finalizers racing on one turn cannot be triggered deterministically through the API |
| 15-03 | Completed: Actual Settlement             | test_quota_enforcement.py | TestQuotaEnforcement::test_bucket_model_premium_counts_total, TestQuotaEnforcement::test_bucket_model_standard_counts_total; test_quota_status.py TestQuotaUsageTracking::test_used_credits_increase_after_send |
| 15-04 | Overshoot ≤ Tolerance → Commit Actual    | —                  | GAP — unit-tested only; an E2E test is possible (the mock provider can report usage above the reserve) |
| 15-05 | Overshoot > Tolerance → Cap at Reserve   | —                  | GAP — unit-tested only; an E2E test is possible (the mock provider can report usage above the reserve) |
| 15-06 | Cancelled After Content, No Provider Usage → Estimated Charge (`aborted`, literal credits), Reserve Released | test_settlement.py | TestSettlement::test_cancelled_with_content |
| 15-07 | Cancelled Before Content → Estimated Charge (`aborted`, literal credits), Reserve Released | test_settlement.py | TestSettlement::test_cancelled_without_content |
| 15-08 | Provider Failure → Estimated Charge (`failed`, literal credits), Reserve Released | test_settlement.py | TestSettlement::test_provider_http_error_releases_reserve |
| 15-09 | Orphan Timeout → Estimated Settlement    | —                  | unit test only — the orphan watchdog's 90 s minimum timeout does not fit the E2E time budget (19-06) |
| 15-10 | Atomic: CAS + Quota + Outbox             | —                  | unit test only — atomicity needs a failure inside the finalization transaction, which the rig cannot inject |
| 15-11 | Outbox Dedupe Key Format                 | test_settlement.py | TestSettlement::test_one_usage_outbox_event_per_turn            |
| 15-12 | Duplicate Outbox Insert Ignored          | —                  | N/A — the toolkit-db outbox has no dedupe; consumers drop duplicates by `dedupe_key` (the `dedupe_key` is pinned in unit tests) |
| 15-13 | One Usage Outbox Message Per Terminal Turn (completed, cancelled, failed) | test_settlement.py | TestSettlement::test_one_usage_outbox_event_per_turn, TestSettlement::test_cancelled_with_content, TestSettlement::test_cancelled_without_content, TestSettlement::test_provider_http_error_releases_reserve |
| 15-14 | Completed Turn Releases Reserve          | test_settlement.py | TestSettlement::test_completed_turn_releases_reserve            |
| 15-15 | Settlement on the Provider's Token Counts: one `actual` usage event whose credits are the provider-reported usage (offline: the mock's `response.usage`, which must also be the `done` usage; online: `done`) times the model multipliers (both providers) | test_settlement.py | TestSettlement::test_completed_turn_releases_reserve |
| 15-16 | Finalization Failure → SSE `error` (`finalization_failed` / `message_persistence_failed`), not `done` | — | unit test only — needs a database failure during finalization, which the rig cannot inject |
| 15-17 | Provider `response.incomplete` → `done`, Turn `completed` with No Error Code, Truncated Text Persisted, Actual Settlement on `response.usage` | test_settlement.py | TestSettlement::test_incomplete_response_is_done_and_settled_on_actual_usage |
| 15-18 | Usage Event `requester_type`: `user` (with `user_id`) for a turn, `system` (no `user_id`, no `turn_id`) for the thread summary | test_settlement.py | TestSettlement::test_one_usage_outbox_event_per_turn; test_thread_summary.py TestThreadSummary::test_summary_replaces_summarized_messages |
| 15-19 | Cached and Reasoning Tokens: stored on the message and sent in the usage event, not in `done`; credits use only total input and output tokens | test_settlement.py | TestSettlement::test_cached_and_reasoning_tokens_recorded_not_billed |
| 15-20 | `response.failed` with `response.usage` → SSE `error` `provider_error`, turn failed, settled on that usage (`failed`, `actual`, literal credits); with `usage: null` the estimate is charged | test_settlement.py | TestSettlement::test_response_failed_settles_on_reported_usage |

## 16 — Context Assembly

| ID    | Scenario                                 | Test File                | Covered by                                                   |
|-------|------------------------------------------|--------------------------|--------------------------------------------------------------|
| 16-01 | System Prompt Delivered (`instructions` equal to the catalog prompt when no tool guard applies) | test_context_assembly.py | TestSystemPrompt::test_system_prompt_sent_as_instructions; TestSystemPrompt::test_ping_pong_proves_system_prompt (online only) |
| 16-02 | System Prompt Across Models              | test_context_assembly.py | TestSystemPrompt::test_system_prompt_sent_as_instructions (both providers) |
| 16-03 | Recent Messages: Oldest First, Up to `recent_messages_limit` (10) | test_context_assembly.py | TestContextHistory::test_history_sent_in_order, TestContextHistoryLimit::test_only_recent_messages_sent |
| 16-04 | Deleted Turns Excluded                   | test_turn_mutations.py   | TestTurnDelete::test_deleted_turn_not_sent_to_provider       |
| 16-05 | Thread Summary Replaces Older Messages; a turn that does not fit next to the summary is dropped whole (question and answer), never an answer without its question | test_thread_summary.py | TestThreadSummary::test_summary_replaces_summarized_messages |
| 16-06 | Only Messages After Summary Boundary; truncation drops whole turns | test_thread_summary.py | TestThreadSummary::test_summary_replaces_summarized_messages; also unit-tested (summary as the first message, truncation drops the summary) |
| 16-07 | Model Recall from Earlier Turns          | test_context_assembly.py | TestContextRecall (online only) |
| 16-08 | web_search Tool with search_context_size (default `low`) | test_provider_request.py | TestWebSearchToolType::test_web_search_tool_type_is_web_search, TestWebSearchToolType::test_web_search_has_search_context_size |
| 16-09 | file_search Tool with max_num_results    | test_provider_request.py | TestFileSearchMaxNumResults::test_file_search_has_max_num_results |
| 16-10 | web_search Disabled → No Tool            | test_provider_request.py | TestWebSearchToolType::test_no_web_search_tool_without_flag; test_web_search.py TestWebSearchDisabledByDefault |
| 16-11 | max_tool_calls in Provider Request       | test_provider_request.py | TestMaxToolCalls                                             |
| 16-12 | Cancelled Partial Message in Context     | test_context_assembly.py | TestCancelledTurnContext::test_cancelled_partial_answer_in_next_request |
| 16-13 | Empty Cancelled Turn → No Message        | test_context_assembly.py | TestCancelledTurnContext::test_empty_cancelled_turn_adds_no_message |
| 16-14 | Tool Guard Instructions Appended         | test_context_assembly.py | TestSystemInstructions::test_web_search_guard_appended       |
| 16-15 | Missing System Prompt → None             | test_context_assembly.py | TestSystemInstructions::test_no_system_prompt_no_instructions |
| 16-16 | Provider Identity: `user` is the hyphen-less tenant and user UUIDs concatenated (64 characters, the OpenAI/Azure limit); `metadata` has tenant, user, chat, `request_type` (`chat` for a turn, `summary` for the thread summary, which runs as the platform default subject: checked in a chat of user B, because the default subject id is user A's id in this rig) and the tool `feature` | test_provider_request.py | TestProviderIdentity::test_chat_request_carries_user_and_metadata; test_thread_summary.py TestThreadSummary::test_summary_replaces_summarized_messages |
| 16-17 | `metadata.feature` of Attachment Tools: `file_search` (document), `code_interpreter` (XLSX), `file_search+code_interpreter` (both) | test_provider_request.py | TestProviderIdentity::test_metadata_feature_of_attachment_tools |
| 16-18 | Provider Endpoints: OpenAI `/v1/files`, `/v1/vector_stores`, `/v1/vector_stores/{id}/files`, `/v1/responses` without a query; Azure `/openai/files`, `/openai/vector_stores`, `/openai/vector_stores/{id}/files` with `api-version=2025-03-01-preview` and the Responses `api_path` `/openai/v1/responses` (the v1 API) without one | test_provider_request.py | TestProviderRequestPaths::test_requests_hit_the_configured_paths |
| 16-19 | Knowledge Search Function Tool Only When Its Parameters Can Be Built (no function tool the provider could call into `unexpected_tool_use`) | — | unit test only (tool omitted when its parameters cannot be built; tool and guard added when enabled, omitted when disabled) — knowledge search is not configured in the rig |

## 17 — Error Mapping & Sanitization

| ID    | Scenario                              | Test File             | Covered by                                                  |
|-------|---------------------------------------|-----------------------|-------------------------------------------------------------|
| 17-01 | Pre-Stream Errors → Problem JSON      | test_streaming.py     | TestStreamPreflightErrors                                   |
| 17-02 | Post-Stream → SSE event: error        | test_error_mapping.py | TestErrorMapping::test_post_stream_sse_error_event          |
| 17-03 | Provider Timeout → provider_timeout   | test_error_mapping.py | TestErrorMapping::test_provider_timeout_error_code          |
| 17-04 | Provider Unavailable (HTTP 503) → `provider_error` with the provider's sanitized `error.message` | test_error_mapping.py | TestErrorMapping::test_provider_unavailable_error_code |
| 17-05 | Provider 429 → `rate_limited`; with `Retry-After: 7` (passed through by OAGW) the message includes the 7 s delay, which reaches the client only in the message; turn `error` with `rate_limited` | test_error_mapping.py | TestErrorMapping::test_rate_limited_error_code, TestErrorMapping::test_rate_limited_with_retry_after |
| 17-06 | Error Sanitization: No Provider IDs; each id is replaced by `[provider_id]` and the rest of the message is kept (`response.failed` and HTTP 500) | test_error_mapping.py | TestErrorMapping::test_error_message_no_provider_ids |
| 17-07 | 404 Masking: Foreign Resource → 404 `not_found` with the same body (`type`, `detail`, `context.resource_type`, `instance`) as the owner's 404 for a chat id that does not exist, once the chat id is masked | test_isolation.py | TestIsolation::test_foreign_resource_is_404 |
| 17-08 | Schema-Invalid Body → 422 `invalid_argument` (one violation on `body`, reason `invalid_json_body`) | test_streaming.py | TestStreamPreflightErrors::test_missing_content_rejected; test_chat_crud.py TestUpdateChat::test_update_without_title_is_422, TestCreateChat::test_create_chat_schema_invalid_is_422; test_reactions.py TestReactions::test_reaction_body_errors; test_turn_mutations.py TestTurnEdit::test_edit_body_errors |
| 17-09 | Malformed JSON → 400 `invalid_argument` (`json_syntax_error`) | test_chat_crud.py | TestUpdateChat::test_update_malformed_json_is_400, TestCreateChat::test_create_chat_malformed_json_is_400; test_streaming.py TestStreamPreflightErrors::test_malformed_json_rejected; test_reactions.py TestReactions::test_reaction_body_errors; test_turn_mutations.py TestTurnEdit::test_edit_body_errors |
| 17-10 | Provider HTTP 504 → `provider_error` (not `provider_timeout`) with the provider's sanitized `error.message` | test_error_mapping.py | TestErrorMapping::test_provider_504_is_provider_error |
| 17-11 | Provider `function_call` Output Item While No Function Tool Is Offered (knowledge search not configured) → SSE `error` `unexpected_tool_use`, turn `error` with that code | test_error_mapping.py | TestErrorMapping::test_function_call_without_knowledge_search_is_unexpected_tool_use |
| 17-12 | Path Parameter Not a UUID (chat id, turn request_id, attachment id, message id of the reaction path on PUT and DELETE) → 400 `invalid_argument`, `field_violations[].reason = invalid_path_params` | test_chat_crud.py | TestPathParameters::test_non_uuid_path_parameter_400, TestPathParameters::test_non_uuid_message_id_in_reaction_path_400, TestPathParameters::test_non_uuid_path_parameter_on_mutations_400 (DELETE/PATCH chat, list messages, send, retry by turn or chat id, edit, delete turn, delete attachment, upload: one violation on `path`) |
| 17-13 | Send to a Chat Whose Model Left the Catalog (DB seed) → 400 `invalid_argument` (`INVALID_MODEL`), no turn, provider not called | test_streaming.py | TestChatModelLeftCatalog::test_send_to_chat_with_model_missing_from_catalog_400 |
| 17-14 | 404 `resource_type` Names the Missing Resource (turn, message, model; `resource_name` its id) | test_turns.py | TestTurnStatus::test_turn_not_found; test_reactions.py TestReactions::test_reaction_on_nonexistent_message_404; test_models.py TestGetModel::test_get_nonexistent_model, TestDisabledModel::test_get_disabled_model_404 |
| 17-15 | Provider Stream Ends Without a Terminal Event → SSE `error` `provider_error` (an invalid response; `stream_interrupted` is only for a provider task that ends without one), turn `error`, no answer stored, estimated settlement (`failed`) | test_error_mapping.py | TestErrorMapping::test_stream_without_terminal_event_is_provider_error |
| 17-16 | `function_call` With Arguments That Are Not JSON → SSE `error` `provider_error`, checked before the tool name, turn `error` | test_error_mapping.py | TestErrorMapping::test_function_call_with_invalid_json_arguments_is_provider_error |
| 17-17 | JSON Body Without a JSON `Content-Type` (none, or `text/plain`) → 415 `invalid_argument`, one violation on `body` (`missing_json_content_type`), no turn: create chat, update chat, send, edit, reaction PUT | test_chat_crud.py | TestJsonContentType::test_json_body_without_json_content_type_415 |
| 17-18 | Provider `response.completed` Without `usage` (the real API always sends it) → SSE `error` `provider_error` (an invalid response), no `done` with zero usage; the turn fails and is settled on the estimate | test_error_mapping.py | TestErrorMapping::test_completed_without_usage_is_provider_error |

## 18 — Web Search

| ID    | Scenario                             | Test File                | Covered by                                                   |
|-------|--------------------------------------|--------------------------|--------------------------------------------------------------|
| 18-01 | Web Search Tool Events (`web_search` `start` then `done`, after `stream_started`, between the deltas of the two answer messages, before `done`) | test_web_search.py | TestWebSearchBasic::test_web_search_tool_events_name_and_phases, TestWebSearchEventOrdering::test_tool_events_before_done |
| 18-02 | Web Search Citations (the `url_citation` range is sliced from the `output_text` part that carries it, in characters) | test_web_search.py | TestWebSearchCitations; TestWebSearchOnline::test_citations_structure_if_present (online only, best-effort: skips when the answer has no citations) |
| 18-03 | Citations Right Before Done | test_web_search.py | TestWebSearchEventOrdering::test_citations_before_done; TestWebSearchOnline::test_citations_structure_if_present (online only, best-effort: skips when the answer has no citations) |
| 18-04 | No web_search Tool Without the Flag: a `SEARCH:` prompt without `web_search` sends no `tools` | test_web_search.py | TestWebSearchDisabledByDefault::test_search_prompt_without_flag_has_no_web_search |
| 18-05 | Works on Standard Model              | test_web_search.py       | TestWebSearchBasic (the `openai` parameter runs on `gpt-5.2`, standard tier) |
| 18-06 | Turn Done After Web Search           | test_web_search.py       | TestWebSearchTurnStatus::test_turn_done_after_web_search     |
| 18-07 | Messages Persisted After Web Search: exactly the user message and the answer, in order, with the sent content, the `delta` text and the turn's request_id | test_web_search.py | TestWebSearchTurnStatus::test_messages_persisted_after_web_search |
| 18-08 | Credits Tracked for Web Search Turns | test_web_search_usage.py | TestWebSearchUsageAccounting::test_web_search_usage_correct  |
| 18-09 | disable_web_search Kill Switch       | —                        | unit test only — kill switches are fixed plugin configuration in the rig (all off) |
| 18-10 | Web Search Call Limits: daily quota; `max_tool_calls` sent to the provider; more than `web_search_max_calls_per_message` (2) in one answer → SSE `error` `web_search_calls_exceeded` (mock only) | test_web_search.py | TestWebSearchPerMessageLimit::test_third_web_search_fails_the_turn; test_quota_policy.py TestWebSearchDailyQuota; test_provider_request.py TestMaxToolCalls |
| 18-11 | Meaningful Answer                    | test_web_search.py       | TestWebSearchOnline (online only)                            |
| 18-12 | `web_search_calls` of the Daily `total` Usage Row Grows by the Completed Searches of a Turn | test_web_search_usage.py | TestWebSearchUsageAccounting::test_web_search_calls_counted |
| 18-13 | Web Search Requested on a Model Without `tool_support.web_search` (gpt-5-nano): no tool, no guard, feature `none`, the daily web_search quota not checked (allowed at the quota), requested flag stored on the turn, no web search counted | test_quota_policy.py | TestWebSearchDailyQuota::test_model_without_web_search_skips_tool_and_quota |

## 19 — Cleanup & Recovery

| ID    | Scenario                               | Test File       | Covered by                                                        |
|-------|----------------------------------------|-----------------|-------------------------------------------------------------------|
| 19-01 | Chat Deletion → Background Cleanup     | test_cleanup.py | TestCleanup::test_deleted_chat_hides_chat_and_attachment, TestCleanupWorkerDB::test_chat_deletion_enqueues_chat_cleanup_event |
| 19-02 | Chat Deletion Cleans Up the Attachment (`cleanup_status` → `done`, no retries) with one DELETE of its own provider file, which the mock then no longer holds | test_cleanup.py | TestCleanupWorkerDB::test_chat_deletion_marks_attachments_for_cleanup |
| 19-03 | Provider 404 on Delete → Success       | test_cleanup.py | TestProviderCleanupOpenAI::test_chat_cleanup_provider_404_is_success, TestProviderCleanupOpenAI::test_attachment_cleanup_provider_404_is_success, TestProviderCleanupAzure::test_chat_cleanup_provider_404_is_success, TestProviderCleanupAzure::test_attachment_cleanup_provider_404_is_success |
| 19-04 | Vector Store Deleted After Attachments | test_cleanup.py | TestProviderCleanupOpenAI::test_vector_store_deleted_after_files, TestProviderCleanupAzure::test_vector_store_deleted_after_files |
| 19-05 | Attachment Cleanup of a Chat with 3 Attachments (each → `done` after one DELETE of its own provider file) | test_cleanup.py | TestCleanupWorkerDB::test_chat_deletion_with_multiple_attachments |
| 19-06 | Orphan Watchdog Detects Stuck Turns | — | unit test only — the orphan watchdog's 90 s minimum timeout (validated at startup) does not fit the E2E time budget |
| 19-07 | Orphan → Estimated Settlement + Outbox | —               | unit test only — the orphan watchdog's 90 s minimum timeout does not fit the E2E time budget (19-06) |
| 19-08 | Crash Recovery: Turn Status API        | —               | unit test only (orphan CAS transition to `failed`, orphan finalization) — the restart itself is not exercised |
| 19-09 | Thread Summary Trigger (no summary task enqueued after a turn below the threshold, while that turn's usage event from the same transaction is; exactly one at the turn that reaches it, targeting the previous turn's answer: the triggering turn is not summarized) | test_thread_summary.py | TestThreadSummary::test_summary_replaces_summarized_messages |
| 19-10 | Thread Summary Worker (summary model, stored summary and frontier, messages marked compressed; `token_estimate` is the summary response's output tokens without its reasoning tokens) | test_thread_summary.py | TestThreadSummary::test_summary_replaces_summarized_messages |
| 19-11 | Retry of the Turn That Triggered the Summary → the Summary Does Not Cover It and Is Kept; the Request Has the Summary and the Original Question, Not the Replaced Answer | test_thread_summary.py | TestThreadSummary::test_retry_after_summary_does_not_resend_replaced_answer |
| 19-12 | Second Chat Delete → 404, Single Cleanup Event | test_cleanup.py | TestCleanupWorkerDB::test_second_delete_chat_404_single_cleanup_event |
| 19-13 | Empty Chat Deletion Still Enqueues Cleanup | test_cleanup.py | TestCleanupWorkerDB::test_chat_without_attachments_still_enqueues |
| 19-14 | Hard Purge After Grace Period          | —               | N/A — not implemented (ADR-0009)                                  |
| 19-15 | Audit Event for Chat Deletion          | —               | N/A — not implemented (ADR-0009)                                  |
| 19-16 | Chat Deletion Does Not Cancel a Running Turn: it completes and is billed (ADR-0009) | test_cleanup.py | TestCleanup::test_running_turn_completes_and_is_billed_after_chat_delete |
| 19-17 | Provider 403 on Every File Delete → the attachment cleanup is retried `max_attempts` times and ends in `failed` | test_cleanup.py | TestProviderCleanupOpenAI::test_attachment_cleanup_provider_403_ends_failed, TestProviderCleanupAzure::test_attachment_cleanup_provider_403_ends_failed |
| 19-18 | Provider 500 on Every Vector Store Delete → the chat cleanup message is dead-lettered after `max_attempts` deliveries, not retried again (the processor offset is past it); the `chat_vector_stores` row stays | test_cleanup.py | TestProviderCleanupOpenAI::test_vector_store_delete_500_is_dead_lettered, TestProviderCleanupAzure::test_vector_store_delete_500_is_dead_lettered |
| 19-19 | Attachment Deletion Enqueues Cleanup Event (with the provider file id); the cleanup deletes that file once and ends `done` | test_cleanup.py | TestCleanupWorkerDB::test_attachment_deletion_enqueues_cleanup_event |
| 19-20 | Mutation of a Turn the Summary Covers (after DELETE of the later turn) Drops the Summary in the Mutation Transaction: retry, edit or delete; a retry or edit is sent without the summary; the covered messages are uncompressed, so a retry of the second of three turns carries the first turn as history | test_thread_summary.py | TestThreadSummary::test_mutation_of_summarized_turn_drops_summary, TestThreadSummary::test_retry_of_summarized_turn_restores_earlier_history; also unit-tested, including that a mutation of a turn the summary does not cover keeps it |
| 19-21 | Summary Request Fails at the Provider: retried, and the next request stores the summary; failing on all `max_attempts` (3) deliveries → the task is dead-lettered with the processor offset past it (never delivered again), no summary, no message compressed, the next turn sent without a summary | test_thread_summary.py | TestThreadSummaryFailures |
| 19-22 | Summary Model Missing from the Catalog or Disabled → the summary task is rejected on its first delivery (dead-lettered, no retries); at startup an unresolvable summary model is reported | — | unit test only — `thread_summary_worker.summary_model_id` and the model catalog are startup configuration shared by the whole rig, and a missing summary model would break every summary test |
| 19-23 | Summary Request Answered 400 `context_length_exceeded` → sent again without the oldest messages (six to summarize: two dropped), and that summary is stored with the same frontier | test_thread_summary.py | TestThreadSummaryFailures::test_context_length_exceeded_drops_oldest_messages |

## 20 — Authorization

| ID    | Scenario                                   | Test File         | Covered by                                                 |
|-------|--------------------------------------------|-------------------|------------------------------------------------------------|
| 20-01 | PEP Before Every Operation                 | —                 | unit test only for send (authorized by `send_message`), retry via preview (one PDP call), PDP evaluation failure and model list/get denial; GAP for the other operations: no test checks that each one calls the PEP. Not E2E: the rig's static PDP allows every request, so a missing PEP call is not observable; the 401s of test_isolation.py TestAuthentication come from api-gateway authentication, not the PEP |
| 20-02 | PDP Unreachable → 503 + Retry-After (Fail-Closed); the static authz plugin of the rig cannot fail, so unit-tested only | —                 | unit test only — the static authz plugin of the rig cannot fail; unit tests cover the mapping to authz unavailable and to 503 with Retry-After |
| 20-03 | Foreign Resource → 404 `not_found`, Provider Not Called | test_isolation.py | TestIsolation::test_foreign_resource_is_404, TestIsolation::test_foreign_resource_not_sent_to_provider |
| 20-04 | Chat List Scoped to the Caller: another user's chat is not listed | test_isolation.py | TestIsolation::test_foreign_chat_not_listed |
| 20-05 | Missing Token → 401 `unauthenticated` (`MISSING_BEARER`), `WWW-Authenticate: Bearer realm="api"` | test_isolation.py | TestAuthentication::test_missing_token_is_401 |
| 20-06 | Unknown Token → 401 `unauthenticated` (`AUTHN_FAILED`), `WWW-Authenticate: Bearer error="invalid_token"` | test_isolation.py | TestAuthentication::test_unknown_token_is_401 |
| 20-07 | Foreign Mutations Leave Owner Data Unchanged (chat, messages, turn, attachment, no reaction) | test_isolation.py | TestIsolation::test_owner_resources_unchanged |
| 20-08 | Send Message Authorized by `send_message` Only (a policy without `read` can send) | — | unit test only — the static PDP of the rig allows everything |
| 20-09 | PDP Denial → 403 `permission_denied` (`AUTHZ_DENIED`) | — | unit test only — the static PDP of the rig allows everything (unit tests cover the reason on reads and on mutations) |
| 20-10 | Own Turn, Message and Attachment Ids Addressed Through Another Chat of the Same User → 404 `not_found` (GET/retry/edit/delete turn, GET/DELETE attachment, PUT/DELETE reaction); nothing changes | test_isolation.py | TestCrossChatIds |
| 20-11 | Retry, Edit or Delete of a Turn Another User Started (DB seed of `chat_turns.requester_user_id`; shared chats are not reachable in P1) → 403 `permission_denied` (`AUTHZ_DENIED`), nothing changed, provider not called | test_turn_mutations.py | TestMutationOtherRequester::test_mutation_of_other_users_turn_403 |
