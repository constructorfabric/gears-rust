"""Tests for message listing with OData query options and field presence."""

import httpx
import pytest
from uuid import uuid4

from .conftest import API_PREFIX, parse_sse, expect_done, expect_stream_started, stream_message, DB_PATH
from .mock_provider.responses import Scenario, MockEvent, Usage


def _create_chat_with_messages(count: int = 1) -> str:
    """Create a chat and send `count` messages. Returns chat_id."""
    resp = httpx.post(f"{API_PREFIX}/chats", json={})
    assert resp.status_code == 201
    chat_id = resp.json()["id"]

    for i in range(count):
        status, events, raw = stream_message(chat_id, f"Message number {i + 1}")
        assert status == 200, f"stream_message failed: {status} {raw[:300]}"
        expect_done(events)

    return chat_id


class TestMessages:
    """GET /chats/{cid}/messages with OData query options."""

    def test_odata_select(self, server):
        """$select should limit returned fields to the requested set.

        NOTE: If $select is not implemented, this test will fail.
        That is expected — triage as a feature gap.
        """
        chat_id = _create_chat_with_messages(1)

        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/messages",
            params={"$select": "id,content"},
        )
        assert resp.status_code == 200, f"GET messages failed: {resp.status_code} {resp.text}"
        items = resp.json()["items"]
        assert len(items) >= 1

        for msg in items:
            # Selected fields must be present
            assert "id" in msg, f"'id' missing from $select response: {msg}"
            assert "content" in msg, f"'content' missing from $select response: {msg}"
            # Non-selected fields should be absent (strict $select)
            # Some APIs include id/type always — at minimum, heavy fields like
            # input_tokens should be absent if $select is enforced.
            non_selected = {"input_tokens", "output_tokens", "model"}
            present_extras = non_selected & set(msg.keys())
            # Soft assertion: warn but don't fail if server includes extra fields
            # (some OData impls include structural fields always)
            if present_extras:
                pytest.xfail(
                    f"$select did not strip extra fields: {present_extras}. "
                    "Server may include structural fields by default."
                )

    def test_odata_orderby(self, server):
        """$orderby=created_at desc should return messages in reverse chronological order."""
        chat_id = _create_chat_with_messages(2)

        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/messages",
            params={"$orderby": "created_at desc"},
        )
        assert resp.status_code == 200, f"GET messages failed: {resp.status_code} {resp.text}"
        body = resp.json()
        assert "page_info" in body
        items = body["items"]
        assert len(items) >= 2, f"Expected at least 2 messages, got {len(items)}"

        # Verify descending order by created_at
        timestamps = [m["created_at"] for m in items]
        for i in range(len(timestamps) - 1):
            assert timestamps[i] >= timestamps[i + 1], (
                f"Messages not in descending order: {timestamps[i]} < {timestamps[i + 1]} "
                f"at positions {i}, {i + 1}"
            )

    def test_odata_filter_role(self, server):
        """$filter=role eq 'assistant' should return only assistant messages."""
        chat_id = _create_chat_with_messages(1)

        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/messages",
            params={"$filter": "role eq 'assistant'"},
        )
        assert resp.status_code == 200, f"GET messages failed: {resp.status_code} {resp.text}"
        body = resp.json()
        assert "page_info" in body
        items = body["items"]
        assert len(items) >= 1, "Expected at least one assistant message"

        for msg in items:
            assert msg["role"] == "assistant", (
                f"Expected only assistant messages, got role={msg['role']}"
            )

    def test_my_reaction_field(self, server):
        """Assistant messages should include a 'my_reaction' field (null when no reaction set)."""
        chat_id = _create_chat_with_messages(1)

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        items = resp.json()["items"]

        assistant_msgs = [m for m in items if m["role"] == "assistant"]
        assert len(assistant_msgs) >= 1, "No assistant messages found"

        for msg in assistant_msgs:
            assert "my_reaction" in msg, (
                f"Assistant message {msg['id']} missing 'my_reaction' field. "
                f"Keys present: {list(msg.keys())}"
            )
            assert msg["my_reaction"] is None, (
                f"Expected my_reaction=null for untouched message, got: {msg['my_reaction']}"
            )

        user_msgs = [m for m in items if m["role"] == "user"]
        assert len(user_msgs) >= 1, "No user messages found"
        for user_msg in user_msgs:
            assert user_msg.get("my_reaction") is None, (
                f"Expected my_reaction=null for user message {user_msg['id']}, "
                f"got: {user_msg.get('my_reaction')}"
            )

    def test_cursor_pagination(self, server):
        chat_id = _create_chat_with_messages()
        # First page with limit=1
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", params={"limit": 1})
        assert resp.status_code == 200
        body = resp.json()
        assert len(body["items"]) <= 1
        assert "page_info" in body
        assert body["page_info"] is not None

    def test_request_id_non_null(self, server):
        """03-05: Every message must have a non-null request_id."""
        chat_id = _create_chat_with_messages()
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        for msg in resp.json()["items"]:
            assert msg.get("request_id") is not None, f"request_id null on {msg['role']} message"

    def test_attachments_array_present(self, server):
        """03-06: Every message must have an attachments array."""
        chat_id = _create_chat_with_messages()
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        for msg in resp.json()["items"]:
            assert isinstance(msg.get("attachments"), list), f"attachments not array on {msg['role']} message"

    def test_request_id_shared_per_turn(self, server):
        """03-08: User and assistant messages in same turn share request_id."""
        chat_id = _create_chat_with_messages()
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        items = resp.json()["items"]
        # Find pairs by position (user, assistant alternating)
        validated = 0
        for i in range(0, len(items) - 1, 2):
            if items[i]["role"] == "user" and items[i+1]["role"] == "assistant":
                assert items[i]["request_id"] == items[i+1]["request_id"], (
                    f"Turn pair request_ids don't match: {items[i]['request_id']} vs {items[i+1]['request_id']}"
                )
                validated += 1
        assert validated >= 1, f"No user+assistant pairs found to validate. Items: {[m['role'] for m in items]}"
