"""Tests for message listing with OData query options and field presence."""

import httpx
import pytest
from uuid import uuid4

from .conftest import API_PREFIX, RESOURCE_ODATA, assert_problem, expect_done, stream_message


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

    def test_odata_orderby(self, server):
        """`$orderby=created_at desc` returns the four messages of two turns
        in exactly the reverse of the default (chronological) order."""
        chat_id = _create_chat_with_messages(2)
        url = f"{API_PREFIX}/chats/{chat_id}/messages"

        default = httpx.get(url)
        assert default.status_code == 200, default.text
        ascending = [m["id"] for m in default.json()["items"]]
        assert len(ascending) == 4, ascending
        assert [m["role"] for m in default.json()["items"]] == ["user", "assistant"] * 2

        resp = httpx.get(url, params={"$orderby": "created_at desc"})
        assert resp.status_code == 200, f"GET messages failed: {resp.status_code} {resp.text}"
        assert [m["id"] for m in resp.json()["items"]] == ascending[::-1], resp.json()

    def test_odata_filter_role(self, server):
        """`$filter=role eq 'assistant'` returns exactly the answers: one
        per turn, in order."""
        chat_id = _create_chat_with_messages(2)
        url = f"{API_PREFIX}/chats/{chat_id}/messages"
        answers = [m["id"] for m in httpx.get(url).json()["items"] if m["role"] == "assistant"]
        assert len(answers) == 2, answers

        resp = httpx.get(url, params={"$filter": "role eq 'assistant'"})
        assert resp.status_code == 200, f"GET messages failed: {resp.status_code} {resp.text}"
        items = resp.json()["items"]
        assert [(m["id"], m["role"]) for m in items] == [(a, "assistant") for a in answers], items

    def test_my_reaction_field(self, server):
        """Every message has the required `my_reaction` field: null when no reaction is set."""
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
            # A required field: present (null) on user messages too.
            assert "my_reaction" in user_msg, user_msg
            assert user_msg["my_reaction"] is None, user_msg

    def test_cursor_pagination(self, server):
        """Following next_cursor with limit=1 visits every message exactly once, in order."""
        chat_id = _create_chat_with_messages(3)
        full = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages").json()["items"]
        assert len(full) == 6

        ids: list[str] = []
        params = {"limit": 1}
        for _ in range(20):
            resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", params=params)
            assert resp.status_code == 200, resp.text
            body = resp.json()
            assert len(body["items"]) == 1
            ids.extend(m["id"] for m in body["items"])
            cursor = body["page_info"].get("next_cursor")
            if not cursor:
                break
            params = {"limit": 1, "cursor": cursor}
        assert ids == [m["id"] for m in full]

    def test_prev_cursor_pages_back(self, server):
        """03-23: `page_info.prev_cursor` is absent on the first page and set
        on the next one; following it returns the first page again, and
        `$top` / `$skiptoken` are the same as `limit` / `cursor`."""
        chat_id = _create_chat_with_messages(3)
        url = f"{API_PREFIX}/chats/{chat_id}/messages"
        ids = [m["id"] for m in httpx.get(url).json()["items"]]
        assert len(ids) == 6, ids

        first = httpx.get(url, params={"limit": 2}).json()
        assert [m["id"] for m in first["items"]] == ids[:2]
        assert first["page_info"].get("prev_cursor") is None, first["page_info"]
        second = httpx.get(url, params={"limit": 2, "cursor": first["page_info"]["next_cursor"]})
        assert second.status_code == 200, second.text
        second = second.json()
        assert [m["id"] for m in second["items"]] == ids[2:4]
        prev = second["page_info"].get("prev_cursor")
        assert prev, second["page_info"]

        back = httpx.get(url, params={"$top": 2, "$skiptoken": prev})
        assert back.status_code == 200, back.text
        back = back.json()
        assert [m["id"] for m in back["items"]] == ids[:2], back
        assert back["page_info"].get("prev_cursor") is None, back["page_info"]
        assert back["page_info"].get("next_cursor"), back["page_info"]

    def test_unknown_filter_field_400(self, chat):
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat['id']}/messages", params={"$filter": "nosuchfield eq 'x'"},
        )
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_FILTER", resource_type=RESOURCE_ODATA,
        )

    def test_unknown_orderby_field_400(self, chat):
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat['id']}/messages", params={"$orderby": "nosuchfield desc"},
        )
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_ORDERBY_FIELD", resource_type=RESOURCE_ODATA,
        )

    def test_malformed_cursor_400(self, chat):
        resp = httpx.get(f"{API_PREFIX}/chats/{chat['id']}/messages", params={"cursor": "not-a-cursor"})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_CURSOR")

    @pytest.mark.parametrize(
        "first_filter,next_filter",
        [("role ne 'system'", "role eq 'user'"), ("role ne 'system'", None)],
        ids=["other_filter", "filter_dropped"],
    )
    def test_cursor_with_other_filter_400(self, server, first_filter, next_filter):
        """A cursor is bound to the `$filter` it was issued for: another or a
        missing filter on the continuation is rejected."""
        chat_id = _create_chat_with_messages(1)
        url = f"{API_PREFIX}/chats/{chat_id}/messages"
        params = {"limit": 1}
        if first_filter is not None:
            params["$filter"] = first_filter
        first = httpx.get(url, params=params)
        assert first.status_code == 200, first.text
        cursor = first.json()["page_info"].get("next_cursor")
        assert cursor, first.json()

        params = {"limit": 1, "cursor": cursor}
        if next_filter is not None:
            params["$filter"] = next_filter
        resp = httpx.get(url, params=params)
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="FILTER_MISMATCH", resource_type=RESOURCE_ODATA,
        )

    def test_zero_limit_400(self, chat):
        resp = httpx.get(f"{API_PREFIX}/chats/{chat['id']}/messages", params={"limit": 0})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_LIMIT")

    def test_limit_above_100_is_clamped(self, server):
        """`limit=500` is not rejected: the page size is clamped to 100."""
        chat_id = _create_chat_with_messages(1)
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", params={"limit": 500})
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["page_info"]["limit"] == 100, body["page_info"]
        assert len(body["items"]) == 2, body["items"]

    def test_select_accepted_and_ignored(self, server):
        """03-02: a valid `$select` is accepted and ignored: the page is the
        one returned without it, with every message field."""
        chat_id = _create_chat_with_messages(1)
        url = f"{API_PREFIX}/chats/{chat_id}/messages"
        plain = httpx.get(url)
        assert plain.status_code == 200, plain.text
        selected = httpx.get(url, params={"$select": "id"})
        assert selected.status_code == 200, selected.text
        assert selected.json() == plain.json()
        assert all("content" in m and "role" in m for m in selected.json()["items"])

    def test_invalid_select_400(self, chat):
        """03-02: the `$select` syntax is validated: a duplicate field is 400
        invalid_argument INVALID_SELECT."""
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat['id']}/messages", params={"$select": "id,id"},
        )
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_SELECT", resource_type=RESOURCE_ODATA,
        )

    def test_unsupported_query_option_400(self, chat):
        """A `$` option the OData extractor does not bind (`$skip`) is 400
        invalid_argument UNSUPPORTED_QUERY_PARAM, not silently ignored."""
        resp = httpx.get(f"{API_PREFIX}/chats/{chat['id']}/messages", params={"$skip": "1"})
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="UNSUPPORTED_QUERY_PARAM", resource_type=RESOURCE_ODATA,
        )

    def test_filter_too_long_400(self, chat):
        """A `$filter` longer than MAX_FILTER_LEN (8 KiB,
        libs/toolkit/src/api/odata.rs) is 400 invalid_argument FILTER_TOO_LONG."""
        long_filter = "role eq '" + "a" * (8 * 1024) + "'"
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat['id']}/messages", params={"$filter": long_filter},
        )
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="FILTER_TOO_LONG", resource_type=RESOURCE_ODATA,
        )

    def test_filter_too_complex_400(self, chat):
        """A `$filter` of more than 2000 nodes (MAX_NODES,
        libs/toolkit/src/api/odata.rs) within the length limit: 501
        comparisons joined with `or` are 4 * 501 - 1 = 2003 nodes in 7511
        bytes → 400 invalid_argument FILTER_TOO_COMPLEX."""
        complex_filter = " or ".join(["role eq 'a'"] * 501)
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat['id']}/messages", params={"$filter": complex_filter},
        )
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="FILTER_TOO_COMPLEX", resource_type=RESOURCE_ODATA,
        )

    def test_limit_not_a_number_400(self, chat):
        """`limit=abc` does not deserialize: 400 invalid_argument
        INVALID_QUERY_PARAMS on `query`."""
        resp = httpx.get(f"{API_PREFIX}/chats/{chat['id']}/messages", params={"limit": "abc"})
        body = assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_QUERY_PARAMS", resource_type=RESOURCE_ODATA,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["query"], body

    def test_orderby_with_cursor_400(self, server):
        chat_id = _create_chat_with_messages(1)
        url = f"{API_PREFIX}/chats/{chat_id}/messages"
        first = httpx.get(url, params={"limit": 1})
        assert first.status_code == 200, first.text
        cursor = first.json()["page_info"].get("next_cursor")
        assert cursor, first.json()
        resp = httpx.get(url, params={"cursor": cursor, "$orderby": "created_at desc"})
        assert_problem(resp, 400, "invalid_argument", field_reason="ORDER_WITH_CURSOR")

    def test_messages_of_nonexistent_chat_404(self, server):
        resp = httpx.get(f"{API_PREFIX}/chats/{uuid4()}/messages")
        assert_problem(resp, 404, "not_found")

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
        chat_id = _create_chat_with_messages(2)
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        items = resp.json()["items"]
        assert [m["role"] for m in items] == ["user", "assistant"] * 2
        pairs = [(items[i]["request_id"], items[i + 1]["request_id"]) for i in (0, 2)]
        for user_rid, assistant_rid in pairs:
            assert user_rid == assistant_rid, pairs
        assert pairs[0][0] != pairs[1][0], "each turn has its own request_id"
