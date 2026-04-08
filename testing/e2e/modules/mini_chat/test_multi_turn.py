"""Tests for multi-turn conversation and message history."""

import httpx

from .conftest import API_PREFIX, stream_message

import pytest


class TestMultiTurn:
    """Multiple messages in the same chat."""

    @pytest.mark.online_only
    def test_two_turns_in_sequence(self, provider_chat):
        chat_id = provider_chat["id"]

        # Turn 1
        s1, ev1, _ = stream_message(chat_id, "Remember the number 42.")
        assert s1 == 200
        assert any(e.event == "done" for e in ev1)

        # Turn 2 — model must recall from context
        s2, ev2, _ = stream_message(chat_id, "What number did I ask you to remember?")
        assert s2 == 200
        assert any(e.event == "done" for e in ev2)

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

    def test_message_count_increments(self, provider_chat):
        chat_id = provider_chat["id"]

        stream_message(chat_id, "Hello.")

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 200
        assert resp.json()["message_count"] == 2  # user + assistant

    def test_messages_ordered_chronologically(self, provider_chat):
        chat_id = provider_chat["id"]

        stream_message(chat_id, "First message.")
        stream_message(chat_id, "Second message.")

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        msgs = resp.json()["items"]
        timestamps = [m["created_at"] for m in msgs]
        assert timestamps == sorted(timestamps)
