# E2E Test Scenario Map

Maps DESIGN.md requirements to e2e test classes in `testing/e2e/suites/mini_chat/`.

**Convention:** Scenario IDs use `{area}-{number}` format (e.g., `10-01`).
Tests reference scenarios in comments: `# 10-01: Upload Attachment → 201`.

---

## 01 — Principles & Constraints

| ID    | Scenario                          | Test File          | Test Class                             |
|-------|-----------------------------------|--------------------|----------------------------------------|
| 01-01 | Tenant-Scoped Isolation           | test_principles.py | TestPrinciples                         |
| 01-02 | Owner-Only Content Access         | test_principles.py | TestPrinciples                         |
| 01-03 | Streaming-First Delivery          | —                  | (architectural, not directly testable) |
| 01-04 | Linear Conversation Model         | —                  | (enforced by schema)                   |
| 01-05 | OpenAI-Compatible Provider        | —                  | (architectural)                        |
| 01-06 | Model Image Capability Constraint | —                  | (enforced by catalog)                  |
| 01-07 | No Credential Storage             | —                  | (architectural)                        |
| 01-08 | Context Window Budget             | —                  | (enforced internally)                  |
| 01-09 | License Gate                      | test_principles.py | TestPrinciples                         |
| 01-10 | No Buffering Constraint           | test_principles.py | TestPrinciples                         |
| 01-11 | Model Locked Per Chat             | test_principles.py | TestPrinciples                         |
| 01-12 | Quota Before Outbound             | test_principles.py | TestPrinciples                         |
| 01-13 | Kill Switch: disable_premium_tier | test_principles.py | TestPrinciples                         |
| 01-14 | Kill Switch: force_standard_tier  | test_principles.py | TestPrinciples                         |
| 01-15 | Kill Switch: disable_file_search  | test_principles.py | TestPrinciples                         |
| 01-16 | Kill Switch: disable_web_search   | test_principles.py | TestPrinciples                         |
| 01-17 | Kill Switch: disable_images       | test_principles.py | TestPrinciples                         |

## 02 — Chat CRUD

| ID    | Scenario                                | Test File         | Test Class     |
|-------|-----------------------------------------|-------------------|----------------|
| 02-01 | Create Chat with Default Model          | test_chat_crud.py | TestCreateChat |
| 02-02 | Create Chat with Custom Model           | test_chat_crud.py | TestCreateChat |
| 02-03 | Create Chat with Title                  | test_chat_crud.py | TestCreateChat |
| 02-04 | Create Chat with Invalid Model          | test_chat_crud.py | TestCreateChat |
| 02-05 | Get Chat                                | test_chat_crud.py | TestGetChat    |
| 02-06 | Get Chat Not Found                      | test_chat_crud.py | TestGetChat    |
| 02-07 | List Chats with Pagination              | test_chat_crud.py | TestListChats  |
| 02-08 | Update Chat Title                       | test_chat_crud.py | TestUpdateChat |
| 02-09 | Update Chat Not Found                   | test_chat_crud.py | TestUpdateChat |
| 02-10 | Delete Chat                             | test_chat_crud.py | TestDeleteChat |
| 02-11 | Delete Chat Not Found                   | test_chat_crud.py | TestDeleteChat |
| 02-12 | Update Title — Whitespace-Only Rejected | test_chat_crud.py | TestUpdateChat |
| 02-13 | Update Title — Max Length               | test_chat_crud.py | TestUpdateChat |

## 03 — Messages API

| ID    | Scenario                                   | Test File         | Test Class   |
|-------|--------------------------------------------|-------------------|--------------|
| 03-01 | List Messages — Cursor Pagination          | test_messages.py  | TestMessages |
| 03-02 | OData $select (Field Projection)           | test_messages.py  | TestMessages |
| 03-03 | OData $orderby                             | test_messages.py  | TestMessages |
| 03-04 | OData $filter                              | test_messages.py  | TestMessages |
| 03-05 | Message request_id Always Non-Null         | test_streaming.py | TestMessages |
| 03-06 | Attachments Array Always Present           | test_streaming.py | TestMessages |
| 03-07 | my_reaction Field on Assistant Messages    | test_messages.py  | TestMessages |
| 03-08 | User + Assistant Messages Share request_id | test_streaming.py | TestMessages |

## 04 — Streaming: Send Message

| ID    | Scenario                                   | Test File              | Test Class                |
|-------|--------------------------------------------|------------------------|---------------------------|
| 04-01 | Send Message Request Body                  | test_streaming.py      | TestStreamBasic           |
| 04-02 | Server Generates request_id if Omitted     | test_stream_started.py | TestStreamStartedOnSend   |
| 04-03 | Client request_id Echoed in stream_started | test_stream_started.py | TestStreamStartedOnSend   |
| 04-04 | Attachment ID Validation                   | test_streaming.py      | TestStreamPreflightErrors |
| 04-05 | Attachment Validation Failure → 400        | test_streaming.py      | TestStreamPreflightErrors |
| 04-06 | Empty Content Rejected                     | test_streaming.py      | TestStreamPreflightErrors |
| 04-07 | Missing Content Rejected                   | test_streaming.py      | TestStreamPreflightErrors |
| 04-08 | Chat Not Found → 404 JSON                  | test_streaming.py      | TestStreamPreflightErrors |
| 04-09 | Messages Persisted After Stream            | test_streaming.py      | TestMessages              |
| 04-10 | Assistant Message Has Token Counts         | test_streaming.py      | TestMessages              |

## 05 — SSE Event Contract

| ID    | Scenario                                    | Test File              | Test Class                        |
|-------|---------------------------------------------|------------------------|-----------------------------------|
| 05-01 | stream_started: First Event with Fields     | test_stream_started.py | TestStreamStartedOnSend           |
| 05-02 | stream_started: is_new_turn=true on Send    | test_stream_started.py | TestStreamStartedOnSend           |
| 05-03 | stream_started: is_new_turn=false on Replay | test_stream_started.py | TestStreamStartedOnReplay         |
| 05-04 | Delta Events: type=text, content=string     | test_streaming.py      | TestStreamBasic                   |
| 05-05 | Tool Events: phase/name/details             | test_web_search.py     | TestWebSearchBasic                |
| 05-06 | Citations Event: items Array                | test_web_search.py     | TestWebSearchCitations            |
| 05-07 | Citations: No Provider IDs Exposed          | test_streaming.py      | TestStreamBasic                   |
| 05-08 | Done Event: Core Fields                     | test_streaming.py      | TestStreamDoneEvent               |
| 05-09 | Done Event: Usage Tokens                    | test_streaming.py      | TestStreamDoneEvent               |
| 05-10 | Done Event: quota_warnings Array            | test_quota_status.py   | TestQuotaWarningsInDoneEvent      |
| 05-11 | Done Event: Downgrade Fields                | test_streaming.py      | TestStreamDoneEvent               |
| 05-12 | Done Event: message_id NOT in done          | test_streaming.py      | TestStreamDoneEvent               |
| 05-13 | Error Event: Terminal with Code             | test_error_mapping.py  | TestErrorMapping                  |
| 05-14 | Error: Provider Details Sanitized           | test_streaming.py      | TestStreamBasic                   |
| 05-15 | Ping Events: Keepalive                      | test_streaming.py      | TestStreamEventOrdering           |
| 05-16 | Event Ordering Grammar                      | test_streaming.py      | TestStreamEventOrdering           |
| 05-17 | Server Closes After Terminal                | —                      | (network-level, hard to e2e test) |

## 06 — Idempotency & Replay

| ID    | Scenario                                   | Test File              | Test Class                |
|-------|--------------------------------------------|------------------------|---------------------------|
| 06-01 | Replay Completed Turn                      | test_turns.py          | TestIdempotency           |
| 06-02 | Replay: is_new_turn=false, Same message_id | test_stream_started.py | TestStreamStartedOnReplay |
| 06-03 | Replay: No LLM Call, No Quota, No Outbox   | test_idempotency.py    | TestIdempotency           |
| 06-04 | Multiple Replays Side-Effect-Free          | test_idempotency.py    | TestIdempotency           |
| 06-05 | Running Turn + Same request_id → 409       | test_idempotency.py    | TestIdempotency           |
| 06-06 | Failed Turn + Same request_id → 409        | test_idempotency.py    | TestIdempotency           |
| 06-07 | Cancelled Turn + Same request_id → 409     | test_idempotency.py    | TestIdempotency           |
| 06-08 | Replay Priority Over Parallel Turn Check   | test_idempotency.py    | TestIdempotency           |
| 06-09 | Replay Does Not Modify Quota               | test_idempotency.py    | TestIdempotency           |

## 07 — Parallel Turn Enforcement

| ID    | Scenario                                    | Test File             | Test Class                   |
|-------|---------------------------------------------|-----------------------|------------------------------|
| 07-01 | Partial Unique Index                        | —                     | (DB-level, not e2e testable) |
| 07-02 | Second Stream → 409 generation_in_progress  | test_parallel_turn.py | TestParallelTurn             |
| 07-03 | New Stream Succeeds After Previous Terminal | test_parallel_turn.py | TestParallelTurn             |

## 08 — Turn Mutations

| ID    | Scenario                                    | Test File              | Test Class                  |
|-------|---------------------------------------------|------------------------|-----------------------------|
| 08-01 | Retry Latest Terminal Turn                  | test_turn_mutations.py | TestTurnRetry               |
| 08-02 | Retry Running Turn → 400                    | test_turn_mutations.py | TestTurnRetry               |
| 08-03 | Retry Non-Latest Turn → 409                 | test_turn_mutations.py | TestTurnRetry               |
| 08-04 | Retry Generates New request_id              | test_turn_mutations.py | TestTurnRetry               |
| 08-05 | Edit: Replace Content + Regenerate          | test_turn_mutations.py | TestTurnRetry               |
| 08-06 | Edit SSE Contract Identical to Stream       | test_stream_started.py | TestStreamStartedOnMutation |
| 08-07 | Delete Last Turn                            | test_turn_mutations.py | TestTurnDelete              |
| 08-08 | Delete Running Turn → 400                   | test_turn_mutations.py | TestTurnDelete              |
| 08-09 | Delete Non-Latest Turn → 409                | test_turn_mutations.py | TestTurnDelete              |
| 08-10 | Soft-Deleted Turn Not in Messages           | test_turn_mutations.py | TestTurnDelete              |
| 08-11 | Concurrent Retries Serialized               | test_turn_mutations.py | TestConcurrentRetries       |
| 08-12 | Retry Cancelled Turn                        | test_turn_mutations.py | TestTurnRetry               |
| 08-13 | Old Turn Marked with replaced_by_request_id | test_turn_mutations.py | TestReplacedByRequestId     |

## 09 — Turn Lifecycle

| ID    | Scenario                                    | Test File              | Test Class                      |
|-------|---------------------------------------------|------------------------|---------------------------------|
| 09-01 | Turn Row Created Before SSE Opens           | test_turns.py          | TestTurnStatus                  |
| 09-02 | Turn Creation Failure → JSON Error          | test_streaming.py      | TestStreamPreflightErrors       |
| 09-03 | Atomic User Message + Turn                  | test_turns.py          | TestTurnStatus                  |
| 09-04 | Immutable Quota Fields                      | test_full_scenario.py  | TestTurnDetailsInDb             |
| 09-05 | Completed → assistant_message_id Set        | test_turns.py          | TestTurnStatus                  |
| 09-06 | Cancelled With Content → Partial Message    | test_stream_started.py | TestCancelledMessagePersistence |
| 09-07 | Cancelled Without Content → message_id NULL | test_turn_lifecycle.py | TestTurnLifecycle               |
| 09-08 | Failed Turn → message_id NULL               | test_turn_lifecycle.py | TestTurnLifecycle               |
| 09-09 | Cancelled Message in GET /messages          | test_stream_started.py | TestCancelledMessagePersistence |
| 09-10 | CAS Prevents Double Finalization            | test_turn_lifecycle.py | TestTurnLifecycle               |
| 09-11 | Turn State Machine: running → terminal      | test_turn_lifecycle.py | TestTurnLifecycle               |

## 10 — Attachments

| ID    | Scenario                                     | Test File                | Test Class                     |
|-------|----------------------------------------------|--------------------------|--------------------------------|
| 10-01 | Upload Attachment → 201                      | test_attachments.py      | TestUploadAndGet               |
| 10-02 | GET Attachment — Polling Until Ready         | test_attachments.py      | TestUploadAndGet               |
| 10-03 | DELETE Attachment → 204, GET → 404           | test_attachments.py      | TestDeleteAndVerifyGone        |
| 10-04 | DELETE Referenced Attachment → 409           | test_attachments.py      | TestDeleteReferencedAttachment |
| 10-05 | Unsupported MIME → 415                       | test_attachments.py      | TestUploadInvalidType          |
| 10-06 | Oversize Image → 413                         | test_attachments.py      | TestUploadSizeEnforcement      |
| 10-07 | Oversize Document → Gateway Rejection        | test_attachments.py      | TestUploadSizeEnforcement      |
| 10-08 | Document Within Limit → 201 + Ready          | test_attachments.py      | TestUploadSizeEnforcement      |
| 10-09 | size_bytes Matches Actual                    | test_attachments.py      | TestUploadSizeBytesAccuracy    |
| 10-10 | MIME Inference from Extension                | test_code_interpreter.py | TestXlsxOctetStreamInference   |
| 10-11 | Kind Routing: XLSX→code_interpreter          | test_code_interpreter.py | TestXlsxPurposeRouting         |
| 10-12 | Image Upload: kind=image                     | test_attachments.py      | TestImageUploadAndSend         |
| 10-13 | Multi-Provider Upload                        | test_attachments.py      | TestProviderUploadAndGet       |
| 10-14 | doc_summary: Async for Docs, Null for Images | —                        | GAP                            |
| 10-15 | img_thumbnail: WEBP Preview for Images       | —                        | GAP                            |
| 10-16 | error_code on Failed Status                  | —                        | GAP                            |
| 10-17 | Mid-Stream 413                               | —                        | GAP                            |
| 10-18 | Content-Length Early Check                   | —                        | GAP                            |
| 10-19 | Chunked-Encoding Streaming Counter           | —                        | GAP                            |
| 10-20 | Images Not Added to Vector Store             | test_attachments.py      | TestImageUploadAndSend         |
| 10-21 | provider_file_id Never Exposed               | test_attachments.py      | TestUploadAndGet               |
| 10-22 | Stream with Document → file_search Events    | test_attachments.py      | TestUploadSearchCitationFlow   |
| 10-23 | Mixed XLSX + TXT → Both Tools                | test_code_interpreter.py | TestMixedAttachments           |
| 10-24 | Image + Document Combined                    | test_attachments.py      | TestDocumentAndImageTogether   |

## 11 — Models API

| ID    | Scenario                    | Test File      | Test Class     |
|-------|-----------------------------|----------------|----------------|
| 11-01 | List Models                 | test_models.py | TestListModels |
| 11-02 | Catalog Models Present      | test_models.py | TestListModels |
| 11-03 | Model Has Required Fields   | test_models.py | TestListModels |
| 11-04 | Get Existing Model          | test_models.py | TestGetModel   |
| 11-05 | Get Nonexistent Model → 404 | test_models.py | TestGetModel   |
| 11-06 | Internal Fields Not Exposed | test_models.py | TestGetModel   |
| 11-07 | Disabled Model Not Visible  | test_models.py | TestListModels |
| 11-08 | Extended Response Fields    | test_models.py | TestGetModel   |

## 12 — Reactions API

| ID    | Scenario                       | Test File         | Test Class    |
|-------|--------------------------------|-------------------|---------------|
| 12-01 | Set Reaction (like/dislike)    | test_reactions.py | TestReactions |
| 12-02 | Reaction Upsert Idempotent     | test_reactions.py | TestReactions |
| 12-03 | Reaction on User Message → 400 | test_reactions.py | TestReactions |
| 12-04 | Remove Reaction → 204          | test_reactions.py | TestReactions |
| 12-05 | Remove Reaction Idempotent     | test_reactions.py | TestReactions |

## 13 — Quota Status API

| ID    | Scenario                                  | Test File                 | Test Class                   |
|-------|-------------------------------------------|---------------------------|------------------------------|
| 13-01 | Quota Status Endpoint Structure           | test_quota_status.py      | TestQuotaStatusEndpoint      |
| 13-02 | Each Tier Has Periods                     | test_quota_status.py      | TestQuotaStatusEndpoint      |
| 13-03 | remaining_percentage in [0, 100]          | test_quota_status.py      | TestQuotaStatusEndpoint      |
| 13-04 | next_reset Is Future                      | test_quota_status.py      | TestQuotaStatusEndpoint      |
| 13-05 | Credits Increase After Send               | test_quota_status.py      | TestQuotaUsageTracking       |
| 13-06 | remaining_percentage Decreases After Send | test_quota_status.py      | TestQuotaUsageTracking       |
| 13-07 | SSE quota_warnings Consistent with REST   | test_quota_status.py      | TestQuotaWarningsInDoneEvent |
| 13-08 | Warning Fires at Threshold Boundary       | test_quota_enforcement.py | TestQuotaEnforcement         |
| 13-09 | Exhausted Flag at Zero                    | test_quota_enforcement.py | TestQuotaEnforcement         |

## 14 — Quota Enforcement

| ID    | Scenario                                  | Test File                 | Test Class                          |
|-------|-------------------------------------------|---------------------------|-------------------------------------|
| 14-01 | Preflight Reserve Persisted               | test_full_scenario.py     | TestTurnDetailsInDb                 |
| 14-02 | Tier Downgrade: Premium → Standard → 429  | test_quota_enforcement.py | TestQuotaEnforcement                |
| 14-03 | Bucket Model: total + tier:premium        | test_quota_enforcement.py | TestQuotaEnforcement                |
| 14-04 | Daily + Monthly Periods Both Checked      | test_quota_enforcement.py | TestQuotaEnforcement                |
| 14-05 | All Tiers Exhausted → 429 quota_exceeded  | test_quota_enforcement.py | TestQuotaEnforcement                |
| 14-06 | Reserve Before Provider Call              | test_quota_status.py      | TestQuotaUsageTracking              |
| 14-07 | Credits Formula: Integer Arithmetic       | test_full_scenario.py     | TestTurnDetailsInDb                 |
| 14-08 | max_output_tokens Hard Cap                | test_full_scenario.py     | TestTurnDetailsInDb                 |
| 14-09 | policy_version_applied Persisted          | test_quota_enforcement.py | TestQuotaEnforcement                |
| 14-10 | Settlement Uses Persisted Policy          | —                         | GAP (needs multi-policy test infra) |
| 14-11 | No Stuck Reserves After Completion        | test_full_scenario.py     | TestQuotaAccumulation               |
| 14-12 | Web Search Surcharge in Reserve           | test_web_search_usage.py  | TestWebSearchUsageAccounting        |
| 14-13 | warning_threshold_pct Configurable        | —                         | GAP (config-level)                  |
| 14-14 | Invalid Threshold → Gear Fails to Start | —                         | GAP (startup test)                  |

## 15 — Settlement & Finalization

| ID    | Scenario                                 | Test File          | Test Class     |
|-------|------------------------------------------|--------------------|----------------|
| 15-01 | CAS Guard: First Terminal Wins           | test_settlement.py | TestSettlement |
| 15-02 | CAS Loser Exits Without Side Effects     | test_settlement.py | TestSettlement |
| 15-03 | Completed: Actual Settlement             | test_settlement.py | TestSettlement |
| 15-04 | Overshoot ≤ 1.1x → Commit Actual         | test_settlement.py | TestSettlement |
| 15-05 | Overshoot > Tolerance → Cap at Reserve   | test_settlement.py | TestSettlement |
| 15-06 | Cancelled With Usage → Actual Settlement | test_settlement.py | TestSettlement |
| 15-07 | Cancelled Without Usage → Estimated      | test_settlement.py | TestSettlement |
| 15-08 | Pre-Provider Failure → Released          | test_settlement.py | TestSettlement |
| 15-09 | Orphan Timeout → Estimated Settlement    | test_settlement.py | TestSettlement |
| 15-10 | Atomic: CAS + Quota + Outbox             | test_settlement.py | TestSettlement |
| 15-11 | Outbox Dedupe Key Format                 | test_settlement.py | TestSettlement |
| 15-12 | Duplicate Outbox Insert Ignored          | test_settlement.py | TestSettlement |
| 15-13 | One Outbox Message Per Terminal Turn     | test_settlement.py | TestSettlement |

## 16 — Context Assembly

| ID    | Scenario                                 | Test File                | Test Class                           |
|-------|------------------------------------------|--------------------------|--------------------------------------|
| 16-01 | System Prompt Delivered                  | test_context_assembly.py | TestSystemPrompt                     |
| 16-02 | System Prompt Across Models              | test_context_assembly.py | TestSystemPrompt                     |
| 16-03 | Recent Messages: Up to K                 | test_context_assembly.py | TestContextInputTokenGrowth          |
| 16-04 | Deleted Turns Excluded                   | test_context_assembly.py | TestContextRecall                    |
| 16-05 | Thread Summary Replaces Older Messages   | —                        | GAP (thread summary not implemented) |
| 16-06 | Only Messages After Summary Boundary     | —                        | GAP                                  |
| 16-07 | Model Recall from Earlier Turns          | test_context_assembly.py | TestContextRecall                    |
| 16-08 | web_search Tool with search_context_size | test_provider_request.py | TestWebSearchToolType                |
| 16-09 | file_search Tool with max_num_results    | test_provider_request.py | TestFileSearchMaxNumResults          |
| 16-10 | web_search Disabled → No Tool            | test_web_search.py       | TestWebSearchDisabledByDefault       |
| 16-11 | max_tool_calls in Provider Request       | test_provider_request.py | TestMaxToolCalls                     |
| 16-12 | Cancelled Partial Message in Context     | —                        | GAP                                  |
| 16-13 | Empty Cancelled Turn → No Message        | —                        | GAP                                  |
| 16-14 | Tool Guard Instructions Appended         | —                        | GAP                                  |
| 16-15 | Missing System Prompt → None             | —                        | GAP                                  |

## 17 — Error Mapping & Sanitization

| ID    | Scenario                              | Test File             | Test Class                |
|-------|---------------------------------------|-----------------------|---------------------------|
| 17-01 | Pre-Stream Errors → JSON HTTP Error   | test_streaming.py     | TestStreamPreflightErrors |
| 17-02 | Post-Stream → SSE event: error        | test_error_mapping.py | TestErrorMapping          |
| 17-03 | Provider Timeout → provider_timeout   | test_error_mapping.py | TestErrorMapping          |
| 17-04 | Provider Unavailable → provider_error | test_error_mapping.py | TestErrorMapping          |
| 17-05 | Rate Limited → rate_limited           | test_error_mapping.py | TestErrorMapping          |
| 17-06 | Error Sanitization: No Provider IDs   | test_error_mapping.py | TestErrorMapping          |
| 17-07 | 404 Masking: AuthZ Denial → 404       | test_authorization.py | TestAuthorization         |

## 18 — Web Search

| ID    | Scenario                             | Test File                | Test Class                       |
|-------|--------------------------------------|--------------------------|----------------------------------|
| 18-01 | Web Search Tool Events               | test_web_search.py       | TestWebSearchBasic               |
| 18-02 | Web Search Citations                 | test_web_search.py       | TestWebSearchCitations           |
| 18-03 | Citations Before Done                | test_web_search.py       | TestWebSearchEventOrdering       |
| 18-04 | No Tools Without web_search Flag     | test_web_search.py       | TestWebSearchDisabledByDefault   |
| 18-05 | Works on Standard Model              | test_web_search.py       | TestWebSearchWithNonDefaultModel |
| 18-06 | Turn Done After Web Search           | test_web_search.py       | TestWebSearchTurnStatus          |
| 18-07 | Messages Persisted After Web Search  | test_web_search.py       | TestWebSearchTurnStatus          |
| 18-08 | Credits Tracked for Web Search Turns | test_web_search_usage.py | TestWebSearchUsageAccounting     |
| 18-09 | disable_web_search Kill Switch → 400 | test_principles.py       | TestPrinciples                   |
| 18-10 | Web Search Call Limits               | —                        | GAP                              |
| 18-11 | Meaningful Answer                    | test_web_search.py       | TestWebSearchOnline              |

## 19 — Cleanup & Recovery

| ID    | Scenario                               | Test File       | Test Class  |
|-------|----------------------------------------|-----------------|-------------|
| 19-01 | Chat Deletion → Background Cleanup     | test_cleanup.py | TestCleanup |
| 19-02 | Cleanup Worker Claims Pending          | test_cleanup.py | TestCleanup |
| 19-03 | Provider 404 on Delete → Success       | test_cleanup.py | TestCleanup |
| 19-04 | Vector Store Deleted After Attachments | test_cleanup.py | TestCleanup |
| 19-05 | Attachment Cleanup State Machine       | test_cleanup.py | TestCleanup |
| 19-06 | Orphan Watchdog Detects Stuck Turns    | test_cleanup.py | TestCleanup |
| 19-07 | Orphan → Estimated Settlement + Outbox | test_cleanup.py | TestCleanup |
| 19-08 | Crash Recovery: Turn Status API        | test_cleanup.py | TestCleanup |
| 19-09 | Thread Summary Trigger                 | test_cleanup.py | TestCleanup |
| 19-10 | Thread Summary Worker                  | test_cleanup.py | TestCleanup |

## 20 — Authorization

| ID    | Scenario                                   | Test File             | Test Class        |
|-------|--------------------------------------------|-----------------------|-------------------|
| 20-01 | PEP: PolicyEnforcer Before Every Operation | test_authorization.py | TestAuthorization |
| 20-02 | PDP Unreachable → 403 Fail-Closed          | test_authorization.py | TestAuthorization |
| 20-03 | PDP Denial → 404 Masking                   | test_authorization.py | TestAuthorization |
| 20-04 | Constraints Compiled to SQL WHERE          | test_authorization.py | TestAuthorization |
