"""Tests for the streaming message endpoint (POST /v1/chats/{id}/messages:stream).

These tests hit a real LLM provider — they require valid API keys in .provider-keys
and a running server (started automatically or via run-server.sh --bg).
"""

import json
import uuid

import pytest
import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL, expect_done, expect_stream_started, parse_sse



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
        events = parse_sse(resp.text)
        assert len(events) > 0
        assert events[0].event == "stream_started"
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data
        assert ss.data.get("is_new_turn") is True

    def test_stream_has_terminal_done(self, provider_chat):
        """Stream must end with exactly one 'done' event."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say hi."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data
        assert ss.data.get("is_new_turn") is True
        terminal = [e for e in events if e.event in ("done", "error")]
        assert len(terminal) == 1
        assert terminal[0].event == "done"

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

    def test_done_has_required_fields(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        done = expect_done(events)
        d = done.data
        assert "effective_model" in d
        assert "selected_model" in d
        assert "quota_decision" in d
        assert d["quota_decision"] in ("allow", "downgrade")
        usage = d.get("usage", {})
        assert usage.get("input_tokens", 0) > 0, "done usage must have input_tokens > 0"
        assert usage.get("output_tokens", 0) > 0, "done usage must have output_tokens > 0"

    def test_done_has_usage(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        done = expect_done(events)
        assert "effective_model" in done.data, "done must have effective_model"
        assert "selected_model" in done.data, "done must have selected_model"
        assert done.data.get("quota_decision") in ("allow", "downgrade"), f"unexpected quota_decision: {done.data.get('quota_decision')}"
        usage = done.data.get("usage")
        assert usage is not None
        assert usage["input_tokens"] > 0
        assert usage["output_tokens"] > 0
        # Token breakdown fields (cache_read/write, reasoning) are internal-only
        # and not exposed in the SSE API.
        assert "cache_read_input_tokens" not in usage
        assert "cache_write_input_tokens" not in usage
        assert "reasoning_tokens" not in usage

    def test_done_does_not_have_message_id(self, provider_chat):
        """message_id moved to stream_started; done should not carry it."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        done = expect_done(events)
        assert "effective_model" in done.data, "done must have effective_model"
        assert "selected_model" in done.data, "done must have selected_model"
        assert done.data.get("quota_decision") in ("allow", "downgrade"), f"unexpected quota_decision: {done.data.get('quota_decision')}"
        usage = done.data.get("usage", {})
        assert usage.get("input_tokens", 0) > 0, "done usage must have input_tokens > 0"
        assert usage.get("output_tokens", 0) > 0, "done usage must have output_tokens > 0"
        assert "message_id" not in done.data

    # NOTE: tests stream_started, not done — kept in this class for historical reasons
    def test_stream_started_has_message_id(self, provider_chat):
        """message_id is now in stream_started."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        ss = expect_stream_started(events)
        msg_id = ss.data.get("message_id")
        assert msg_id is not None
        uuid.UUID(msg_id)

    def test_done_effective_model_matches_chat(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        done = expect_done(events)
        # When no downgrade, effective == selected == chat model
        assert done.data["quota_decision"] == "allow"
        assert done.data["effective_model"] == provider_chat["model"]
        assert done.data["selected_model"] == provider_chat["model"]
        usage = done.data.get("usage", {})
        assert usage.get("input_tokens", 0) > 0, "done usage must have input_tokens > 0"
        assert usage.get("output_tokens", 0) > 0, "done usage must have output_tokens > 0"


@pytest.mark.multi_provider
class TestStreamEventOrdering:
    """SSE event ordering: ping* (delta|tool)* citations? (done|error)"""

    def test_no_events_after_terminal(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say hi."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        terminal_idx = None
        for i, e in enumerate(events):
            if e.event in ("done", "error"):
                terminal_idx = i
                break
        assert terminal_idx is not None
        # Nothing after terminal
        assert terminal_idx == len(events) - 1

    def test_ping_only_before_content(self, provider_chat):
        """Pings should only appear before the first delta/tool."""
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "Say hi briefly."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        first_content_idx = None
        for i, e in enumerate(events):
            if e.event in ("delta", "tool"):
                first_content_idx = i
                break
        if first_content_idx is not None:
            for e in events[first_content_idx:]:
                if e.event == "ping":
                    pytest.fail("Ping after content events")


@pytest.mark.multi_provider
class TestStreamPreflightErrors:
    """Pre-stream errors should return JSON, not SSE."""

    def test_chat_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.post(
            f"{API_PREFIX}/chats/{fake_id}/messages:stream",
            json={"content": "hello"},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code == 404
        body = resp.json()
        assert "type" in body and "status" in body and "detail" in body

    def test_empty_content_rejected(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": ""},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code == 400
        body = resp.json()
        assert "type" in body and "status" in body and "detail" in body, f"Error response must be RFC 7807 format: {body}"

    def test_missing_content_rejected(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code in (400, 422)
        if resp.headers.get("content-type", "").startswith("application/json"):
            body = resp.json()
            assert "type" in body and "status" in body and "detail" in body

    def test_invalid_attachment_id_rejected(self, provider_chat):
        """04-04: Invalid attachment ID format should be rejected."""
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Hello", "attachment_ids": ["not-a-uuid"]},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert resp.status_code in (400, 422)

    def test_nonexistent_attachment_id_rejected(self, provider_chat):
        """04-05: Nonexistent attachment ID should be rejected."""
        import uuid
        chat_id = provider_chat["id"]
        fake_id = str(uuid.uuid4())
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Hello", "attachment_ids": [fake_id]},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert resp.status_code == 400


@pytest.mark.multi_provider
class TestMessages:
    """Verify messages are persisted after streaming."""

    def test_messages_persisted_after_stream(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Say exactly: PONG"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        assert any(e.event == "done" for e in events)

        # Fetch messages
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        roles = [m["role"] for m in msgs]
        assert "user" in roles
        assert "assistant" in roles

        user_msg = next(m for m in msgs if m["role"] == "user")
        assert user_msg.get("request_id") is not None, "request_id must be non-null"
        assert isinstance(user_msg.get("attachments"), list), "attachments must be an array"

        asst_msg = next(m for m in msgs if m["role"] == "assistant")
        assert asst_msg.get("request_id") is not None, "request_id must be non-null"
        assert isinstance(asst_msg.get("attachments"), list), "attachments must be an array"

    def test_user_message_content_matches(self, provider_chat):
        prompt = "Say exactly: TEST_ECHO"
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": prompt},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        user_msgs = [m for m in msgs if m["role"] == "user"]
        assert any(prompt in m["content"] for m in user_msgs)

    def test_assistant_message_has_tokens(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Say OK."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        asst = [m for m in msgs if m["role"] == "assistant"]
        assert len(asst) >= 1
        # Token counts should be populated
        assert asst[0].get("input_tokens", 0) > 0 and asst[0].get("output_tokens", 0) > 0
