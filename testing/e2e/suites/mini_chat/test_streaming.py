"""Tests for the streaming message endpoint (POST /v1/chats/{id}/messages:stream):
SSE contract, pre-stream errors and message persistence."""

import dataclasses
import threading
import time
import uuid

import pytest
import httpx

from .conftest import (
    API_PREFIX,
    CATALOG_SYSTEM_PROMPT,
    DEFAULT_MODEL,
    NO_INPUT_LIMIT_MODEL,
    RESOURCE_CHAT,
    TINY_CTX_MODEL,
    assert_problem,
    delta_text,
    exec_db,
    expect_done,
    expect_stream_started,
    parse_sse,
    provider_input,
    query_db,
    slow_scenario,
    turn_count,
    uuid_from_db,
    wait_for,
)
from .mock_provider.responses import SCENARIOS, MockEvent
from .mock_provider.server import FILES_PATH
from .test_attachments import _chunked_multipart, _upload, _upload_ready


@pytest.mark.multi_provider
class TestStreamBasic:
    """Basic streaming happy path."""

    def test_stream_returns_200_sse(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say hello in one word."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        assert resp.headers["content-type"].startswith("text/event-stream"), resp.headers
        events = parse_sse(resp.text)
        assert len(events) > 0
        assert events[0].event == "stream_started"
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data
        assert ss.data.get("is_new_turn") is True

    def test_stream_has_delta_events(self, provider_chat):
        """Stream should contain at least one delta with text content."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Tell me a one-line joke."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data
        assert ss.data.get("is_new_turn") is True
        deltas = [e for e in events if e.event == "delta"]
        assert len(deltas) > 0
        for d in deltas:
            assert d.data["type"] == "text"
            assert isinstance(d.data["content"], str)

    def test_stream_assembled_text_nonempty(self, provider_chat):
        """Concatenated delta content should form a non-empty response."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "What is 2+2? Answer in one word."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data
        assert ss.data.get("is_new_turn") is True
        text = "".join(
            e.data["content"] for e in events if e.event == "delta"
        )
        assert len(text.strip()) > 0


@pytest.mark.multi_provider
class TestStreamDoneEvent:
    """Validate the 'done' event fields per DESIGN.md."""

    def test_done_event_contract(self, provider_chat):
        """`done` carries models, quota decision and usage; no message_id, no internal token fields."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        d = expect_done(parse_sse(resp.text)).data
        assert d["effective_model"] == provider_chat["model"]
        assert d["selected_model"] == provider_chat["model"]
        assert d["quota_decision"] == "allow"
        assert "message_id" not in d, "message_id belongs to stream_started"
        usage = d["usage"]
        assert usage["input_tokens"] > 0
        assert usage["output_tokens"] > 0
        # Token breakdown fields are internal-only and not exposed in the SSE API.
        for internal in ("cache_read_input_tokens", "cache_write_input_tokens", "reasoning_tokens"):
            assert internal not in usage


class TestStreamPing:
    """`ping` keepalives are sent only while the stream waits for the first content."""

    @pytest.mark.timeout(30)
    def test_ping_only_before_content(self, request, chat, mock_provider):
        """No content reaches the client for about 8 s (sse_ping_interval_seconds
        is 5 in base.yaml): at least one ping arrives, and none after the
        first delta. The mock sends `response.in_progress` (no client event)
        after 4 s and the first delta 4 s later, so each silent gap of the
        provider stream stays well under the OAGW idle timeout
        (proxy_timeout_secs 8) while the client wait is 3 s over the ping
        interval."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (delayed scenario)")
        scenario = slow_scenario(3, slow=0.3)
        scenario.events = [
            MockEvent("response.in_progress", {"response": {"status": "in_progress"}}, delay=4.0),
            dataclasses.replace(scenario.events[0], delay=4.0),
            *scenario.events[1:],
        ]
        mock_provider.set_next_scenario(scenario)

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            json={"content": "Think first."},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert resp.status_code == 200
        types = [e.event for e in parse_sse(resp.text)]
        first_delta = types.index("delta")
        assert types[0] == "stream_started"
        assert "ping" in types[1:first_delta], types
        assert "ping" not in types[first_delta:], types
        assert types[-1] == "done"


class TestStreamPreflightErrors:
    """Pre-stream errors should return JSON, not SSE."""

    def test_chat_not_found(self, server):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{uuid.uuid4()}/messages:stream",
            json={"content": "hello"},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert_problem(resp, 404, "not_found")

    def test_malformed_json_rejected(self, chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            content=b"{not json",
            headers={"Accept": "text/event-stream", "Content-Type": "application/json"},
            timeout=10,
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="json_syntax_error")

    @pytest.mark.multi_provider
    def test_empty_content_rejected(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": ""},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="EMPTY_CONTENT")

    def test_whitespace_only_content_rejected(self, chat):
        """Content of only whitespace is empty after trimming: 400
        invalid_argument EMPTY_CONTENT on `content`, no turn."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            json={"content": "  \n\t "},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        body = assert_problem(
            resp, 400, "invalid_argument", field_reason="EMPTY_CONTENT", resource_type=RESOURCE_CHAT,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["content"], body
        assert turn_count(chat["id"]) == 0

    @pytest.mark.multi_provider
    def test_missing_content_rejected(self, provider_chat):
        """A body that does not match the schema is 422 invalid_argument
        (platform JSON extractor; malformed JSON would be 400)."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert_problem(resp, 422, "invalid_argument")

    @pytest.mark.multi_provider
    def test_malformed_attachment_id_rejected(self, provider_chat):
        """04-04: an attachment id that is not a UUID fails body deserialization: 422."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Hello", "attachment_ids": ["not-a-uuid"]},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert_problem(resp, 422, "invalid_argument")

    @pytest.mark.multi_provider
    @pytest.mark.usefixtures("offline_only")
    def test_nonexistent_attachment_id_rejected(self, provider_chat, mock_provider):
        """04-05: an unknown attachment id is 400 invalid_attachment; the provider is not called."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Hello", "attachment_ids": [str(uuid.uuid4())]},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="invalid_attachment")
        assert mock_provider.get_last_request() is None


def _post_stream(chat_id: str, body: dict) -> httpx.Response:
    return httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/messages:stream", json=body,
        headers={"Accept": "text/event-stream"}, timeout=30,
    )


@pytest.mark.usefixtures("offline_only")
class TestProviderEventsWithoutEventLine:
    """A provider stream whose SSE events have no `event:` line: each event
    is named only by its `data.type`, as every Responses event carries it."""

    def test_plain_answer(self, chat, mock_provider):
        mock_provider.set_next_scenario(
            dataclasses.replace(SCENARIOS["*"], omit_event_lines=True),
        )
        rid = str(uuid.uuid4())
        resp = _post_stream(chat["id"], {"content": "Hello.", "request_id": rid})
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        expect_done(events)
        assert delta_text(events) == "Hello! How can I help?"
        rows = query_db("SELECT state FROM chat_turns WHERE request_id = ?", (rid,))
        assert rows == [{"state": "completed"}], rows

    def test_web_search_answer(self, chat, mock_provider):
        """Tool and citation events are dispatched by `data.type` too."""
        mock_provider.set_next_scenario(
            dataclasses.replace(SCENARIOS["SEARCH:*"], omit_event_lines=True),
        )
        resp = _post_stream(chat["id"], {
            "content": "SEARCH: weather", "web_search": {"enabled": True},
        })
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        expect_done(events)
        assert [(e.data["name"], e.data["phase"]) for e in events if e.event == "tool"] == [
            ("web_search", "start"), ("web_search", "done"),
        ]
        (citations,) = [e.data for e in events if e.event == "citations"]
        assert [c["url"] for c in citations["items"]] == ["https://example.com"], citations


@pytest.mark.usefixtures("offline_only")
class TestStreamInvalidAttachments:
    """`attachment_ids` that exist but cannot be used: 400 invalid_attachment,
    no turn row, the provider is not called (ADR-0004: invalid, foreign or
    not-ready attachment_ids). Offline only: checks the mock's traffic."""

    def _assert_rejected(self, chat_id: str, attachment_ids: list[str], mock_provider) -> None:
        mock_provider.clear_captured_requests()
        resp = _post_stream(chat_id, {"content": "Use the file.", "attachment_ids": attachment_ids})
        assert_problem(resp, 400, "invalid_argument", field_reason="invalid_attachment")
        assert mock_provider.get_captured_requests() == []
        assert turn_count(chat_id) == 0

    def test_attachment_of_other_chat_rejected(self, chat, chat_with_model, mock_provider):
        other_chat = chat_with_model(DEFAULT_MODEL)["id"]
        att_id = _upload_ready(other_chat, "other.txt", b"other chat document", "text/plain")
        self._assert_rejected(chat["id"], [att_id], mock_provider)

    def test_failed_attachment_rejected(self, chat, mock_provider):
        """An attachment whose provider upload failed (status `failed`) is not ready."""
        chat_id = chat["id"]
        mock_provider.set_fault("POST", FILES_PATH, 500)
        assert _upload(chat_id, "fail.txt", b"upload fails", "text/plain").status_code == 503
        rows = query_db("SELECT id, status FROM attachments WHERE chat_id = ?", (chat_id,))
        assert [r["status"] for r in rows] == ["failed"], rows
        self._assert_rejected(chat_id, [uuid_from_db(rows[0]["id"])], mock_provider)

    def test_duplicate_attachment_ids_rejected(self, chat, mock_provider):
        chat_id = chat["id"]
        att_id = _upload_ready(chat_id, "dup.txt", b"one document", "text/plain")
        self._assert_rejected(chat_id, [att_id, att_id], mock_provider)

    def test_too_many_attachment_ids_rejected(self, chat, mock_provider):
        """More IDs than a valid message can reference (default
        rag.max_documents_per_chat 50 + rag.max_images_per_message 4 = 54)
        are rejected before any attachment query."""
        ids = [str(uuid.uuid4()) for _ in range(55)]
        self._assert_rejected(chat["id"], ids, mock_provider)

    def test_deleted_attachment_rejected(self, chat, mock_provider):
        """An attachment removed by DELETE /attachments/{id}."""
        chat_id = chat["id"]
        att_id = _upload_ready(chat_id, "gone.txt", b"deleted document", "text/plain")
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 204, resp.text
        self._assert_rejected(chat_id, [att_id], mock_provider)

    @pytest.mark.timeout(30)
    def test_pending_attachment_rejected(self, chat, mock_provider):
        """An attachment whose upload is still in progress (status `pending`:
        the upload has sent its multipart headers and holds the rest of the
        body). The upload then completes `ready`."""
        chat_id = chat["id"]
        release = threading.Event()
        result: dict[str, httpx.Response] = {}
        content_type, body = _chunked_multipart("held.txt", "text/plain", b"held upload")
        head = next(body)

        def gated():
            yield head
            release.wait(20)
            yield from body

        def held_upload():
            result["resp"] = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/attachments",
                content=gated(), headers={"Content-Type": content_type}, timeout=30,
            )

        thread = threading.Thread(target=held_upload)
        thread.start()
        try:
            rows = wait_for(
                lambda: query_db("SELECT id, status FROM attachments WHERE chat_id = ?", (chat_id,)),
                "the attachment row of the held upload",
            )
            assert [r["status"] for r in rows] == ["pending"], rows
            self._assert_rejected(chat_id, [uuid_from_db(rows[0]["id"])], mock_provider)
        finally:
            release.set()
            thread.join(timeout=30)
        assert result["resp"].status_code == 201, result["resp"].text
        assert result["resp"].json()["status"] == "ready"

    @pytest.mark.timeout(30)
    def test_uploaded_attachment_rejected(self, chat, mock_provider):
        """A document stored at the provider while the vector store is still
        indexing it: GET reports `uploaded`, and a send with it is rejected.
        Released, the upload completes `ready`."""
        chat_id = chat["id"]
        mock_provider.hold_indexing()
        result: dict[str, httpx.Response] = {}

        def upload():
            result["resp"] = _upload(chat_id, "indexing.txt", b"being indexed", "text/plain")

        thread = threading.Thread(target=upload)
        thread.start()
        try:
            rows = wait_for(
                lambda: query_db(
                    "SELECT id FROM attachments WHERE chat_id = ? AND status = 'uploaded'",
                    (chat_id,),
                ),
                "the attachment in `uploaded` while indexing is polled",
            )
            att_id = uuid_from_db(rows[0]["id"])
            detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
            assert detail.status_code == 200, detail.text
            assert detail.json()["status"] == "uploaded", detail.json()
            self._assert_rejected(chat_id, [att_id], mock_provider)
        finally:
            mock_provider.hold_indexing(False)
            thread.join(timeout=30)
        assert result["resp"].status_code == 201, result["resp"].text
        assert result["resp"].json()["status"] == "ready"


    @pytest.mark.timeout(90)
    def test_indexing_past_request_deadline_returns_uploaded_then_ready(
        self, chat, mock_provider,
    ):
        """Indexing still `in_progress` when the upload's 25 s deadline passes:
        the upload answers 201 `uploaded`, a send with it is rejected, and
        once the vector store reports `completed` the background wait makes
        the attachment `ready` and usable."""
        chat_id = chat["id"]
        mock_provider.hold_indexing()
        try:
            started = time.monotonic()
            resp = _upload(chat_id, "slow-index.txt", b"slowly indexed", "text/plain")
            assert resp.status_code == 201, resp.text
            assert resp.json()["status"] == "uploaded", resp.json()
            assert time.monotonic() - started >= 20, "returned before the request deadline"
            att_id = resp.json()["id"]
            self._assert_rejected(chat_id, [att_id], mock_provider)
        finally:
            mock_provider.hold_indexing(False)
        detail = wait_for(
            lambda: (
                d if (d := httpx.get(
                    f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10,
                ).json())["status"] != "uploaded" else None
            ),
            "the background indexing wait to settle the attachment",
            timeout=30,
        )
        assert detail["status"] == "ready", detail


@pytest.mark.usefixtures("offline_only")
class TestStreamInputLimits:
    """Token limits checked before the turn is created, on the small-context
    model gpt-4.1-mini-tiny-ctx (base.yaml: context_window 4096,
    max_output_tokens 1024, max_input_tokens 3000; 4 bytes per token,
    500 fixed overhead tokens per item, 10% safety margin)."""

    def _assert_rejected(self, chat_id: str, content: str, reason: str, mock_provider) -> None:
        mock_provider.clear_captured_requests()
        resp = _post_stream(chat_id, {"content": content})
        assert_problem(resp, 400, "out_of_range", field_reason=reason)
        assert mock_provider.get_captured_requests() == []
        assert turn_count(chat_id) == 0

    def test_message_over_max_input_tokens_400(self, chat_with_model, mock_provider):
        """01-08: 12000 bytes are estimated at (3000 + 500) * 1.1 = 3850 tokens
        > max_input_tokens 3000: 400 out_of_range INPUT_TOO_LONG."""
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        self._assert_rejected(chat_id, "x" * 12_000, "INPUT_TOO_LONG", mock_provider)

    def test_mandatory_context_over_budget_400(self, chat_with_model, mock_provider):
        """01-08: 6000 bytes (2200 tokens) pass max_input_tokens, but with the
        system prompt (587 tokens) they exceed the context budget
        min(3000, 4096 - 1024) - 500 = 2500 (see
        test_mandatory_context_capped_by_max_input_tokens_400): 400
        out_of_range CONTEXT_BUDGET_EXCEEDED."""
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        self._assert_rejected(chat_id, "x" * 6_000, "CONTEXT_BUDGET_EXCEEDED", mock_provider)

    def test_mandatory_context_capped_by_max_input_tokens_400(self, chat_with_model, mock_provider):
        """01-08: the context budget is capped by max_input_tokens.

        Budget (context_assembly.rs `compute_available_budget`), no tools:
          min(max_input_tokens, context_window - max_output_tokens_applied)
            - fixed_overhead_tokens = min(3000, 4096 - 1024) - 500 = 2500.
        Without the max_input_tokens cap it would be 3072 - 500 = 2572.

        Mandatory context (`estimate_item_tokens` per item:
        (ceil(bytes / 4) + 500) * 110 / 100, integer division):
          system prompt, 134 bytes: (34 + 500) * 1.1 = 587
          message, 5000 bytes: (1250 + 500) * 1.1 = 1925
          total 2512: over 2500, within 2572.
        The message alone (1925, `estimate_tokens`) is under
        max_input_tokens 3000, so INPUT_TOO_LONG does not apply: 400
        out_of_range CONTEXT_BUDGET_EXCEEDED."""
        assert len(CATALOG_SYSTEM_PROMPT.encode()) == 134
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        self._assert_rejected(chat_id, "x" * 5_000, "CONTEXT_BUDGET_EXCEEDED", mock_provider)

    def test_max_input_tokens_zero_is_no_limit(self, chat_with_model, mock_provider):
        """01-08: `max_input_tokens: 0` sets no separate input limit. On
        NO_INPUT_LIMIT_MODEL (TINY_CTX_MODEL with max_input_tokens 0) the
        budget is 4096 - 1024 - 500 = 2572, so the 2512-token mandatory
        context that test_mandatory_context_capped_by_max_input_tokens_400
        rejects on TINY_CTX_MODEL is sent."""
        chat_id = chat_with_model(NO_INPUT_LIMIT_MODEL)["id"]
        content = "x" * 5_000
        mock_provider.clear_captured_requests()
        resp = _post_stream(chat_id, {"content": content})
        assert resp.status_code == 200, resp.text
        expect_done(parse_sse(resp.text))
        (req,) = mock_provider.get_captured_requests()
        assert provider_input(req) == [("user", content)]

    def test_max_input_tokens_zero_skips_input_too_long(self, chat_with_model, mock_provider):
        """01-08: 12000 bytes (3850 tokens, INPUT_TOO_LONG on TINY_CTX_MODEL)
        on NO_INPUT_LIMIT_MODEL are not checked against an input limit; with
        the system prompt (4437 tokens) they exceed the 2572-token context
        budget: 400 out_of_range CONTEXT_BUDGET_EXCEEDED."""
        chat_id = chat_with_model(NO_INPUT_LIMIT_MODEL)["id"]
        mock_provider.clear_captured_requests()
        resp = _post_stream(chat_id, {"content": "x" * 12_000})
        body = assert_problem(resp, 400, "out_of_range", field_reason="CONTEXT_BUDGET_EXCEEDED")
        assert [v["reason"] for v in body["context"]["field_violations"]] == [
            "CONTEXT_BUDGET_EXCEEDED",
        ], body
        assert mock_provider.get_captured_requests() == []
        assert turn_count(chat_id) == 0


@pytest.mark.usefixtures("offline_only")
class TestChatModelLeftCatalog:
    """The chat's model is no longer in the catalog (not just disabled:
    a disabled model is downgraded, test_quota_policy.py
    TestDowngrade::test_disabled_chat_model_downgrades)."""

    def test_send_to_chat_with_model_missing_from_catalog_400(self, chat, mock_provider):
        """The chat row points at a model id the catalog does not have
        (seeded in the DB, as after a catalog change): sending is 400
        invalid_argument INVALID_MODEL, before any turn or provider call."""
        chat_id = chat["id"]
        assert exec_db(
            "UPDATE chats SET model = ? WHERE id = ?", ("gpt-removed-from-catalog", chat_id),
        ) == 1
        mock_provider.clear_captured_requests()
        resp = _post_stream(chat_id, {"content": "Anyone there?"})
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_MODEL", resource_type=RESOURCE_CHAT,
        )
        assert mock_provider.get_captured_requests() == []
        assert turn_count(chat_id) == 0


@pytest.mark.multi_provider
class TestMessages:
    """Verify messages are persisted after streaming."""

    def test_messages_persisted_after_stream(self, provider_chat):
        """04-09: after `done` the chat holds exactly the user message and the
        answer, in that order, both with the turn's request_id and an
        attachments array; the answer is the text of the `delta` events."""
        chat_id = provider_chat["id"]
        prompt = "Say exactly: PONG"
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": prompt},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        expect_done(events)
        request_id = expect_stream_started(events).data["request_id"]

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        assert [(m["role"], m["content"], m["request_id"], m["attachments"]) for m in msgs] == [
            ("user", prompt, request_id, []),
            ("assistant", delta_text(events), request_id, []),
        ], msgs

    def test_user_message_content_matches(self, provider_chat):
        """04-09: the user message is stored with the content that was sent."""
        prompt = "Say exactly: TEST_ECHO"
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": prompt},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        expect_done(parse_sse(resp.text))

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        user_msgs = [m["content"] for m in resp.json()["items"] if m["role"] == "user"]
        assert user_msgs == [prompt]

    def test_assistant_message_has_tokens(self, provider_chat):
        """The assistant message stores the token counts of the turn's `done` usage."""
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        usage = expect_done(parse_sse(resp.text)).data["usage"]
        assert usage["input_tokens"] > 0 and usage["output_tokens"] > 0, usage

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        asst = [m for m in resp.json()["items"] if m["role"] == "assistant"]
        assert len(asst) == 1
        assert (asst[0]["input_tokens"], asst[0]["output_tokens"]) == (
            usage["input_tokens"], usage["output_tokens"],
        )
