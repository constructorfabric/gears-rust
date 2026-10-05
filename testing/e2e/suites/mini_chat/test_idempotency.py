# Updated: 2026-04-16 by Constructor Tech
"""Tests for request_id idempotency — conflict detection, replay priority, quota invariance."""

import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    assert_problem,
    delta_text,
    expect_done,
    expect_stream_started,
    find_period,
    get_quota_status,
    open_stream,
    parse_sse,
    poll_turn,
    slow_scenario,
    stream_message,
    turn_count,
)
from .mock_provider.responses import MockEvent, Scenario


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def stream_url(chat_id: str) -> str:
    return f"{API_PREFIX}/chats/{chat_id}/messages:stream"


def post_stream(chat_id: str, body: dict) -> httpx.Response:
    return httpx.post(
        stream_url(chat_id), json=body,
        headers={"Accept": "text/event-stream"}, timeout=30,
    )


def total_daily_used() -> int:
    return find_period(get_quota_status(), "total", "daily")["used_credits_micro"]


def assert_request_id_conflict(resp: httpx.Response) -> None:
    assert_problem(
        resp, 409, "aborted", reason="request_id_conflict",
    )


def _require_offline(request):
    if request.config.getoption("mode") == "online":
        pytest.skip("requires mock provider (offline mode)")


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestIdempotency:
    """Request-id idempotency conflict detection and replay semantics."""

    @pytest.mark.timeout(30)
    def test_running_turn_same_request_id_409(self, request, chat, mock_provider):
        """Resending the request_id of a running turn is 409 request_id_conflict."""
        _require_offline(request)
        chat_id = chat["id"]
        body = {"content": "Hello slow.", "request_id": str(uuid.uuid4())}
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))

        with open_stream(chat_id, body["content"], request_id=body["request_id"]) as first:
            first.read_until_started()
            assert_request_id_conflict(post_stream(chat_id, body))
            expect_done(first.drain())
        assert turn_count(chat_id) == 1

    @pytest.mark.timeout(30)
    def test_failed_turn_same_request_id_409(self, request, chat, mock_provider):
        """Resending the request_id of a failed turn is 409 request_id_conflict."""
        _require_offline(request)
        chat_id = chat["id"]
        body = {"content": "Fail please.", "request_id": str(uuid.uuid4())}
        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={"code": "server_error", "message": "fail"},
            events=[MockEvent("response.output_text.delta", {"delta": "x"})],
        ))
        assert post_stream(chat_id, body).status_code == 200
        assert poll_turn(chat_id, body["request_id"])["state"] == "error"

        assert_request_id_conflict(post_stream(chat_id, body))
        assert turn_count(chat_id) == 1

    @pytest.mark.timeout(30)
    def test_cancelled_turn_same_request_id_409(self, request, chat, mock_provider):
        """Resending the request_id of a cancelled turn is 409 request_id_conflict."""
        _require_offline(request)
        chat_id = chat["id"]
        body = {"content": "Write slowly.", "request_id": str(uuid.uuid4())}
        mock_provider.set_next_scenario(slow_scenario(20, slow=0.3))

        with open_stream(chat_id, body["content"], request_id=body["request_id"]) as s:
            s.read_until(lambda e: e.event == "delta")
        assert poll_turn(chat_id, body["request_id"])["state"] == "cancelled"

        assert_request_id_conflict(post_stream(chat_id, body))
        assert turn_count(chat_id) == 1

    @pytest.mark.timeout(30)
    def test_request_id_replaced_by_retry_409(self, chat):
        """The request_id of a turn replaced by retry is not replayed: 409 request_id_conflict."""
        chat_id = chat["id"]
        body = {"content": "Replace me.", "request_id": str(uuid.uuid4())}
        status, events, _ = stream_message(chat_id, body["content"], request_id=body["request_id"])
        assert status == 200
        expect_done(events)
        poll_turn(chat_id, body["request_id"], ("done",))

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{body['request_id']}/retry",
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert resp.status_code == 200, resp.text
        expect_done(parse_sse(resp.text))

        assert_request_id_conflict(post_stream(chat_id, body))
        assert turn_count(chat_id) == 2

    @pytest.mark.timeout(30)
    def test_request_id_of_deleted_turn_409(self, chat):
        """The request_id of a turn removed by DELETE /turns/{request_id} is
        neither replayed nor reused: 409 request_id_conflict, no new turn."""
        chat_id = chat["id"]
        body = {"content": "Delete me.", "request_id": str(uuid.uuid4())}
        status, events, _ = stream_message(chat_id, body["content"], request_id=body["request_id"])
        assert status == 200
        expect_done(events)
        poll_turn(chat_id, body["request_id"], ("done",))

        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/turns/{body['request_id']}", timeout=10)
        assert resp.status_code == 204

        assert_request_id_conflict(post_stream(chat_id, body))
        assert turn_count(chat_id) == 1

    @pytest.mark.timeout(30)
    def test_replay_priority_over_parallel_check(self, request, chat, mock_provider):
        """Replay of a completed turn returns 200 even while another turn is running."""
        _require_offline(request)
        chat_id = chat["id"]
        rid_a = str(uuid.uuid4())
        status_a, events_a, _ = stream_message(chat_id, "Turn A.", request_id=rid_a)
        assert status_a == 200
        expect_done(events_a)

        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))
        with open_stream(chat_id, "Turn B slow.", request_id=str(uuid.uuid4())) as b:
            b.read_until_started()

            replay = post_stream(chat_id, {"content": "Turn A.", "request_id": rid_a})
            assert replay.status_code == 200, replay.text
            replay_events = parse_sse(replay.text)
            assert expect_stream_started(replay_events).data["is_new_turn"] is False
            assert delta_text(replay_events) == delta_text(events_a)
            expect_done(b.drain())

    @pytest.mark.usefixtures("offline_only", "same_utc_day")
    @pytest.mark.timeout(30)
    def test_replay_does_not_modify_quota_or_call_provider(self, chat, mock_provider):
        """Replaying a completed turn changes neither quota nor provider traffic
        (offline only: provider traffic is what the mock saw)."""
        chat_id = chat["id"]
        rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say OK.", request_id=rid)
        assert status == 200
        expect_done(events)
        poll_turn(chat_id, rid, ("done",))

        used_before = total_daily_used()
        mock_provider.clear_captured_requests()
        for _ in range(3):
            resp = post_stream(chat_id, {"content": "Say OK.", "request_id": rid})
            assert resp.status_code == 200
            assert delta_text(parse_sse(resp.text)) == delta_text(events)

        assert total_daily_used() == used_before
        assert mock_provider.get_captured_requests() == []
        assert turn_count(chat_id) == 1

    @pytest.mark.timeout(30)
    def test_request_id_of_another_chat_starts_a_new_turn(self, chat):
        """The idempotency key is (chat_id, request_id): the request_id of a
        completed turn in another chat of the same user starts a new turn
        here, not a replay and not a conflict."""
        other_chat = httpx.post(f"{API_PREFIX}/chats", json={}, timeout=10)
        assert other_chat.status_code == 201, other_chat.text
        other_id = other_chat.json()["id"]
        rid = str(uuid.uuid4())
        status, events, _ = stream_message(other_id, "Say OK.", request_id=rid)
        assert status == 200
        expect_done(events)

        resp = post_stream(chat["id"], {"content": "Same key, other chat.", "request_id": rid})
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        started = expect_stream_started(events)
        assert (started.data["request_id"], started.data["is_new_turn"]) == (rid, True)
        expect_done(events)
        assert poll_turn(chat["id"], rid, ("done",))["state"] == "done"
        assert turn_count(chat["id"]) == 1
        assert turn_count(other_id) == 1

    def test_request_id_not_a_uuid_422(self, chat, mock_provider):
        """A `request_id` that is not a UUID fails body deserialization: 422
        invalid_argument, no turn, provider not called."""
        mock_provider.clear_captured_requests()
        resp = post_stream(chat["id"], {"content": "Hello.", "request_id": "not-a-uuid"})
        assert_problem(resp, 422, "invalid_argument")
        assert turn_count(chat["id"]) == 0
        assert mock_provider.get_captured_requests() == []
