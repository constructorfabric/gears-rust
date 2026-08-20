"""Tests for multi-turn conversation and message history."""

import httpx

from .conftest import API_PREFIX, parse_sse, expect_done, expect_stream_started

import pytest


@pytest.mark.multi_provider
class TestMultiTurn:
    """Multiple messages in the same chat."""

    @pytest.mark.online_only
    def test_two_turns_in_sequence(self, provider_chat):
        chat_id = provider_chat["id"]

        # Turn 1
        _resp1 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Remember the number 42."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        s1 = _resp1.status_code
        ev1 = parse_sse(_resp1.text) if s1 == 200 else []
        assert s1 == 200
        assert any(e.event == "done" for e in ev1)
        ss1 = expect_stream_started(ev1)
        assert "request_id" in ss1.data
        assert "message_id" in ss1.data
        assert ss1.data.get("is_new_turn") is True

        # Turn 2 — model must recall from context
        _resp2 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What number did I ask you to remember?"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        s2 = _resp2.status_code
        ev2 = parse_sse(_resp2.text) if s2 == 200 else []
        assert s2 == 200
        assert any(e.event == "done" for e in ev2)
        ss2 = expect_stream_started(ev2)
        assert "request_id" in ss2.data
        assert "message_id" in ss2.data
        assert ss2.data.get("is_new_turn") is True

        # Verify the model actually recalled the number (proves context assembly works)
        text2 = "".join(e.data["content"] for e in ev2 if e.event == "delta")
        assert "42" in text2, (
            f"Model should recall '42' from conversation history. Got: {text2!r}"
        )

        # Check message history has 4 messages (2 user + 2 assistant)
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        assert len(msgs) == 4
        roles = [m["role"] for m in msgs]
        assert roles == ["user", "assistant", "user", "assistant"]

        first_msg = msgs[0]
        assert first_msg.get("request_id") is not None, "request_id must be non-null"
        assert isinstance(first_msg.get("attachments"), list), "attachments must be an array"

    def test_message_count_increments(self, provider_chat):
        chat_id = provider_chat["id"]

        stream_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Hello."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert stream_resp.status_code == 200
        events = parse_sse(stream_resp.text)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data
        assert ss.data.get("is_new_turn") is True

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 200
        assert resp.json()["message_count"] == 2  # user + assistant

    def test_messages_ordered_chronologically(self, provider_chat):
        chat_id = provider_chat["id"]

        sr1 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "First message."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert sr1.status_code == 200
        ev1 = parse_sse(sr1.text)
        ss1 = expect_stream_started(ev1)
        assert "request_id" in ss1.data
        assert "message_id" in ss1.data
        assert ss1.data.get("is_new_turn") is True

        sr2 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Second message."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert sr2.status_code == 200
        ev2 = parse_sse(sr2.text)
        ss2 = expect_stream_started(ev2)
        assert "request_id" in ss2.data
        assert "message_id" in ss2.data
        assert ss2.data.get("is_new_turn") is True

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        timestamps = [m["created_at"] for m in msgs]
        assert timestamps == sorted(timestamps)

        first_msg = msgs[0]
        assert first_msg.get("request_id") is not None, "request_id must be non-null"
        assert isinstance(first_msg.get("attachments"), list), "attachments must be an array"
