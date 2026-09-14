"""Tests for turn lifecycle — cancelled/failed null message_id, terminal immutability."""

import time
import uuid

import httpx
import pytest

from .conftest import API_PREFIX, expect_done, parse_sse, stream_message
from .mock_provider.responses import MockEvent, Scenario


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def poll_turn_terminal(chat_id: str, request_id: str,
                       timeout: float = 15.0) -> dict:
    """Poll GET /turns/{request_id} until the turn reaches a terminal state."""
    deadline = time.monotonic() + timeout
    body = None
    while time.monotonic() < deadline:
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}", timeout=5
        )
        if resp.status_code == 200:
            body = resp.json()
            if body["state"] in ("done", "error", "cancelled"):
                return body
        time.sleep(0.3)
    state = body["state"] if body else "no response"
    raise AssertionError(
        f"Turn {request_id} did not reach terminal state within {timeout}s "
        f"(last state: {state})"
    )


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestTurnLifecycle:
    """Turn state transitions, null message_id cases, terminal immutability."""

    def test_cancelled_without_content_null_message_id(self, request, chat, mock_provider):
        """Cancelling before any content is accumulated yields null assistant_message_id."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Very slow scenario — 5s between events guarantees disconnect arrives
        # before any content delta. Only 3 events to keep total duration short.
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"chunk{i} "})
            for i in range(3)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=5.0, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Write slowly.", "request_id": request_id}

        # Open the SSE connection, then disconnect without reading any content.
        # The 5s delay ensures the mock provider hasn't sent any delta yet.
        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            # Don't read anything — disconnect immediately

        # Poll until terminal
        turn = poll_turn_terminal(chat_id, request_id)
        assert turn["state"] == "cancelled"

        # No assistant message should exist for a content-less cancellation
        msgs_resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/messages", timeout=10
        )
        assert msgs_resp.status_code == 200
        assistant_msgs = [
            m for m in msgs_resp.json()["items"]
            if m["role"] == "assistant" and m.get("request_id") == request_id
        ]
        assert len(assistant_msgs) == 0, (
            f"Expected no assistant message for content-less cancel, "
            f"got {len(assistant_msgs)}"
        )

    def test_failed_turn_null_message_id(self, request, chat, mock_provider):
        """A turn that fails without producing content has null assistant_message_id."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Fail immediately with no content events
        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={"code": "server_error", "message": "fail"},
            events=[],
        ))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Fail now.", "request_id": request_id}
        resp = httpx.post(
            url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        # The stream completes (server sends error terminal in SSE)
        assert resp.status_code == 200

        # Poll until terminal
        turn = poll_turn_terminal(chat_id, request_id)
        assert turn["state"] == "error"

        # No assistant message should exist for a no-content failure
        msgs_resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/messages", timeout=10
        )
        assert msgs_resp.status_code == 200
        assistant_msgs = [
            m for m in msgs_resp.json()["items"]
            if m["role"] == "assistant" and m.get("request_id") == request_id
        ]
        assert len(assistant_msgs) == 0, (
            f"Expected no assistant message for no-content failure, "
            f"got {len(assistant_msgs)}"
        )

    def test_terminal_state_immutable_on_repeated_reads(self, chat):
        """A completed turn stays in 'done' state on repeated queries (CAS invariant)."""
        # TODO: Ideally this would verify the DB-level CAS by racing two
        # finalization attempts, but that requires internal hooks. For now we
        # verify the observable effect: a done turn remains done.
        chat_id = chat["id"]
        rid = str(uuid.uuid4())

        status, events, _ = stream_message(chat_id, "Say OK.", request_id=rid)
        assert status == 200
        expect_done(events)

        # Poll until DB commits the terminal state (SSE done can race the DB write)
        turn = poll_turn_terminal(chat_id, rid)
        assert turn["state"] == "done"

        # Now verify repeated reads still return "done"
        for _ in range(5):
            resp = httpx.get(
                f"{API_PREFIX}/chats/{chat_id}/turns/{rid}", timeout=5
            )
            assert resp.status_code == 200
            assert resp.json()["state"] == "done"

    def test_turn_state_machine_terminal_immutable(self, chat):
        """A terminal turn state does not change over time."""
        chat_id = chat["id"]
        rid = str(uuid.uuid4())

        status, events, _ = stream_message(chat_id, "Say OK.", request_id=rid)
        assert status == 200
        expect_done(events)

        # Poll until DB commits the terminal state
        turn = poll_turn_terminal(chat_id, rid)
        assert turn["state"] == "done"
        state1 = turn["state"]

        # Wait and verify state is unchanged
        time.sleep(1.0)

        resp2 = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid}", timeout=5
        )
        assert resp2.status_code == 200
        assert resp2.json()["state"] == state1
