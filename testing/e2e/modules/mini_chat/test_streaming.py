"""Tests for the streaming message endpoint (POST /v1/chats/{id}/messages:stream).

These tests hit a real LLM provider — they require valid API keys in .provider-keys
and a running server (started automatically or via run-server.sh --bg).
"""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL, expect_done, expect_stream_started, parse_sse, stream_message



class TestStreamBasic:
    """Basic streaming happy path."""

    def test_stream_returns_200_sse(self, provider_chat):
        status, events, _ = stream_message(provider_chat["id"], "Say hello in one word.")
        assert status == 200
        assert len(events) > 0

    def test_stream_has_terminal_done(self, provider_chat):
        """Stream must end with exactly one 'done' event."""
        status, events, raw = stream_message(provider_chat["id"], "Say hi.")
        assert status == 200
        terminal = [e for e in events if e.event in ("done", "error")]
        assert len(terminal) == 1
        assert terminal[0].event == "done"

    def test_stream_has_delta_events(self, provider_chat):
        """Stream should contain at least one delta with text content."""
        _, events, _ = stream_message(provider_chat["id"], "Tell me a one-line joke.")
        deltas = [e for e in events if e.event == "delta"]
        assert len(deltas) > 0
        for d in deltas:
            assert d.data["type"] == "text"
            assert isinstance(d.data["content"], str)

    def test_stream_assembled_text_nonempty(self, provider_chat):
        """Concatenated delta content should form a non-empty response."""
        _, events, _ = stream_message(provider_chat["id"], "What is 2+2? Answer in one word.")
        text = "".join(
            e.data["content"] for e in events if e.event == "delta"
        )
        assert len(text.strip()) > 0


class TestStreamDoneEvent:
    """Validate the 'done' event fields per DESIGN.md."""

    def test_done_has_required_fields(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        done = expect_done(events)
        d = done.data
        assert "effective_model" in d
        assert "selected_model" in d
        assert "quota_decision" in d
        assert d["quota_decision"] in ("allow", "downgrade")

    def test_done_has_usage(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        done = expect_done(events)
        usage = done.data.get("usage")
        assert usage is not None
        assert usage["input_tokens"] > 0
        assert usage["output_tokens"] > 0

    def test_done_does_not_have_message_id(self, provider_chat):
        """message_id moved to stream_started; done should not carry it."""
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        done = expect_done(events)
        assert "message_id" not in done.data

    def test_stream_started_has_message_id(self, provider_chat):
        """message_id is now in stream_started."""
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        ss = expect_stream_started(events)
        msg_id = ss.data.get("message_id")
        assert msg_id is not None
        uuid.UUID(msg_id)

    def test_done_effective_model_matches_chat(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        done = expect_done(events)
        # When no downgrade, effective == selected == chat model
        assert done.data["quota_decision"] == "allow"
        assert done.data["effective_model"] == provider_chat["model"]
        assert done.data["selected_model"] == provider_chat["model"]


class TestStreamEventOrdering:
    """SSE event ordering: ping* (delta|tool)* citations? (done|error)"""

    def test_no_events_after_terminal(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say hi.")
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
        _, events, _ = stream_message(provider_chat["id"], "Say hi briefly.")
        first_content_idx = None
        for i, e in enumerate(events):
            if e.event in ("delta", "tool"):
                first_content_idx = i
                break
        if first_content_idx is not None:
            for e in events[first_content_idx:]:
                if e.event == "ping":
                    pytest.fail("Ping after content events")


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
        assert "code" in body

    def test_empty_content_rejected(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": ""},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code == 400

    def test_missing_content_rejected(self, provider_chat):
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code in (400, 422)


class TestMessages:
    """Verify messages are persisted after streaming."""

    def test_messages_persisted_after_stream(self, provider_chat):
        chat_id = provider_chat["id"]
        _, events, _ = stream_message(chat_id, "Say exactly: PONG")
        assert any(e.event == "done" for e in events)

        # Fetch messages
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        roles = [m["role"] for m in msgs]
        assert "user" in roles
        assert "assistant" in roles

    def test_user_message_content_matches(self, provider_chat):
        prompt = "Say exactly: TEST_ECHO"
        chat_id = provider_chat["id"]
        stream_message(chat_id, prompt)

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        msgs = resp.json()["items"]
        user_msgs = [m for m in msgs if m["role"] == "user"]
        assert any(prompt in m["content"] for m in user_msgs)

    def test_assistant_message_has_tokens(self, provider_chat):
        chat_id = provider_chat["id"]
        stream_message(chat_id, "Say OK.")

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        msgs = resp.json()["items"]
        asst = [m for m in msgs if m["role"] == "assistant"]
        assert len(asst) >= 1
        # Token counts should be populated
        assert asst[0].get("input_tokens", 0) > 0 or asst[0].get("output_tokens", 0) > 0
