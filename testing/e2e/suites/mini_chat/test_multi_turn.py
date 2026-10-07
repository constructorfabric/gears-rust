"""Tests for multi-turn conversation and message history."""

import httpx

from .conftest import (
    API_PREFIX,
    delta_text,
    expect_done,
    expect_stream_started,
    list_messages,
    stream_message,
)

import pytest


@pytest.mark.multi_provider
class TestMultiTurn:
    """Multiple messages in the same chat."""

    def test_message_count_increments(self, provider_chat):
        """`message_count` is 0 for a new chat and grows by 2 (user + assistant) per turn."""
        chat_id = provider_chat["id"]
        assert provider_chat["message_count"] == 0

        for turn, content in enumerate(("Hello.", "Hello again."), start=1):
            status, events, raw = stream_message(chat_id, content)
            assert status == 200, raw
            expect_done(events)

            resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
            assert resp.status_code == 200
            assert resp.json()["message_count"] == 2 * turn

    def test_messages_ordered_chronologically(self, provider_chat):
        """03-12: two completed turns are listed oldest first: the first
        question, its answer, the second question, its answer."""
        chat_id = provider_chat["id"]
        answers = []
        request_ids = []
        for content in ("First message.", "Second message."):
            status, events, raw = stream_message(chat_id, content)
            assert status == 200, raw
            expect_done(events)
            request_ids.append(expect_stream_started(events).data["request_id"])
            answers.append(delta_text(events))

        msgs = list_messages(chat_id)
        assert [(m["role"], m["request_id"], m["content"]) for m in msgs] == [
            ("user", request_ids[0], "First message."),
            ("assistant", request_ids[0], answers[0]),
            ("user", request_ids[1], "Second message."),
            ("assistant", request_ids[1], answers[1]),
        ]
        timestamps = [m["created_at"] for m in msgs]
        assert timestamps == sorted(timestamps)
