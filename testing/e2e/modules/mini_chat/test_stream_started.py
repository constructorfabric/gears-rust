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

import time
import uuid

import pytest
import httpx

from .conftest import API_PREFIX, expect_done, expect_stream_started, parse_sse, stream_message



# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def stream_message_raw_partial(chat_id: str, content: str, read_bytes: int = 512):
    """Start a streaming request, read a small chunk, then close the connection.

    Returns (request_id, partial_raw) where request_id is from the body
    (caller-generated) and partial_raw is the bytes read before disconnect.
    """
    request_id = str(uuid.uuid4())
    url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
    body = {"content": content, "request_id": request_id}
    # httpx.stream() context manager — exiting closes the connection
    partial = b""
    with httpx.stream(
        "POST", url, json=body,
        headers={"Accept": "text/event-stream"},
        timeout=30,
    ) as resp:
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
        # Read chunks until we have enough bytes, then exit — triggers cancellation
        for chunk in resp.iter_bytes():
            partial += chunk
            if len(partial) >= read_bytes:
                break
    return request_id, partial


def poll_turn_status(chat_id: str, request_id: str, target_state: str,
                     timeout: float = 15.0) -> dict:
    """Poll GET /turns/{request_id} until the target state or timeout."""
    deadline = time.monotonic() + timeout
    body = None
    while time.monotonic() < deadline:
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}", timeout=5
        )
        if resp.status_code == 200:
            body = resp.json()
            if body["state"] == target_state:
                return body
        time.sleep(0.3)
    state = body["state"] if body else "no response"
    raise AssertionError(
        f"Turn {request_id} did not reach '{target_state}' within {timeout}s "
        f"(last state: {state})"
    )


# ---------------------------------------------------------------------------
# Tests: stream_started event on initial send
# ---------------------------------------------------------------------------

class TestStreamStartedOnSend:
    """stream_started is the first SSE event on POST /messages:stream."""

    def test_stream_started_is_first_event(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        assert len(events) >= 2, f"Expected >=2 events, got {len(events)}"
        assert events[0].event == "stream_started", (
            f"First event should be stream_started, got {events[0].event}"
        )

    def test_stream_started_has_request_id(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        ss = expect_stream_started(events)
        rid = ss.data.get("request_id")
        assert rid is not None, "stream_started should have request_id"
        uuid.UUID(rid)  # validates it's a UUID

    def test_stream_started_has_message_id(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        ss = expect_stream_started(events)
        mid = ss.data.get("message_id")
        assert mid is not None, "stream_started should have message_id"
        uuid.UUID(mid)  # validates it's a UUID

    def test_stream_started_is_new_turn_true_on_send(self, provider_chat):
        """Live generation should have is_new_turn=true."""
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
        ss = expect_stream_started(events)
        assert ss.data.get("is_new_turn") is True

    def test_stream_started_request_id_matches_client_id(self, provider_chat):
        """When client provides request_id, stream_started echoes it back."""
        request_id = str(uuid.uuid4())
        _, events, _ = stream_message(provider_chat["id"], "Say OK.", request_id=request_id)
        ss = expect_stream_started(events)
        assert ss.data["request_id"] == request_id

    def test_stream_started_request_id_matches_turn_status(self, provider_chat):
        """request_id from stream_started matches GET /turns/{request_id}."""
        _, events, _ = stream_message(provider_chat["id"], "Say OK.")
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

class TestStreamStartedOrdering:
    """Grammar: stream_started ping* (delta | tool)* citations? (done | error)."""

    def test_stream_started_before_deltas_before_done(self, provider_chat):
        _, events, _ = stream_message(provider_chat["id"], "Say hello briefly.")
        types = [e.event for e in events]

        # stream_started must be first
        assert types[0] == "stream_started"

        # done must be last
        assert types[-1] == "done"

        # No stream_started after the first one
        assert types.count("stream_started") == 1

        # Deltas between stream_started and done
        deltas = [e for e in events if e.event == "delta"]
        assert len(deltas) > 0

    def test_pings_only_between_stream_started_and_first_content(self, provider_chat):
        """Pings should only appear before the first delta/tool."""
        _, events, _ = stream_message(provider_chat["id"], "Say hi.")
        first_content_idx = None
        for i, e in enumerate(events):
            if e.event in ("delta", "tool"):
                first_content_idx = i
                break
        if first_content_idx is not None:
            for e in events[first_content_idx:]:
                assert e.event != "ping", "Ping after content events"


# ---------------------------------------------------------------------------
# Tests: stream_started on retry and edit
# ---------------------------------------------------------------------------

class TestStreamStartedOnMutation:
    """stream_started carries a NEW request_id on retry and edit."""

    def test_retry_emits_stream_started_with_new_request_id(self, provider_chat):
        chat_id = provider_chat["id"]

        # Complete a turn
        orig_rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say ALPHA.", request_id=orig_rid)
        assert status == 200
        expect_done(events)

        # Wait for CAS finalization before mutating
        poll_turn_status(chat_id, orig_rid, "done")

        # Retry
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{orig_rid}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Retry failed: {resp.status_code} {resp.text}"
        retry_events = parse_sse(resp.text)

        ss = expect_stream_started(retry_events)
        new_rid = ss.data["request_id"]
        assert new_rid != orig_rid, "Retry should generate a new request_id"
        uuid.UUID(new_rid)

        # Verify done event present
        expect_done(retry_events)

    def test_edit_emits_stream_started_with_new_request_id(self, provider_chat):
        chat_id = provider_chat["id"]

        # Complete a turn
        orig_rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say BETA.", request_id=orig_rid)
        assert status == 200
        expect_done(events)

        # Wait for CAS finalization before mutating
        poll_turn_status(chat_id, orig_rid, "done")

        # Edit
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}/turns/{orig_rid}",
            json={"content": "Say GAMMA instead."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Edit failed: {resp.status_code} {resp.text}"
        edit_events = parse_sse(resp.text)

        ss = expect_stream_started(edit_events)
        new_rid = ss.data["request_id"]
        assert new_rid != orig_rid, "Edit should generate a new request_id"
        uuid.UUID(new_rid)

        expect_done(edit_events)


# ---------------------------------------------------------------------------
# Tests: stream_started on replay (idempotent)
# ---------------------------------------------------------------------------

class TestStreamStartedOnReplay:
    """Replay of a completed turn emits stream_started with is_new_turn=false."""

    def test_replay_emits_stream_started_with_is_new_turn_false(self, provider_chat):
        chat_id = provider_chat["id"]

        # Complete a turn
        rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say OK.", request_id=rid)
        assert status == 200
        ss_orig = expect_stream_started(events)
        orig_msg_id = ss_orig.data["message_id"]

        # Replay same request_id
        status2, events2, _ = stream_message(chat_id, "Say OK.", request_id=rid)
        assert status2 == 200

        ss_replay = expect_stream_started(events2)
        assert ss_replay.data["is_new_turn"] is False
        assert ss_replay.data["message_id"] == orig_msg_id
        assert ss_replay.data["request_id"] == rid


# ---------------------------------------------------------------------------
# Tests: cancelled stream persists partial message
# ---------------------------------------------------------------------------

class TestCancelledMessagePersistence:
    """Cancelled streams with accumulated content persist a partial assistant message."""

    def test_cancelled_turn_has_assistant_message_id(self, provider_chat):
        """Disconnect mid-stream; cancelled turn should have assistant_message_id."""
        chat_id = provider_chat["id"]

        # Use a long prompt to trigger slow-response scenario in mock.
        # read_bytes=256 reads just stream_started + first deltas, then disconnects.
        request_id, _ = stream_message_raw_partial(
            chat_id,
            "Write a detailed 500-word essay about the history of computing.",
            read_bytes=256,
        )

        # Poll until turn reaches 'cancelled' state
        turn = poll_turn_status(chat_id, request_id, "cancelled")
        # D4: cancelled turn with accumulated text should have assistant_message_id
        assert turn.get("assistant_message_id") is not None, (
            f"Cancelled turn should have assistant_message_id, got: {turn}"
        )

    def test_cancelled_message_appears_in_messages(self, provider_chat):
        """The partial assistant message from a cancelled turn appears in GET /messages."""
        chat_id = provider_chat["id"]

        # Read a small chunk then disconnect to trigger cancellation.
        # Use a very long prompt to maximize generation time.
        request_id, _ = stream_message_raw_partial(
            chat_id,
            "Write a 2000-word essay about the complete history of computing "
            "from Charles Babbage to modern quantum computers. Include every "
            "major milestone, inventor, and breakthrough in chronological order.",
            read_bytes=512,
        )

        # Poll until turn reaches a terminal state.
        # Fast providers/mock may complete before disconnect → "done" is also valid.
        turn = None
        deadline = time.monotonic() + 20.0
        while time.monotonic() < deadline:
            resp = httpx.get(
                f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}", timeout=5
            )
            if resp.status_code == 200:
                turn = resp.json()
                if turn["state"] in ("cancelled", "done"):
                    break
            time.sleep(0.3)
        assert turn is not None and turn["state"] in ("cancelled", "done"), (
            f"Turn did not reach terminal state: {turn}"
        )
        msg_id = turn.get("assistant_message_id")
        assert msg_id is not None, "Should have assistant_message_id"

        # Fetch messages — partial assistant message should be present
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        asst_msgs = [m for m in msgs if m["role"] == "assistant"]
        asst_ids = [m["id"] for m in asst_msgs]
        assert msg_id in asst_ids, (
            f"Cancelled assistant message {msg_id} not found in messages. "
            f"Got IDs: {asst_ids}"
        )

    def test_retry_cancelled_turn_produces_new_message(self, provider_chat):
        """Retrying a cancelled turn produces a complete response with a new message_id."""
        chat_id = provider_chat["id"]

        # Cancel mid-stream
        request_id, _ = stream_message_raw_partial(
            chat_id,
            "Write a very long and detailed explanation of every prime number "
            "below 1000, their properties, and mathematical significance.",
            read_bytes=256,
        )
        turn = poll_turn_status(chat_id, request_id, "cancelled", timeout=20.0)
        partial_msg_id = turn.get("assistant_message_id")

        # Retry the cancelled turn
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Retry failed: {resp.status_code} {resp.text}"
        retry_events = parse_sse(resp.text)

        # Retry should emit stream_started with a new request_id and message_id
        ss = expect_stream_started(retry_events)
        new_rid = ss.data["request_id"]
        new_msg_id = ss.data["message_id"]
        assert new_rid != request_id, "Retry should use a new request_id"
        assert new_msg_id is not None, "stream_started should have message_id"

        # The new message should be different from the partial one
        if partial_msg_id is not None:
            assert new_msg_id != partial_msg_id, (
                "Retry should produce a new message, not reuse the partial"
            )

        # Retry should complete with a done event
        expect_done(retry_events)

        # Verify the new turn is in 'done' state
        new_turn = poll_turn_status(chat_id, new_rid, "done")
        assert new_turn["assistant_message_id"] == new_msg_id
