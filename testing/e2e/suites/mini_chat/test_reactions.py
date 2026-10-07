"""Tests for message reaction endpoints (like/dislike, upsert, remove)."""

import uuid

import httpx

from .conftest import API_PREFIX, RESOURCE_MESSAGE, assert_problem, expect_done, list_messages, query_db, stream_message


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


def reaction_url(chat_id: str, message_id: str) -> str:
    return f"{API_PREFIX}/chats/{chat_id}/messages/{message_id}/reaction"


def my_reaction(chat_id: str, message_id: str):
    matching = [m for m in list_messages(chat_id) if m["id"] == message_id]
    assert len(matching) == 1, f"message {message_id} not listed"
    return matching[0]["my_reaction"]


class TestReactions:
    """PUT/DELETE /chats/{cid}/messages/{msg_id}/reaction"""

    def test_set_reaction_like(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()

        resp = httpx.put(reaction_url(chat_id, assistant_msg_id), json={"reaction": "like"})
        assert resp.status_code == 200
        body = resp.json()
        assert body["message_id"] == assistant_msg_id
        assert body["reaction"] == "like"
        assert "created_at" in body
        assert my_reaction(chat_id, assistant_msg_id) == "like"

    def test_switch_reaction_like_to_dislike(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = reaction_url(chat_id, assistant_msg_id)

        assert httpx.put(url, json={"reaction": "like"}).status_code == 200
        resp = httpx.put(url, json={"reaction": "dislike"})
        assert resp.status_code == 200
        assert resp.json()["reaction"] == "dislike"
        assert my_reaction(chat_id, assistant_msg_id) == "dislike"

    def test_put_same_reaction_twice_is_idempotent(self, server):
        """PUT like twice: both 200, one stored reaction."""
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = reaction_url(chat_id, assistant_msg_id)

        for _ in range(2):
            resp = httpx.put(url, json={"reaction": "like"})
            assert resp.status_code == 200
            assert resp.json()["reaction"] == "like"

        rows = query_db(
            "SELECT reaction FROM message_reactions WHERE message_id = ?", (assistant_msg_id,),
        )
        assert rows == [{"reaction": "like"}]
        assert my_reaction(chat_id, assistant_msg_id) == "like"

    def test_reaction_on_user_message_400(self, server):
        """PUT and DELETE on a user message are both rejected the same way."""
        chat_id, user_msg_id, _ = _create_chat_with_assistant_message()
        url = reaction_url(chat_id, user_msg_id)

        for resp in (httpx.put(url, json={"reaction": "like"}), httpx.delete(url)):
            body = assert_problem(
                resp, 400, "failed_precondition",
                violation_subject="reaction_target", violation_type="STATE",
                resource_type=RESOURCE_MESSAGE,
            )
            assert body["context"].get("resource_name") == user_msg_id, body

    def test_invalid_reaction_value_400(self, server):
        """A reaction other than `like` / `dislike` is a validation error:
        400 invalid_argument; nothing is stored."""
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()

        resp = httpx.put(reaction_url(chat_id, assistant_msg_id), json={"reaction": "love"})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_REACTION")
        assert query_db(
            "SELECT reaction FROM message_reactions WHERE message_id = ?", (assistant_msg_id,),
        ) == []
        assert my_reaction(chat_id, assistant_msg_id) is None

    def test_reaction_body_errors(self, server):
        """A body without `reaction` (schema-invalid) is 422 invalid_argument;
        malformed JSON is 400 invalid_argument. Nothing is stored."""
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = reaction_url(chat_id, assistant_msg_id)

        assert_problem(httpx.put(url, json={}), 422, "invalid_argument")
        resp = httpx.put(url, content=b"{not json", headers={"Content-Type": "application/json"})
        assert_problem(resp, 400, "invalid_argument", field_reason="json_syntax_error")
        assert my_reaction(chat_id, assistant_msg_id) is None

    def test_reaction_on_nonexistent_message_404(self, chat):
        msg_id = str(uuid.uuid4())
        url = reaction_url(chat["id"], msg_id)
        for resp in (httpx.put(url, json={"reaction": "like"}), httpx.delete(url)):
            body = assert_problem(resp, 404, "not_found", resource_type=RESOURCE_MESSAGE)
            assert body["context"]["resource_name"] == msg_id, body

    def test_reaction_on_answer_of_deleted_turn_404(self, server):
        """The answer of a turn removed by DELETE /turns/{request_id} is
        gone: PUT and DELETE of its reaction are 404 (message resource)."""
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        (answer,) = [m for m in list_messages(chat_id) if m["id"] == assistant_msg_id]
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/turns/{answer['request_id']}")
        assert resp.status_code == 204, resp.text

        url = reaction_url(chat_id, assistant_msg_id)
        for resp in (httpx.put(url, json={"reaction": "like"}), httpx.delete(url)):
            body = assert_problem(resp, 404, "not_found", resource_type=RESOURCE_MESSAGE)
            assert body["context"]["resource_name"] == assistant_msg_id, body
        assert query_db(
            "SELECT COUNT(*) AS n FROM message_reactions WHERE message_id = ?", (assistant_msg_id,),
        ) == [{"n": 0}]

    def test_remove_reaction_204(self, server):
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = reaction_url(chat_id, assistant_msg_id)

        assert httpx.put(url, json={"reaction": "like"}).status_code == 200
        assert httpx.delete(url).status_code == 204
        assert my_reaction(chat_id, assistant_msg_id) is None

    def test_remove_reaction_idempotent(self, server):
        """DELETE is idempotent: after a reaction was set and removed, a
        second DELETE is 204 too and nothing is stored; so is a DELETE when
        no reaction was ever set."""
        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        url = reaction_url(chat_id, assistant_msg_id)
        assert httpx.put(url, json={"reaction": "like"}).status_code == 200
        assert [httpx.delete(url).status_code for _ in range(2)] == [204, 204]
        assert my_reaction(chat_id, assistant_msg_id) is None

        chat_id, _, assistant_msg_id = _create_chat_with_assistant_message()
        assert httpx.delete(reaction_url(chat_id, assistant_msg_id)).status_code == 204
        assert my_reaction(chat_id, assistant_msg_id) is None
