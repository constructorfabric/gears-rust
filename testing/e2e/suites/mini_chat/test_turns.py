"""Tests for the turn status endpoint and turn lifecycle."""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, expect_done, expect_stream_started, parse_sse, stream_message



@pytest.mark.multi_provider
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
        assert "updated_at" in body, "turn status must have updated_at"
        assert body.get("assistant_message_id") is not None, "done turn must have assistant_message_id"

    def test_turn_has_assistant_message_id(self, provider_chat):
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())
        status, _, _ = stream_message(chat_id, "Say OK.", request_id=request_id)
        assert status == 200

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body.get("assistant_message_id") is not None

    def test_turn_not_found(self, provider_chat):
        fake_request_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{provider_chat['id']}/turns/{fake_request_id}")
        assert resp.status_code == 404


@pytest.mark.multi_provider
class TestIdempotency:
    """Idempotency via request_id."""

    def test_replay_completed_turn(self, provider_chat):
        """Sending the same request_id for a completed turn should replay."""
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())

        # First request
        _url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        _resp1 = httpx.post(_url, json={"content": "Say HELLO.", "request_id": request_id}, headers={"Accept": "text/event-stream"}, timeout=90)
        s1 = _resp1.status_code
        events1 = parse_sse(_resp1.text) if s1 == 200 else []
        assert s1 == 200
        expect_done(events1)
        ss1 = expect_stream_started(events1)

        # Replay with same request_id
        _resp2 = httpx.post(_url, json={"content": "Say HELLO.", "request_id": request_id}, headers={"Accept": "text/event-stream"}, timeout=90)
        s2 = _resp2.status_code
        events2 = parse_sse(_resp2.text) if s2 == 200 else []
        assert s2 == 200
        expect_done(events2)
        ss2 = expect_stream_started(events2)

        # Replay must return is_new_turn=false and same message_id
        assert ss2.data["is_new_turn"] is False, "Replay should have is_new_turn=false"
        assert ss2.data["message_id"] == ss1.data["message_id"], (
            f"Replay message_id mismatch: {ss2.data['message_id']} != {ss1.data['message_id']}"
        )
