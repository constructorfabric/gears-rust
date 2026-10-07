"""Tests for chat CRUD operations."""

import uuid
from datetime import datetime

import pytest
import httpx

from .conftest import (
    API_PREFIX,
    DEFAULT_MODEL,
    DISABLED_MODEL,
    RESOURCE_HTTP_REQUEST,
    RESOURCE_ODATA,
    USER_A_ID,
    STANDARD_MODEL,
    TOKEN_USER_B,
    assert_problem,
    auth_headers,
    expect_done,
    query_db,
    stream_message,
)


def _ts(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def list_all_chats(page_size: int, token: str) -> list[str]:
    """Follow `page_info.next_cursor` to the end; return chat ids in order."""
    ids: list[str] = []
    params = {"limit": page_size}
    for _ in range(1000):
        resp = httpx.get(f"{API_PREFIX}/chats", params=params, headers=auth_headers(token))
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert len(body["items"]) <= page_size
        ids.extend(c["id"] for c in body["items"])
        cursor = body["page_info"].get("next_cursor")
        if not cursor:
            return ids
        params = {"limit": page_size, "cursor": cursor}
    raise AssertionError("pagination did not terminate")


class TestCreateChat:
    """POST /v1/chats"""

    def test_create_chat_default_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        assert resp.status_code == 201
        body = resp.json()
        assert "id" in body
        assert body["model"] == DEFAULT_MODEL
        assert body["message_count"] == 0
        assert "created_at" in body
        assert "updated_at" in body

    def test_create_chat_location_header(self, server):
        """The 201 carries `Location: /mini-chat/v1/chats/{id}`. The path is
        the one the gear router sees: the api-gateway `prefix_path` (`/cf`
        here) is not included (toolkit `created_json`)."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        assert resp.status_code == 201
        chat_id = resp.json()["id"]
        assert resp.headers.get("location") == f"/mini-chat/v1/chats/{chat_id}", resp.headers

    def test_create_chat_with_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": STANDARD_MODEL})
        assert resp.status_code == 201
        assert resp.json()["model"] == STANDARD_MODEL

    def test_create_chat_with_title(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "My Test Chat"})
        assert resp.status_code == 201
        assert resp.json()["title"] == "My Test Chat"

    def test_create_chat_invalid_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": "nonexistent-model"})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_MODEL")

    def test_create_chat_disabled_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": DISABLED_MODEL})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_MODEL")

    def test_create_chat_title_trimmed(self, server):
        """02-36: leading and trailing whitespace of the title is trimmed
        (domain/service/chat_service.rs)."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "  \tPadded title \n "})
        assert resp.status_code == 201, resp.text
        assert resp.json()["title"] == "Padded title"
        fetched = httpx.get(f"{API_PREFIX}/chats/{resp.json()['id']}").json()
        assert fetched["title"] == "Padded title", fetched

    def test_create_chat_title_length_boundary(self, server):
        """Title of 255 characters is accepted, 256 is rejected."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "T" * 255})
        assert resp.status_code == 201, resp.text
        assert resp.json()["title"] == "T" * 255
        assert_problem(
            httpx.post(f"{API_PREFIX}/chats", json={"title": "T" * 256}), 400, "invalid_argument",
            field_reason="INVALID_TITLE",
        )

    def test_create_chat_whitespace_title_rejected(self, server):
        """A whitespace-only title is 400 invalid_argument."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "   "})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_TITLE")

    def test_create_chat_schema_invalid_is_422(self, server):
        """A body that does not match the schema (`model` is not a string) is 422."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": 123})
        body = assert_problem(resp, 422, "invalid_argument", field_reason="invalid_json_body")
        assert [v["field"] for v in body["context"]["field_violations"]] == ["body"], body

    def test_create_chat_malformed_json_is_400(self, server):
        resp = httpx.post(
            f"{API_PREFIX}/chats", content=b"{not json",
            headers={"Content-Type": "application/json"},
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="json_syntax_error")


class TestGetChat:
    """GET /v1/chats/{id}"""

    @pytest.mark.multi_provider
    def test_get_chat(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["id"] == chat_id
        assert body["model"] == provider_chat["model"]
        assert "created_at" in body
        assert "updated_at" in body

    def test_get_chat_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{fake_id}")
        assert_problem(resp, 404, "not_found")


class TestListChats:
    """GET /v1/chats"""

    @pytest.mark.multi_provider
    def test_list_chats(self, provider_chat):
        resp = httpx.get(f"{API_PREFIX}/chats")
        assert resp.status_code == 200
        body = resp.json()
        assert "items" in body
        assert "page_info" in body
        assert len(body["items"]) >= 1
        assert provider_chat["id"] in [c["id"] for c in body["items"]]

    def test_list_chats_pagination(self, server):
        """Following next_cursor visits every chat exactly once, in list order.

        Runs as user B, whose chat list stays short (user A owns hundreds of chats).
        """
        headers = auth_headers(TOKEN_USER_B)
        for _ in range(5):
            r = httpx.post(f"{API_PREFIX}/chats", json={}, headers=headers)
            assert r.status_code == 201
        full = httpx.get(f"{API_PREFIX}/chats", params={"limit": 100}, headers=headers).json()
        assert full["page_info"].get("next_cursor") is None, "B's chats must fit in one page"
        full = full["items"]
        assert len(full) >= 5

        paged = list_all_chats(page_size=2, token=TOKEN_USER_B)
        assert len(paged) == len(set(paged)), "duplicate chats across pages"
        assert paged == [c["id"] for c in full]

    def test_list_chats_unknown_filter_field_400(self, server):
        resp = httpx.get(f"{API_PREFIX}/chats", params={"$filter": "nosuchfield eq 'x'"})
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_FILTER", resource_type=RESOURCE_ODATA,
        )

    def test_list_chats_unknown_orderby_field_400(self, server):
        resp = httpx.get(f"{API_PREFIX}/chats", params={"$orderby": "nosuchfield desc"})
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_ORDERBY_FIELD", resource_type=RESOURCE_ODATA,
        )

    def test_list_chats_malformed_cursor_400(self, server):
        resp = httpx.get(f"{API_PREFIX}/chats", params={"cursor": "not-a-cursor"})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_CURSOR")

    @pytest.mark.parametrize(
        "first_filtered,next_filter",
        [(True, "contains(title, 'other')"), (True, None)],
        ids=["other_filter", "filter_dropped"],
    )
    def test_list_chats_cursor_with_other_filter_400(self, server, first_filtered, next_filter):
        """A cursor is bound to the `$filter` it was issued for: continuing it
        under another filter or without the filter is rejected. The cursor
        carries only the filter hash, so the server cannot restore a dropped
        filter."""
        tag = f"fm-{uuid.uuid4().hex}"
        for _ in range(2):
            r = httpx.post(f"{API_PREFIX}/chats", json={"title": f"{tag} chat"})
            assert r.status_code == 201, r.text
        params = {"limit": 1}
        if first_filtered:
            params["$filter"] = f"contains(title, '{tag}')"
        first = httpx.get(f"{API_PREFIX}/chats", params=params)
        assert first.status_code == 200, first.text
        cursor = first.json()["page_info"].get("next_cursor")
        assert cursor, first.json()

        params = {"limit": 1, "cursor": cursor}
        if next_filter is not None:
            params["$filter"] = next_filter
        resp = httpx.get(f"{API_PREFIX}/chats", params=params)
        assert_problem(
            resp, 400, "invalid_argument",
            field_reason="FILTER_MISMATCH", resource_type=RESOURCE_ODATA,
        )

    def test_list_chats_zero_limit_400(self, server):
        resp = httpx.get(f"{API_PREFIX}/chats", params={"limit": 0})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_LIMIT")

    def test_list_chats_limit_above_100_is_clamped(self, chat):
        """`limit=500` is not rejected: the page size is clamped to 100
        (LimitCfg max of the chat repository, gear.rs)."""
        resp = httpx.get(f"{API_PREFIX}/chats", params={"limit": 500})
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["page_info"]["limit"] == 100, body["page_info"]
        (row,) = query_db(
            "SELECT COUNT(*) AS n FROM chats WHERE user_id = ? AND deleted_at IS NULL",
            (USER_A_ID,),
        )
        assert len(body["items"]) == min(row["n"], 100), (row, len(body["items"]))

    @pytest.mark.parametrize(("params", "field", "reason"), [
        pytest.param({"$select": "id,id"}, "$select", "INVALID_SELECT", id="duplicate_select"),
        pytest.param({"$skip": "1"}, "$skip", "UNSUPPORTED_QUERY_PARAM", id="skip"),
        pytest.param(
            {"$filter": "title eq '" + "a" * (8 * 1024) + "'"}, "$filter", "FILTER_TOO_LONG",
            id="filter_over_8_kib",
        ),
        # 501 comparisons joined with `or`: 4 * 501 - 1 = 2003 nodes, over
        # the 2000-node limit (libs/toolkit/src/api/odata.rs), in 8012 bytes.
        pytest.param(
            {"$filter": " or ".join(["title eq 'a'"] * 501)}, "$filter", "FILTER_TOO_COMPLEX",
            id="filter_over_2000_nodes",
        ),
        pytest.param({"limit": "abc"}, "query", "INVALID_QUERY_PARAMS", id="limit_not_a_number"),
    ])
    def test_list_chats_invalid_query_400(self, server, params, field, reason):
        """The OData query options are validated by the platform extractor:
        400 invalid_argument with one violation on the option."""
        resp = httpx.get(f"{API_PREFIX}/chats", params=params)
        body = assert_problem(
            resp, 400, "invalid_argument", field_reason=reason, resource_type=RESOURCE_ODATA,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == [field], body

    def test_list_chats_orderby_with_cursor_400(self, server):
        """A cursor combined with `$orderby` is rejected. The test creates
        the two chats that make a `limit=1` page have a next cursor."""
        for _ in range(2):
            r = httpx.post(f"{API_PREFIX}/chats", json={})
            assert r.status_code == 201, r.text
        first = httpx.get(f"{API_PREFIX}/chats", params={"limit": 1})
        assert first.status_code == 200, first.text
        cursor = first.json()["page_info"]["next_cursor"]
        assert cursor, first.json()
        resp = httpx.get(
            f"{API_PREFIX}/chats", params={"cursor": cursor, "$orderby": "updated_at desc"},
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="ORDER_WITH_CURSOR")

    def test_send_moves_older_chat_to_top(self, server):
        """The list is ordered by activity: sending into an older chat puts it first."""
        older = httpx.post(f"{API_PREFIX}/chats", json={}).json()["id"]
        newer = httpx.post(f"{API_PREFIX}/chats", json={}).json()["id"]
        ids = [c["id"] for c in httpx.get(f"{API_PREFIX}/chats").json()["items"]]
        assert ids.index(newer) < ids.index(older)

        status, events, _ = stream_message(older, "Say OK.")
        assert status == 200
        expect_done(events)

        ids = [c["id"] for c in httpx.get(f"{API_PREFIX}/chats").json()["items"]]
        assert ids[0] == older


def _tagged_chats(titles: list[str]) -> tuple[str, list[dict]]:
    """Create chats titled `<tag> <title>` in this order; return (tag, chats)."""
    tag = f"q-{uuid.uuid4().hex}"
    chats = []
    for title in titles:
        r = httpx.post(f"{API_PREFIX}/chats", json={"title": f"{tag} {title}"})
        assert r.status_code == 201, r.text
        chats.append(r.json())
    return tag, chats


def _list(params: dict) -> dict:
    resp = httpx.get(f"{API_PREFIX}/chats", params=params)
    assert resp.status_code == 200, resp.text
    return resp.json()


class TestListChatsQuery:
    """02-32..02-35: valid OData options on GET /chats (fields `updated_at`,
    `id`, `title`, infra/db/odata_mapper.rs `ChatCursorField`). Each test
    narrows the list to its own chats with a unique title tag."""

    def test_filter_on_each_field(self, server):
        """`title eq`, `contains(title, ...)`, `id eq` and `updated_at ge`
        return exactly the matching chats, newest first."""
        tag, chats = _tagged_chats(["one", "two", "three"])
        ids = [c["id"] for c in chats]
        in_tag = f"contains(title, '{tag}')"

        items = _list({"$filter": f"title eq '{tag} two'"})["items"]
        assert [c["id"] for c in items] == [ids[1]], items
        items = _list({"$filter": in_tag})["items"]
        assert [c["id"] for c in items] == ids[::-1], items
        items = _list({"$filter": f"id eq {ids[2]}"})["items"]
        assert [c["id"] for c in items] == [ids[2]], items
        since = chats[1]["updated_at"]
        items = _list({"$filter": f"{in_tag} and updated_at ge {since}"})["items"]
        assert [c["id"] for c in items] == [ids[2], ids[1]], items

    @pytest.mark.parametrize(("orderby", "key", "reverse"), [
        pytest.param("title asc", "title", False, id="title_asc"),
        pytest.param("title desc", "title", True, id="title_desc"),
        pytest.param("updated_at asc", "updated_at", False, id="updated_at_asc"),
        pytest.param("id asc", "id", False, id="id_asc"),
        pytest.param("id desc", "id", True, id="id_desc"),
    ])
    def test_orderby_each_field(self, server, orderby, key, reverse):
        """`$orderby` on each field sorts the page by that field."""
        tag, chats = _tagged_chats(["bravo", "alpha", "charlie"])
        items = _list({"$filter": f"contains(title, '{tag}')", "$orderby": orderby})["items"]
        if key == "updated_at":
            expected = [c["id"] for c in chats]
        else:
            expected = [c["id"] for c in sorted(chats, key=lambda c: c[key], reverse=reverse)]
        assert [c["id"] for c in items] == expected, items

    def test_prev_cursor_pages_back(self, server):
        """`page_info.prev_cursor`: absent on the first page, present on the
        next one, and following it returns the first page again (with a
        `next_cursor` and no `prev_cursor`)."""
        tag, chats = _tagged_chats([str(i) for i in range(5)])
        newest_first = [c["id"] for c in chats][::-1]
        flt = {"$filter": f"contains(title, '{tag}')", "limit": 2}

        first = _list(flt)
        assert [c["id"] for c in first["items"]] == newest_first[:2]
        assert first["page_info"].get("prev_cursor") is None, first["page_info"]
        second = _list({**flt, "cursor": first["page_info"]["next_cursor"]})
        assert [c["id"] for c in second["items"]] == newest_first[2:4]
        prev = second["page_info"].get("prev_cursor")
        assert prev, second["page_info"]

        back = _list({**flt, "cursor": prev})
        assert [c["id"] for c in back["items"]] == newest_first[:2], back
        assert back["page_info"].get("prev_cursor") is None, back["page_info"]
        assert back["page_info"].get("next_cursor"), back["page_info"]

    def test_top_and_skiptoken_aliases(self, server):
        """`$top` is an alias of `limit` and `$skiptoken` of `cursor`
        (libs/toolkit/src/api/odata.rs): the same pages as with the plain
        names. Both spellings of one option in a request are 400."""
        tag, chats = _tagged_chats([str(i) for i in range(3)])
        flt = {"$filter": f"contains(title, '{tag}')"}
        plain = _list({**flt, "limit": 2})
        aliased = _list({**flt, "$top": 2})
        assert aliased == plain
        assert aliased["page_info"]["limit"] == 2, aliased["page_info"]
        cursor = plain["page_info"]["next_cursor"]
        assert _list({**flt, "$top": 2, "$skiptoken": cursor}) == _list(
            {**flt, "limit": 2, "cursor": cursor},
        )
        assert [c["id"] for c in _list({**flt, "$top": 2, "$skiptoken": cursor})["items"]] == [
            chats[0]["id"],
        ]

        resp = httpx.get(f"{API_PREFIX}/chats", params={**flt, "limit": 2, "$top": 2})
        assert_problem(resp, 400, "invalid_argument", resource_type=RESOURCE_ODATA)


class TestUpdateChat:
    """PATCH /v1/chats/{id}"""

    @pytest.mark.multi_provider
    def test_update_title(self, provider_chat):
        """PATCH title is persisted and bumps updated_at."""
        chat_id = provider_chat["id"]
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "Updated Title"},
        )
        assert resp.status_code == 200
        assert resp.json()["title"] == "Updated Title"

        fetched = httpx.get(f"{API_PREFIX}/chats/{chat_id}").json()
        assert fetched["title"] == "Updated Title"
        assert _ts(fetched["updated_at"]) > _ts(provider_chat["updated_at"])

    def test_update_title_trimmed(self, chat):
        """02-36: PATCH trims the title too."""
        resp = httpx.patch(f"{API_PREFIX}/chats/{chat['id']}", json={"title": "  Renamed  "})
        assert resp.status_code == 200, resp.text
        assert resp.json()["title"] == "Renamed"
        assert httpx.get(f"{API_PREFIX}/chats/{chat['id']}").json()["title"] == "Renamed"

    def test_update_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{fake_id}",
            json={"title": "Nope"},
        )
        assert_problem(resp, 404, "not_found")

    @pytest.mark.multi_provider
    def test_update_whitespace_title_rejected(self, provider_chat):
        """02-12: Whitespace-only title should be rejected."""
        chat_id = provider_chat["id"]
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "   "},
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_TITLE")
        assert httpx.get(f"{API_PREFIX}/chats/{chat_id}").json().get("title") == provider_chat.get("title")

    @pytest.mark.multi_provider
    def test_update_without_title_is_422(self, provider_chat):
        """A body that does not match the schema is 422 invalid_argument."""
        resp = httpx.patch(f"{API_PREFIX}/chats/{provider_chat['id']}", json={})
        assert_problem(resp, 422, "invalid_argument")

    @pytest.mark.multi_provider
    def test_update_malformed_json_is_400(self, provider_chat):
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{provider_chat['id']}",
            content=b"{not json",
            headers={"Content-Type": "application/json"},
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="json_syntax_error")

    @pytest.mark.multi_provider
    def test_update_title_length_boundary(self, provider_chat):
        """02-13: a 255-character title is accepted, 256 is rejected."""
        chat_id = provider_chat["id"]
        resp = httpx.patch(f"{API_PREFIX}/chats/{chat_id}", json={"title": "A" * 255})
        assert resp.status_code == 200, resp.text
        assert resp.json()["title"] == "A" * 255

        resp = httpx.patch(f"{API_PREFIX}/chats/{chat_id}", json={"title": "A" * 256})
        assert_problem(resp, 400, "invalid_argument", field_reason="INVALID_TITLE")
        assert httpx.get(f"{API_PREFIX}/chats/{chat_id}").json()["title"] == "A" * 255


class TestDeleteChat:
    """DELETE /v1/chats/{id}"""

    @pytest.mark.multi_provider
    def test_delete_chat(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 204

        # Verify gone, and a second DELETE is 404
        assert_problem(httpx.get(f"{API_PREFIX}/chats/{chat_id}"), 404, "not_found")
        assert_problem(httpx.delete(f"{API_PREFIX}/chats/{chat_id}"), 404, "not_found")
        ids = [c["id"] for c in httpx.get(f"{API_PREFIX}/chats").json()["items"]]
        assert chat_id not in ids

    def test_delete_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.delete(f"{API_PREFIX}/chats/{fake_id}")
        assert_problem(resp, 404, "not_found")


class TestJsonContentType:
    """17-17: a JSON body sent without a JSON `Content-Type` is 415
    invalid_argument with one violation on `body`, reason
    `missing_json_content_type` (libs/toolkit/src/api/rest/extract/json.rs).
    The body is valid JSON; the path ids need not exist, the body is
    rejected before the handler runs."""

    @pytest.mark.parametrize(("method", "path", "body"), [
        pytest.param("POST", "/chats", b"{}", id="create_chat"),
        pytest.param("PATCH", "/chats/{chat_id}", b'{"title": "T"}', id="update_chat"),
        pytest.param("POST", "/chats/{chat_id}/messages:stream", b'{"content": "Hi"}', id="send"),
        pytest.param("PATCH", "/chats/{chat_id}/turns/{id}", b'{"content": "Hi"}', id="edit"),
        pytest.param(
            "PUT", "/chats/{chat_id}/messages/{id}/reaction", b'{"reaction": "like"}',
            id="reaction",
        ),
    ])
    @pytest.mark.parametrize("content_type", [None, "text/plain"], ids=["none", "text_plain"])
    def test_json_body_without_json_content_type_415(
        self, chat, method, path, body, content_type,
    ):
        headers = {"Content-Type": content_type} if content_type else {}
        url = API_PREFIX + path.format(chat_id=chat["id"], id=uuid.uuid4())
        resp = httpx.request(method, url, content=body, headers=headers, timeout=10)
        problem = assert_problem(resp, 415, "invalid_argument", field_reason="missing_json_content_type")
        assert [v["field"] for v in problem["context"]["field_violations"]] == ["body"], problem
        assert query_db("SELECT id FROM chat_turns WHERE chat_id = ?", (chat["id"],)) == []


class TestPathParameters:
    """17-12: a path parameter that is not a UUID is a canonical 400."""

    @pytest.mark.parametrize("path", [
        "/chats/not-a-uuid",
        "/chats/{chat_id}/turns/not-a-uuid",
        "/chats/{chat_id}/attachments/not-a-uuid",
    ])
    def test_non_uuid_path_parameter_400(self, chat, path):
        resp = httpx.get(API_PREFIX + path.format(chat_id=chat["id"]))
        assert_problem(resp, 400, "invalid_argument", field_reason="invalid_path_params")

    @pytest.mark.parametrize("method", ["PUT", "DELETE"])
    def test_non_uuid_message_id_in_reaction_path_400(self, chat, method):
        """The message id of the reaction path is not a UUID (PUT with a
        valid body, and DELETE)."""
        body = {"reaction": "like"} if method == "PUT" else None
        resp = httpx.request(
            method, f"{API_PREFIX}/chats/{chat['id']}/messages/not-a-uuid/reaction", json=body,
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="invalid_path_params")

    @pytest.mark.parametrize(("method", "path", "kwargs"), [
        pytest.param("DELETE", "/chats/not-a-uuid", {}, id="delete_chat"),
        pytest.param("PATCH", "/chats/not-a-uuid", {"json": {"title": "T"}}, id="update_chat"),
        pytest.param("GET", "/chats/not-a-uuid/messages", {}, id="list_messages"),
        pytest.param(
            "POST", "/chats/not-a-uuid/messages:stream", {"json": {"content": "Hi"}}, id="send",
        ),
        pytest.param("POST", "/chats/{chat_id}/turns/not-a-uuid/retry", {}, id="retry"),
        pytest.param(
            "PATCH", "/chats/{chat_id}/turns/not-a-uuid", {"json": {"content": "Hi"}}, id="edit",
        ),
        pytest.param("DELETE", "/chats/{chat_id}/turns/not-a-uuid", {}, id="delete_turn"),
        pytest.param("POST", "/chats/not-a-uuid/turns/{turn_id}/retry", {}, id="retry_chat_id"),
        pytest.param(
            "DELETE", "/chats/{chat_id}/attachments/not-a-uuid", {}, id="delete_attachment",
        ),
        pytest.param(
            "POST", "/chats/not-a-uuid/attachments",
            {"files": {"file": ("a.txt", b"x", "text/plain")}}, id="upload",
        ),
    ])
    def test_non_uuid_path_parameter_on_mutations_400(self, chat, method, path, kwargs):
        """17-12 for the other operations: the path is rejected before the
        body is read, with one violation on `path` (the HTTP request
        resource of the platform extractor, libs/toolkit/src/api/rest/extract)."""
        url = API_PREFIX + path.format(chat_id=chat["id"], turn_id=uuid.uuid4())
        resp = httpx.request(method, url, timeout=10, **kwargs)
        body = assert_problem(
            resp, 400, "invalid_argument", field_reason="invalid_path_params",
            resource_type=RESOURCE_HTTP_REQUEST,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["path"], body
