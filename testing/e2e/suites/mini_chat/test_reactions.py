"""Tests for message reaction endpoints (like/dislike, upsert, remove)."""

import httpx
import pytest
from uuid import uuid4

from .conftest import API_PREFIX, parse_sse, expect_done, expect_stream_started, stream_message, DB_PATH
from .mock_provider.responses import Scenario, MockEvent, Usage


def _create_chat_with_assistant_message() -> tuple[str, str, str]:
    """Create a chat, send a message, return (chat_id, user_msg_id, assistant_msg_id)."""
    resp = httpx.post(f"{API_PREFIX}/chats", json={})
    assert resp.status_code == 201
    chat_id = resp.json()["id"]

    status, events, _ = stream_message(chat_id, "Hello")
    assert status == 200
    expect_done(events)

    msgs_resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
    assert msgs_resp.status_code == 200
    messages = msgs_resp.json()["items"]

    user_msg_id = None
    assistant_msg_id = None
    for m in messages:
        if m["role"] == "user":
            user_msg_id = m["id"]
        elif m["role"] == "assistant":
            assistant_msg_id = m["id"]

    assert user_msg_id is not None, "No user message found"
    assert assistant_msg_id is not None, "No assistant message found"
    return chat_id, user_msg_id, assistant_msg_id


class TestReactions:
    """PUT/DELETE /chats/{cid}/messages/{msg_id}/reaction"""

    def test_set_reaction_like(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()

        resp = httpx.put(
            f"{API_PREFIX}/chats/{chat_id}/messages/{assistant_msg_id}/reaction",
            json={"reaction": "like"},
        )
        assert resp.status_code == 200
        body = resp.json()
        assert body["message_id"] == assistant_msg_id
        assert body["reaction"] == "like"
        assert "created_at" in body

    def test_upsert_reaction_idempotent(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = f"{API_PREFIX}/chats/{chat_id}/messages/{assistant_msg_id}/reaction"

        # Set like
        resp1 = httpx.put(url, json={"reaction": "like"})
        assert resp1.status_code == 200
        assert resp1.json()["reaction"] == "like"

        # Upsert to dislike
        resp2 = httpx.put(url, json={"reaction": "dislike"})
        assert resp2.status_code == 200
        assert resp2.json()["reaction"] == "dislike"

        # Back to like — no error
        resp3 = httpx.put(url, json={"reaction": "like"})
        assert resp3.status_code == 200
        assert resp3.json()["reaction"] == "like"

    def test_reaction_on_user_message_400(self, server):
        chat_id, user_msg_id, _ = _create_chat_with_assistant_message()

        resp = httpx.put(
            f"{API_PREFIX}/chats/{chat_id}/messages/{user_msg_id}/reaction",
            json={"reaction": "like"},
        )
        assert resp.status_code == 400, (
            f"Expected 400 for reaction on user message, got {resp.status_code}: {resp.text}"
        )

    def test_remove_reaction_204(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = f"{API_PREFIX}/chats/{chat_id}/messages/{assistant_msg_id}/reaction"

        # Set like first
        resp = httpx.put(url, json={"reaction": "like"})
        assert resp.status_code == 200

        # Delete reaction
        del_resp = httpx.delete(url)
        assert del_resp.status_code == 204

        # Verify reaction is gone via GET messages
        msgs_resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert msgs_resp.status_code == 200
        found = False
        for m in msgs_resp.json()["items"]:
            if m["id"] == assistant_msg_id:
                found = True
                assert m.get("my_reaction") is None, (
                    f"Expected my_reaction to be null after deletion, got: {m.get('my_reaction')}"
                )
                break
        assert found, f"Assistant message {assistant_msg_id} not found in messages list"

    def test_remove_reaction_idempotent(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()

        # Delete without ever setting a reaction — should be 204
        resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/messages/{assistant_msg_id}/reaction",
        )
        assert resp.status_code == 204
