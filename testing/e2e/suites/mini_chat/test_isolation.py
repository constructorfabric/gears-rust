"""Authentication and isolation between users and tenants.

User A owns the resources. User B is another user in the same tenant; user C
is in another tenant. Both must see A's resources as nonexistent (404) and
must not be able to change them. Requests without valid credentials get 401.
"""

from __future__ import annotations

import io
import json
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    NO_AUTH,
    TOKEN_TENANT_B,
    TOKEN_USER_A,
    TOKEN_USER_B,
    USER_A_ID,
    assert_no_reserves,
    assert_problem,
    auth_headers,
    expect_done,
    expect_stream_started,
    find_period,
    get_quota_status,
    list_messages,
    provider_usage,
    stream_message,
)

# Every public operation, with placeholders filled from the owned resources.
OPERATIONS = [
    ("POST", "/chats"),
    ("GET", "/chats"),
    ("GET", "/chats/{chat_id}"),
    ("PATCH", "/chats/{chat_id}"),
    ("DELETE", "/chats/{chat_id}"),
    ("GET", "/chats/{chat_id}/messages"),
    ("POST", "/chats/{chat_id}/messages:stream"),
    ("POST", "/chats/{chat_id}/attachments"),
    ("GET", "/chats/{chat_id}/attachments/{attachment_id}"),
    ("DELETE", "/chats/{chat_id}/attachments/{attachment_id}"),
    ("GET", "/chats/{chat_id}/turns/{request_id}"),
    ("POST", "/chats/{chat_id}/turns/{request_id}/retry"),
    ("PATCH", "/chats/{chat_id}/turns/{request_id}"),
    ("DELETE", "/chats/{chat_id}/turns/{request_id}"),
    ("PUT", "/chats/{chat_id}/messages/{message_id}/reaction"),
    ("DELETE", "/chats/{chat_id}/messages/{message_id}/reaction"),
    ("GET", "/models"),
    ("GET", "/models/{model_id}"),
    ("GET", "/quota/status"),
]

BODIES = {
    ("POST", "/chats"): {},
    ("PATCH", "/chats/{chat_id}"): {"title": "hijacked"},
    ("POST", "/chats/{chat_id}/messages:stream"): {"content": "hello"},
    ("PATCH", "/chats/{chat_id}/turns/{request_id}"): {"content": "edited"},
    ("PUT", "/chats/{chat_id}/messages/{message_id}/reaction"): {"reaction": "like"},
}


@pytest.fixture(scope="module")
def owned(server) -> dict:
    """A chat of user A with one completed turn and one ready document."""
    resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "owned by A"})
    assert resp.status_code == 201, resp.text
    chat_id = resp.json()["id"]

    request_id = str(uuid.uuid4())
    status, events, _ = stream_message(chat_id, "Say OK.", request_id=request_id)
    assert status == 200
    expect_done(events)
    message_id = expect_stream_started(events).data["message_id"]

    resp = httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        files={"file": ("notes.txt", io.BytesIO(b"owned document"), "text/plain")},
        timeout=60,
    )
    assert resp.status_code == 201, resp.text

    model_id = httpx.get(f"{API_PREFIX}/models").json()["items"][0]["model_id"]
    return {
        "chat_id": chat_id,
        "request_id": request_id,
        "message_id": message_id,
        "attachment_id": resp.json()["id"],
        "model_id": model_id,
    }


def _call(method: str, path: str, ids: dict, headers: dict) -> httpx.Response:
    url = API_PREFIX + path.format(**ids)
    if path.endswith("/attachments") and method == "POST":
        return httpx.post(
            url,
            files={"file": ("x.txt", io.BytesIO(b"x"), "text/plain")},
            headers=headers,
            timeout=30,
        )
    body = BODIES.get((method, path))
    return httpx.request(method, url, json=body, headers=headers, timeout=30)


def _without_ids(problem: dict, chat_id: str) -> str:
    """A Problem body without `trace_id`, with `chat_id` replaced by a
    placeholder, for comparing two 404s."""
    body = {k: v for k, v in problem.items() if k != "trace_id"}
    return json.dumps(body, sort_keys=True).replace(chat_id, "<chat_id>")


class TestAuthentication:
    """Every operation requires a valid bearer token. The API gateway answers
    401 `unauthenticated` with an RFC 6750 challenge
    (gears/system/api-gateway/src/middleware/auth.rs, `authn_middleware`)."""

    @pytest.mark.parametrize(("method", "path"), OPERATIONS)
    def test_missing_token_is_401(self, owned, method, path):
        """No Authorization header: reason `MISSING_BEARER`, challenge with
        `realm` only."""
        resp = _call(method, path, owned, NO_AUTH)
        assert_problem(resp, 401, "unauthenticated", reason="MISSING_BEARER")
        assert resp.headers.get_list("www-authenticate") == ['Bearer realm="api"']

    @pytest.mark.parametrize(("method", "path"), OPERATIONS)
    def test_unknown_token_is_401(self, owned, method, path):
        """A token the static authn plugin does not know: reason
        `AUTHN_FAILED`, challenge `invalid_token`."""
        resp = _call(method, path, owned, auth_headers("not-a-valid-token"))
        assert_problem(resp, 401, "unauthenticated", reason="AUTHN_FAILED")
        assert resp.headers.get_list("www-authenticate") == ['Bearer error="invalid_token"']


# Operations on A's resources that another user must see as 404.
FOREIGN_OPERATIONS = [
    (m, p) for (m, p) in OPERATIONS
    if "{chat_id}" in p
]
FOREIGN_MUTATIONS = [(m, p) for (m, p) in FOREIGN_OPERATIONS if m != "GET"]


@pytest.mark.parametrize("token", [TOKEN_USER_B, TOKEN_TENANT_B], ids=["same_tenant", "other_tenant"])
class TestIsolation:
    """User B (same tenant) and user C (other tenant) cannot reach A's chat."""

    @pytest.mark.parametrize(("method", "path"), FOREIGN_OPERATIONS)
    def test_foreign_resource_is_404(self, owned, token, method, path):
        """17-07: the 404 for A's chat is the one A gets for a chat id that
        does not exist: same body (resource type, detail, context) once the
        chat id is masked, so it does not reveal that the chat exists."""
        resp = _call(method, path, owned, auth_headers(token))
        masked = assert_problem(resp, 404, "not_found")

        unknown_ids = {**owned, "chat_id": str(uuid.uuid4())}
        unknown = assert_problem(
            _call(method, path, unknown_ids, auth_headers(TOKEN_USER_A)), 404, "not_found",
        )
        assert _without_ids(masked, owned["chat_id"]) == _without_ids(
            unknown, unknown_ids["chat_id"],
        ), (masked, unknown)

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.parametrize(("method", "path"), FOREIGN_OPERATIONS)
    def test_foreign_resource_not_sent_to_provider(self, owned, mock_provider, token, method, path):
        """A rejected foreign request neither calls the model nor uploads a
        file: the mock sees no POST. (Background cleanup of earlier tests may
        still send DELETEs.)"""
        assert _call(method, path, owned, auth_headers(token)).status_code == 404
        posts = [p for m, p in mock_provider.get_request_paths() if m == "POST"]
        assert posts == [], f"{method} {path} must not reach the provider"

    def test_foreign_chat_not_listed(self, owned, token):
        resp = httpx.get(f"{API_PREFIX}/chats", headers=auth_headers(token))
        assert resp.status_code == 200
        ids = {c["id"] for c in resp.json()["items"]}
        assert owned["chat_id"] not in ids

    def test_owner_resources_unchanged(self, owned, token):
        """Every foreign mutation of A's chat (404) leaves A's chat, messages,
        turn, attachment and reactions as they were."""
        chat_url = f"{API_PREFIX}/chats/{owned['chat_id']}"
        messages_before = list_messages(owned["chat_id"])

        for method, path in FOREIGN_MUTATIONS:
            resp = _call(method, path, owned, auth_headers(token))
            assert_problem(resp, 404, "not_found")

        chat = httpx.get(chat_url)
        assert chat.status_code == 200
        assert chat.json()["title"] == "owned by A"
        messages_after = list_messages(owned["chat_id"])
        assert messages_after == messages_before
        # The foreign PUT /reaction set no reaction on A's answer.
        answer = [m for m in messages_after if m["id"] == owned["message_id"]]
        assert [(m["role"], m["my_reaction"]) for m in answer] == [("assistant", None)]

        turn = httpx.get(f"{chat_url}/turns/{owned['request_id']}")
        assert turn.status_code == 200
        assert turn.json()["state"] == "done"

        att = httpx.get(f"{chat_url}/attachments/{owned['attachment_id']}")
        assert att.status_code == 200
        assert att.json()["status"] == "ready"


# Operations addressing a turn, message or attachment inside a chat.
NESTED_OPERATIONS = [
    (m, p) for (m, p) in FOREIGN_OPERATIONS
    if any(k in p for k in ("{request_id}", "{message_id}", "{attachment_id}"))
]


@pytest.fixture(scope="module")
def other_chat(server) -> str:
    """A second, empty chat of user A."""
    resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "other chat of A"})
    assert resp.status_code == 201, resp.text
    return resp.json()["id"]


class TestCrossChatIds:
    """The caller's own turn, message and attachment ids, addressed through
    another chat of the same caller, are 404: they are looked up within the
    chat of the path."""

    @pytest.mark.parametrize(("method", "path"), NESTED_OPERATIONS)
    def test_id_of_another_chat_is_404(self, owned, other_chat, method, path):
        resp = _call(method, path, {**owned, "chat_id": other_chat}, auth_headers(TOKEN_USER_A))
        assert_problem(resp, 404, "not_found")

    def test_owner_resources_unchanged(self, owned, other_chat):
        """The 404 mutations above changed nothing in the chat that holds the ids."""
        messages_before = list_messages(owned["chat_id"])
        for method, path in NESTED_OPERATIONS:
            if method != "GET":
                ids = {**owned, "chat_id": other_chat}
                assert _call(method, path, ids, auth_headers(TOKEN_USER_A)).status_code == 404
        assert list_messages(owned["chat_id"]) == messages_before
        assert list_messages(other_chat) == []
        chat_url = f"{API_PREFIX}/chats/{owned['chat_id']}"
        assert httpx.get(f"{chat_url}/turns/{owned['request_id']}").json()["state"] == "done"
        assert httpx.get(f"{chat_url}/attachments/{owned['attachment_id']}").json()["status"] == "ready"


@pytest.mark.usefixtures("same_utc_day")
class TestQuotaIsolation:
    """Usage is accounted per user."""

    @pytest.mark.timeout(20)
    def test_other_user_usage_is_not_charged(self, chat, mock_provider):
        """A's turn is charged to A (total daily grows by the turn's cost) and
        leaves B's usage unchanged. The turn runs in a chat of its own, so
        the module chat of TestIsolation stays at one turn."""
        def used(token: str) -> int:
            resp = httpx.get(f"{API_PREFIX}/quota/status", headers=auth_headers(token))
            assert resp.status_code == 200, resp.text
            return sum(
                p["used_credits_micro"] for t in resp.json()["tiers"] for p in t["periods"]
            )

        def used_total_daily(token: str) -> int:
            return find_period(get_quota_status(token=token), "total", "daily")["used_credits_micro"]

        before_a = used_total_daily(TOKEN_USER_A)
        before_b = used(TOKEN_USER_B)
        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(chat["id"], "Say OK again.")
        assert status == 200
        usage = provider_usage(mock_provider, expect_done(events).data["usage"])
        assert_no_reserves(USER_A_ID)

        # azure-gpt-4.1 multipliers (base.yaml): 3 credits_micro per input
        # token, 15 per output token.
        cost = usage["input_tokens"] * 3 + usage["output_tokens"] * 15
        assert used_total_daily(TOKEN_USER_A) - before_a == cost
        assert used(TOKEN_USER_B) == before_b
