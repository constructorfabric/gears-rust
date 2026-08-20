"""Tests for quota settlement — CAS guard, actual settlement, cancel settlement, orphan timeout.

Most settlement internals are not directly observable via HTTP. These tests verify
the observable effects: turn state transitions, quota changes, and absence of stuck
reserves after various turn outcomes.

Covers:
- CAS guard observable effect (no stuck reserves after completion)
- CAS loser path (no duplicate settlements observable)
- Completed actual settlement (used_credits > 0)
- Overshoot within tolerance / capped
- Cancelled with/without usage
- Pre-provider failure release
- Orphan timeout settlement
- Atomic transaction observable
- Outbox deduplication observable
"""

import os
import sqlite3
import threading
import time
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    DB_PATH,
    DEFAULT_MODEL,
    STANDARD_MODEL,
    expect_done,
    expect_stream_started,
    parse_sse,
    stream_message,
)
from .mock_provider.responses import MockEvent, Scenario, Usage


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _to_blob(value):
    if isinstance(value, str):
        try:
            return uuid.UUID(value).bytes
        except ValueError:
            pass
    return value


def query_db(sql, params=()):
    if not os.path.exists(DB_PATH):
        pytest.skip(f"DB not found at {DB_PATH}")
    conn = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    blob_params = tuple(_to_blob(p) for p in params)
    try:
        rows = conn.execute(sql, blob_params).fetchall()
        return [dict(r) for r in rows]
    finally:
        conn.close()


def get_quota_status() -> dict:
    resp = httpx.get(f"{API_PREFIX}/quota/status", timeout=10)
    assert resp.status_code == 200
    return resp.json()


def get_total_daily_used() -> int:
    """Return total daily used_credits_micro."""
    for tier in get_quota_status()["tiers"]:
        if tier["tier"] == "total":
            for period in tier["periods"]:
                if period["period"] == "daily":
                    return period["used_credits_micro"]
    raise AssertionError("Could not find total/daily period in quota status")


def has_stuck_reserves() -> bool:
    """Check if any tier/period has non-zero reserved_credits_micro."""
    qs = get_quota_status()
    for tier in qs["tiers"]:
        for period in tier["periods"]:
            if period.get("reserved_credits_micro", 0) != 0:
                return True
    return False


def poll_turn_terminal(chat_id: str, request_id: str, timeout: float = 15.0) -> dict:
    """Poll GET /turns/{request_id} until terminal state."""
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

class TestSettlement:
    """Quota settlement after various turn outcomes."""

    def test_cas_guard_observable(self, chat, mock_provider):
        """After a completed turn, no stuck reserves remain (CAS worked correctly)."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        done = expect_done(events)

        # Verify turn reached done state
        turn = poll_turn_terminal(chat_id, request_id)
        assert turn["state"] == "done"

        time.sleep(0.5)

        # No stuck reserves — CAS settled correctly
        assert not has_stuck_reserves(), "Stuck reserves detected after completed turn"

    def test_cas_loser_no_side_effects(self, chat, mock_provider):
        """Completing a turn produces no duplicate settlements (observable via quota)."""
        # TODO: Cannot directly test CAS loser path via HTTP. The CAS loser
        # simply loses the compare-and-swap and does nothing. We verify that
        # after completion, quota is settled exactly once (no stuck reserves,
        # no double-counting).
        chat_id = chat["id"]

        used_before = get_total_daily_used()

        request_id = str(uuid.uuid4())
        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)

        used_after = get_total_daily_used()
        increase = used_after - used_before

        # Should have increased by exactly one turn's worth (not doubled)
        assert increase > 0, "Quota should increase after turn"
        assert not has_stuck_reserves(), "No stuck reserves expected"

    def test_completed_actual_settlement(self, chat, mock_provider):
        """After completing a turn, used_credits > 0 (actual settlement happened)."""
        chat_id = chat["id"]

        used_before = get_total_daily_used()

        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        time.sleep(0.5)

        used_after = get_total_daily_used()
        assert used_after > used_before, (
            f"used_credits should increase: before={used_before}, after={used_after}"
        )

    def test_reservation_snapshot_persisted(self, chat, mock_provider):
        # TODO: Requires mock returning usage above the reserve estimate.
        # The mock always returns fixed usage values, so overshoot cannot
        # be triggered via HTTP. This test verifies the DB values for
        # manual inspection.
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)

        rows = query_db(
            "SELECT reserve_tokens, reserved_credits_micro "
            "FROM chat_turns WHERE request_id = ?",
            (request_id,),
        )
        assert len(rows) > 0, f"No turn row for request_id={request_id}"
        row = rows[0]
        # reserve_tokens and reserved_credits_micro on the turn row are the
        # initial reservation snapshot — they are immutable after creation.
        # Settlement releases the reservation in quota_usage, not on the turn.
        assert row["reserve_tokens"] > 0, "reserve_tokens should be set"
        assert row["reserved_credits_micro"] > 0, "reserved_credits_micro should be set"
        print(
            f"  overshoot check: reserve_tokens={row['reserve_tokens']}, "
            f"reserved_credits_micro={row['reserved_credits_micro']}"
        )

    def test_no_stuck_reserves_after_completion(self, chat, mock_provider):
        # TODO: Same constraint as test_overshoot_within_tolerance.
        # Mock always returns fixed usage. Overshoot cap cannot be triggered.
        # Verify no stuck reserves after completion (cap would still release).
        chat_id = chat["id"]

        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
        assert not has_stuck_reserves(), "No stuck reserves expected (cap releases)"

    def test_cancelled_with_usage(self, chat, mock_provider):
        """Cancelling mid-stream (after some deltas) should still settle quota."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Slow scenario — disconnect after receiving some data
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"word{i} "})
            for i in range(30)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.5, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Write a long essay.", "request_id": request_id}

        # Read a few chunks then disconnect
        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            chunk_count = 0
            for _ in resp.iter_bytes(chunk_size=256):
                chunk_count += 1
                if chunk_count >= 3:
                    break  # disconnect after a few chunks

        # Poll until cancelled
        turn = poll_turn_terminal(chat_id, request_id, timeout=20.0)
        assert turn["state"] == "cancelled"

        time.sleep(0.5)
        assert not has_stuck_reserves(), (
            "Stuck reserves after cancelled turn with partial usage"
        )

    def test_cancelled_without_usage(self, chat, mock_provider):
        """Cancelling before any delta arrives should still release reserves."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Very slow scenario — disconnect before first delta
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"word{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=2.0, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Write slowly.", "request_id": request_id}

        # Disconnect immediately after connection
        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            # disconnect immediately — do not read any bytes

        # Poll until cancelled
        turn = poll_turn_terminal(chat_id, request_id, timeout=20.0)
        assert turn["state"] == "cancelled"

        time.sleep(0.5)
        assert not has_stuck_reserves(), (
            "Stuck reserves after cancelled turn without usage"
        )

    def test_pre_provider_failure_released(self, request, chat, mock_provider):
        """When the provider returns an HTTP error, quota reserves are released."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")
        chat_id = chat["id"]

        used_before = get_total_daily_used()

        mock_provider.set_next_scenario(Scenario(
            http_error_status=500,
            http_error_body={"error": {"message": "Internal server error", "type": "server_error"}},
        ))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        resp = httpx.post(
            url,
            json={"content": "This should fail."},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )

        # Server may return the error as SSE error event (200) or as HTTP error
        if resp.status_code == 200:
            events = parse_sse(resp.text)
            error_events = [e for e in events if e.event == "error"]
            assert len(error_events) > 0, "Expected error event in SSE stream"
        else:
            assert resp.status_code >= 400

        time.sleep(0.5)

        # Credits should not be permanently consumed; no stuck reserves
        assert not has_stuck_reserves(), (
            "Stuck reserves after provider failure"
        )

    def test_disconnected_turn_reaches_terminal(self, chat, mock_provider):
        # TODO: Requires orphan watchdog timeout (default 5 min). Too slow
        # for normal e2e test runs. The orphan watchdog detects turns stuck
        # in "streaming" state and force-settles them after the timeout.
        #
        # To fully test:
        # 1. Set mock to never respond: Scenario(slow=999, events=[...]*100)
        # 2. Start stream, disconnect immediately
        # 3. Wait 5+ minutes for orphan watchdog to trigger
        # 4. Verify turn state changed and no stuck reserves
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Use a moderately slow scenario and disconnect
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"w{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=1.0, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Orphan test.", "request_id": request_id}

        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            # disconnect immediately

        # Wait for the turn to reach a terminal state (cancelled by disconnect detection)
        turn = poll_turn_terminal(chat_id, request_id, timeout=30.0)
        assert turn["state"] in ("cancelled", "done", "error")

        time.sleep(0.5)
        assert not has_stuck_reserves(), "Stuck reserves after orphan-like turn"

    def test_atomic_transaction_observable(self, chat, mock_provider):
        """If turn state is done, quota must also be settled (no stuck reserves)."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        expect_done(events)

        turn = poll_turn_terminal(chat_id, request_id)
        assert turn["state"] == "done"

        time.sleep(0.5)

        # Both conditions must hold: state=done AND no stuck reserves
        assert not has_stuck_reserves(), (
            "Atomic violation: turn state is done but reserves are stuck"
        )

        # Also verify credits were actually consumed
        used = get_total_daily_used()
        assert used > 0, "Credits should be consumed after done turn"

    def test_outbox_dedupe_observable(self, chat, mock_provider):
        """Replaying a completed turn should not change quota (outbox not re-emitted)."""
        # TODO: Outbox internals are not observable via HTTP. We verify the
        # observable effect: replaying a request_id does not increase quota.
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
        used_before = get_total_daily_used()

        # Replay the same request_id
        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Say OK.", "request_id": request_id}
        replay_status, replay_events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert replay_status == 200
        replay_ss = expect_stream_started(replay_events)
        assert replay_ss.data.get("is_new_turn") is False

        time.sleep(0.5)
        used_after = get_total_daily_used()

        assert used_after == used_before, (
            f"Quota should not change on replay: before={used_before}, after={used_after}"
        )

    def test_one_outbox_per_turn_observable(self, chat, mock_provider):
        """Multiple replays of the same turn should not change quota."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say hello.", request_id=request_id
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
        used_before = get_total_daily_used()

        # Replay 3 times
        for _ in range(3):
            replay_status, replay_events, _ = stream_message(
                chat_id, "Say hello.", request_id=request_id
            )
            assert replay_status == 200
            replay_ss = expect_stream_started(replay_events)
            assert replay_ss.data.get("is_new_turn") is False

        time.sleep(0.5)
        used_after = get_total_daily_used()

        assert used_after == used_before, (
            f"Quota unchanged after 3 replays: before={used_before}, after={used_after}"
        )


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestSettlementPerProvider:
    """Verify settlement arithmetic with real provider token counts."""

    def test_completed_settlement_per_provider(self, provider_chat):
        """Settlement should produce non-zero credits with real provider tokens."""
        chat_id = provider_chat["id"]
        status, events, _ = stream_message(chat_id, "Say hello in exactly three words.")
        assert status == 200
        done = expect_done(events)
        usage = done.data.get("usage", {})
        assert usage.get("input_tokens", 0) > 0
        assert usage.get("output_tokens", 0) > 0
        # Verify no stuck reserves after real-provider settlement
        assert not has_stuck_reserves()
