"""Tests for the turn status endpoint and turn lifecycle."""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, expect_done, stream_message



class TestTurnStatus:
    """GET /v1/chats/{id}/turns/{request_id}"""

    def test_turn_completed_after_stream(self, provider_chat):
        """After a successful stream, the turn should be in 'done' state."""
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say OK.", request_id=request_id)
        assert status == 200

        done = expect_done(events)
        assert done is not None

        # Check turn status via API
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["state"] == "done"
        assert body["request_id"] == request_id

    def test_turn_has_assistant_message_id(self, provider_chat):
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())
        status, _, _ = stream_message(chat_id, "Say OK.", request_id=request_id)
        assert status == 200

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}")
        body = resp.json()
        assert body.get("assistant_message_id") is not None

    def test_turn_not_found(self, provider_chat):
        fake_request_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{provider_chat['id']}/turns/{fake_request_id}")
        assert resp.status_code == 404


class TestIdempotency:
    """Idempotency via request_id."""

    def test_replay_completed_turn(self, provider_chat):
        """Sending the same request_id for a completed turn should replay."""
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())

        # First request
        s1, events1, _ = stream_message(chat_id, "Say HELLO.", request_id=request_id)
        assert s1 == 200
        assert any(e.event == "done" for e in events1)

        # Replay with same request_id
        s2, events2, _ = stream_message(chat_id, "Say HELLO.", request_id=request_id)
        assert s2 == 200
        # Replay should also have a done event
        assert any(e.event == "done" for e in events2)
