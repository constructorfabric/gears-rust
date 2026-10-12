"""Tests for the stream_started SSE lifecycle event and cancelled message persistence.

Covers:
- stream_started as first SSE event on initial send, retry, and edit
- stream_started.request_id / message_id / is_new_turn fields
- stream_started.request_id matches Turn Status API
- Full event grammar ordering with stream_started
- stream_started emitted on replay with is_new_turn=false
- Cancelled stream persists partial assistant message
- Cancelled message appears in GET /messages
- Retry of cancelled turn replaces partial message
"""

import uuid

import pytest
import httpx

from .conftest import (
    API_PREFIX,
    delta_text,
    expect_done,
    expect_stream_started,
    open_stream,
    parse_sse,
    poll_turn,
    slow_scenario,
    stream_message,
)

_STREAM_HEADERS = {"Accept": "text/event-stream"}


# ---------------------------------------------------------------------------
# Tests: stream_started event on initial send
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestStreamStartedOnSend:
    """stream_started is the first SSE event on POST /messages:stream."""

    def test_stream_started_is_first_event(self, provider_chat):
        """04-02, 05-01, 05-02: without a client request_id, the first event is
        `stream_started` with a server-generated request_id, the message_id
        (both UUIDs) and `is_new_turn: true`."""
        url = f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream"
        resp = httpx.post(url, json={"content": "Say OK."}, headers=_STREAM_HEADERS, timeout=90)
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        assert len(events) >= 2, [e.event for e in events]
        first = events[0]
        assert first.event == "stream_started", [e.event for e in events]
        uuid.UUID(first.data["request_id"])
        uuid.UUID(first.data["message_id"])
        assert first.data["is_new_turn"] is True

    def test_stream_started_request_id_matches_client_id(self, provider_chat):
        """When client provides request_id, stream_started echoes it back."""
        request_id = str(uuid.uuid4())
        url = f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream"
        resp = httpx.post(url, json={"content": "Say OK.", "request_id": request_id}, headers=_STREAM_HEADERS, timeout=90)
        events = parse_sse(resp.text) if resp.status_code == 200 else []
        ss = expect_stream_started(events)
        assert ss.data["request_id"] == request_id

    def test_stream_started_request_id_matches_turn_status(self, provider_chat):
        """request_id from stream_started matches GET /turns/{request_id}."""
        url = f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream"
        resp = httpx.post(url, json={"content": "Say OK."}, headers=_STREAM_HEADERS, timeout=90)
        events = parse_sse(resp.text) if resp.status_code == 200 else []
        ss = expect_stream_started(events)
        rid = ss.data["request_id"]

        resp = httpx.get(f"{API_PREFIX}/chats/{provider_chat['id']}/turns/{rid}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["request_id"] == rid
        assert body["state"] == "done"


# ---------------------------------------------------------------------------
# Tests: event ordering grammar
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestStreamStartedOrdering:
    """Grammar: stream_started ping* (delta | tool)* citations? (done | error)."""

    def test_stream_started_before_deltas_before_done(self, provider_chat):
        """A plain message: one `stream_started`, then deltas (and pings), then
        exactly one terminal event, `done`, as the last event."""
        url = f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream"
        resp = httpx.post(url, json={"content": "Say hello briefly."}, headers=_STREAM_HEADERS, timeout=90)
        assert resp.status_code == 200, resp.text
        types = [e.event for e in parse_sse(resp.text)]

        assert types[0] == "stream_started", types
        assert types[-1] == "done", types
        assert set(types[1:-1]) <= {"ping", "delta"}, types
        assert "delta" in types, types


# ---------------------------------------------------------------------------
# Tests: stream_started on retry and edit
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestStreamStartedOnMutation:
    """stream_started carries a NEW request_id on retry and edit."""

    @pytest.mark.timeout(30)
    def test_retry_emits_stream_started_with_new_request_id(self, provider_chat):
        chat_id = provider_chat["id"]

        # Complete a turn
        orig_rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say ALPHA.", request_id=orig_rid)
        assert status == 200
        expect_done(events)

        # Wait for CAS finalization before mutating
        poll_turn(chat_id, orig_rid, ("done",))

        # Retry
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{orig_rid}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Retry failed: {resp.status_code} {resp.text}"
        assert resp.headers["content-type"].startswith("text/event-stream"), resp.headers
        retry_events = parse_sse(resp.text)

        ss = expect_stream_started(retry_events)
        new_rid = ss.data["request_id"]
        assert new_rid != orig_rid, "Retry should generate a new request_id"
        uuid.UUID(new_rid)

        # Verify done event present
        expect_done(retry_events)

    @pytest.mark.timeout(30)
    def test_edit_emits_stream_started_with_new_request_id(self, provider_chat):
        chat_id = provider_chat["id"]

        # Complete a turn
        orig_rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say BETA.", request_id=orig_rid)
        assert status == 200
        expect_done(events)

        # Wait for CAS finalization before mutating
        poll_turn(chat_id, orig_rid, ("done",))

        # Edit
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}/turns/{orig_rid}",
            json={"content": "Say GAMMA instead."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Edit failed: {resp.status_code} {resp.text}"
        assert resp.headers["content-type"].startswith("text/event-stream"), resp.headers
        edit_events = parse_sse(resp.text)

        ss = expect_stream_started(edit_events)
        new_rid = ss.data["request_id"]
        assert new_rid != orig_rid, "Edit should generate a new request_id"
        uuid.UUID(new_rid)
        assert ss.data["is_new_turn"] is True

        # Same grammar and `done` fields as a send.
        types = [e.event for e in edit_events]
        assert types[0] == "stream_started", types
        assert types[-1] == "done", types
        assert set(types[1:-1]) <= {"ping", "delta"}, types
        done = expect_done(edit_events).data
        assert (done["selected_model"], done["effective_model"], done["quota_decision"]) == (
            provider_chat["model"], provider_chat["model"], "allow",
        )


# ---------------------------------------------------------------------------
# Tests: stream_started on replay (idempotent)
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestStreamStartedOnReplay:
    """Replay of a completed turn emits stream_started with is_new_turn=false."""

    def test_replay_emits_stream_started_with_is_new_turn_false(self, provider_chat):
        chat_id = provider_chat["id"]

        # Complete a turn
        rid = str(uuid.uuid4())
        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        resp = httpx.post(url, json={"content": "Say OK.", "request_id": rid}, headers=_STREAM_HEADERS, timeout=90)
        status = resp.status_code
        events = parse_sse(resp.text) if status == 200 else []
        assert status == 200
        ss_orig = expect_stream_started(events)
        orig_msg_id = ss_orig.data["message_id"]

        # Replay same request_id
        resp2 = httpx.post(url, json={"content": "Say OK.", "request_id": rid}, headers=_STREAM_HEADERS, timeout=90)
        status2 = resp2.status_code
        events2 = parse_sse(resp2.text) if status2 == 200 else []
        assert status2 == 200
        assert resp2.headers["content-type"].startswith("text/event-stream"), resp2.headers

        ss_replay = expect_stream_started(events2)
        assert ss_replay.data["is_new_turn"] is False
        assert ss_replay.data["message_id"] == orig_msg_id
        assert ss_replay.data["request_id"] == rid
        assert delta_text(events2) == delta_text(events)
        # `done` is rebuilt from the stored turn and message.
        done, replayed = expect_done(events).data, expect_done(events2).data
        fields = ("usage", "effective_model", "selected_model", "quota_decision")
        assert {k: replayed.get(k) for k in fields} == {k: done.get(k) for k in fields}, (
            done, replayed,
        )
        assert replayed["quota_decision"] == "allow", replayed
        assert "downgrade_from" not in replayed, replayed


# ---------------------------------------------------------------------------
# Tests: cancelled stream persists partial message
# ---------------------------------------------------------------------------

class TestCancelledMessagePersistence:
    """A client disconnect cancels the turn and persists the text received so far.

    The mock sends one delta every 0.3 s; the client disconnects after the
    third delta, long before the provider would finish (20 deltas).
    """

    @pytest.fixture(autouse=True)
    def _offline_only(self, request):
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (slow scenario)")

    @staticmethod
    def _cancel_after_three_deltas(chat_id: str, mock_provider) -> tuple[str, str, str]:
        """Return (request_id, message_id from stream_started, received text)."""
        rid = str(uuid.uuid4())
        mock_provider.set_next_scenario(slow_scenario(20, slow=0.3))
        with open_stream(chat_id, "Write a long essay.", request_id=rid) as s:
            started = s.read_until_started()
            seen = []
            s.read_until(lambda e: e.event == "delta" and (seen.append(e) or len(seen) == 3))
            received = delta_text(s.events)
        assert received == "w0 w1 w2 "
        return rid, started.data["message_id"], received

    @pytest.mark.timeout(30)
    def test_cancelled_turn_has_assistant_message_id(self, chat, mock_provider):
        """The cancelled turn points at the pre-allocated assistant message."""
        rid, message_id, _ = self._cancel_after_three_deltas(chat["id"], mock_provider)

        turn = poll_turn(chat["id"], rid, ("cancelled",))
        assert turn["state"] == "cancelled"
        assert turn["assistant_message_id"] == message_id

    @pytest.mark.timeout(30)
    def test_cancelled_message_content_starts_with_received_deltas(self, chat, mock_provider):
        """GET /messages holds the partial answer: it starts with the deltas the
        client received and is a prefix of the full scenario text (the gear may
        read a few more deltas than the client before it sees the cancel)."""
        rid, message_id, received = self._cancel_after_three_deltas(chat["id"], mock_provider)
        poll_turn(chat["id"], rid, ("cancelled",))

        resp = httpx.get(f"{API_PREFIX}/chats/{chat['id']}/messages")
        assert resp.status_code == 200
        assistant = [m for m in resp.json()["items"] if m["role"] == "assistant"]
        assert [(m["id"], m["request_id"]) for m in assistant] == [(message_id, rid)]
        content = assistant[0]["content"]
        full_text = "".join(f"w{i} " for i in range(20))
        assert content.startswith(received), (content, received)
        assert full_text.startswith(content), content

    @pytest.mark.timeout(30)
    def test_retry_cancelled_turn_produces_new_message(self, chat, mock_provider):
        """Retrying a cancelled turn completes with a new request_id and message_id."""
        rid, partial_msg_id, _ = self._cancel_after_three_deltas(chat["id"], mock_provider)
        poll_turn(chat["id"], rid, ("cancelled",))

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/turns/{rid}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Retry failed: {resp.status_code} {resp.text}"
        retry_events = parse_sse(resp.text)
        ss = expect_stream_started(retry_events)
        new_rid = ss.data["request_id"]
        new_msg_id = ss.data["message_id"]
        assert new_rid != rid
        assert new_msg_id != partial_msg_id
        expect_done(retry_events)

        new_turn = poll_turn(chat["id"], new_rid, ("done",))
        assert new_turn["assistant_message_id"] == new_msg_id
