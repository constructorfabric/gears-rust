"""Tests for the turn status endpoint and turn lifecycle."""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, RESOURCE_TURN, assert_problem, expect_done, expect_stream_started, stream_message


@pytest.mark.multi_provider
class TestTurnStatus:
    """GET /v1/chats/{id}/turns/{request_id}"""

    def test_turn_completed_after_stream(self, provider_chat):
        """After a successful stream, the turn should be in 'done' state."""
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say OK.", request_id=request_id)
        assert status == 200

        expect_done(events)
        message_id = expect_stream_started(events).data["message_id"]

        # Check turn status via API
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["state"] == "done"
        assert body["request_id"] == request_id
        assert "updated_at" in body, "turn status must have updated_at"
        assert body["assistant_message_id"] == message_id

    def test_turn_not_found(self, provider_chat):
        fake_request_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{provider_chat['id']}/turns/{fake_request_id}")
        body = assert_problem(resp, 404, "not_found", resource_type=RESOURCE_TURN)
        assert body["context"]["resource_name"] == fake_request_id, body
