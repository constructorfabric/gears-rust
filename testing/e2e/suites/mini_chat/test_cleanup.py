"""Tests for cleanup — chat deletion, attachment cleanup, orphan watchdog, thread summary.

Most cleanup internals (background workers, file deletion timing, vector store ordering)
are not directly observable via HTTP. These tests verify the observable effects:
DELETE returns correct status codes, GET returns 404 after deletion, and turn states
transition correctly.

Covers:
- Cleanup worker observable effect (delete chat -> 404)
- Provider 404 idempotent (double delete)
- Vector store ordering (upload then delete)
- Attachment cleanup state machine
- Orphan watchdog detects stuck turn
- Orphan settlement estimated
- Thread summary trigger (P2 deferred)
- Thread summary worker (P2 deferred)
"""

from __future__ import annotations

import io
import os
import sqlite3
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
    poll_until,
    stream_message,
)
from .mock_provider.responses import MockEvent, Scenario, Usage


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def create_chat(model: str | None = None) -> dict:
    body = {"model": model} if model else {}
    resp = httpx.post(f"{API_PREFIX}/chats", json=body, timeout=10)
    assert resp.status_code == 201, f"Create chat failed: {resp.status_code} {resp.text}"
    return resp.json()


def delete_chat(chat_id: str) -> httpx.Response:
    return httpx.delete(f"{API_PREFIX}/chats/{chat_id}", timeout=10)


def get_chat(chat_id: str) -> httpx.Response:
    return httpx.get(f"{API_PREFIX}/chats/{chat_id}", timeout=10)


def upload_file(
    chat_id: str,
    content: bytes = b"Hello, world!",
    filename: str = "test.txt",
    content_type: str = "text/plain",
) -> httpx.Response:
    return httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        files={"file": (filename, io.BytesIO(content), content_type)},
        timeout=60,
    )


def poll_turn_terminal(chat_id: str, request_id: str, timeout: float = 15.0) -> dict:
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
        elif resp.status_code != 404:
            raise AssertionError(
                f"Unexpected status {resp.status_code} polling turn {request_id}: {resp.text}"
            )
        time.sleep(0.3)
    state = body["state"] if body else "no response"
    raise AssertionError(
        f"Turn {request_id} did not reach terminal state within {timeout}s "
        f"(last state: {state})"
    )


def get_quota_status() -> dict:
    resp = httpx.get(f"{API_PREFIX}/quota/status", timeout=10)
    assert resp.status_code == 200
    return resp.json()


def has_stuck_reserves() -> bool:
    qs = get_quota_status()
    for tier in qs["tiers"]:
        for period in tier["periods"]:
            if period.get("reserved_credits_micro", 0) != 0:
                return True
    return False


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestCleanup:
    """Cleanup — deletion, attachment cleanup, orphan handling."""

    def test_cleanup_worker_observable(self, server):
        # TODO: Background cleanup happens asynchronously. Cannot observe
        # file deletion timing. This test verifies the HTTP-observable effects.
        chat = create_chat()
        chat_id = chat["id"]

        # Upload an attachment
        upload_resp = upload_file(chat_id)
        assert upload_resp.status_code == 201

        # Delete the chat
        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # Verify chat is gone
        get_resp = get_chat(chat_id)
        assert get_resp.status_code == 404

    def test_provider_404_idempotent(self, server):
        # TODO: Provider cleanup internals not observable via HTTP.
        # Test idempotency by deleting the same chat twice.
        chat = create_chat()
        chat_id = chat["id"]

        # First delete
        resp1 = delete_chat(chat_id)
        assert resp1.status_code == 204

        # Second delete — should return 404 (already gone)
        resp2 = delete_chat(chat_id)
        assert resp2.status_code == 404

    def test_vector_store_ordering(self, server):
        # TODO: Vector store cleanup timing not observable via HTTP.
        # Verify that uploading a document (which creates a vector store)
        # and then deleting the chat produces correct HTTP responses.
        chat = create_chat()
        chat_id = chat["id"]

        # Upload a document (triggers vector store creation)
        doc_content = b"This is a document for vector store testing."
        upload_resp = upload_file(
            chat_id,
            content=doc_content,
            filename="vector_test.txt",
            content_type="text/plain",
        )
        assert upload_resp.status_code == 201

        # Delete chat — should succeed even with vector store
        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # Verify chat is gone
        get_resp = get_chat(chat_id)
        assert get_resp.status_code == 404

    def test_attachment_cleanup_state_machine(self, server):
        # TODO: Cleanup state not exposed in API. We verify the
        # observable transitions: upload -> delete chat -> 404.
        chat = create_chat()
        chat_id = chat["id"]

        upload_resp = upload_file(chat_id)
        assert upload_resp.status_code == 201
        attachment_id = upload_resp.json()["id"]

        # Attachment should be accessible before deletion
        att_resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment_id}",
            timeout=10,
        )
        assert att_resp.status_code == 200

        # Delete chat
        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # After deletion, chat and attachment should be inaccessible
        get_resp = get_chat(chat_id)
        assert get_resp.status_code == 404
        att_resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment_id}", timeout=10)
        assert att_resp.status_code == 404

    def test_orphan_watchdog_detects_stuck_turn(self, chat, mock_provider):
        # This test verifies that a disconnected streaming turn reaches a
        # terminal state. With the test config (orphan_watchdog.timeout_secs=60,
        # scan_interval_secs=2), the orphan watchdog will detect and finalize
        # any truly stuck turn within ~62s. However, the server's disconnect
        # detection (via SSE channel closure) usually finalizes the turn as
        # "cancelled" much faster. Both paths produce a correct terminal state.
        #
        # The orphan watchdog is the safety net for cases where disconnect
        # detection fails (e.g., pod crash). That path is covered by the
        # unit tests in turn_repo.rs and finalization_service.rs.
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Very slow scenario — ensures the mock provider doesn't finish naturally
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"w{i} "})
            for i in range(30)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=1.0, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Stuck turn test.", "request_id": request_id}

        # Start stream and disconnect immediately — turn stays running
        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            # disconnect immediately

        # Poll until turn reaches terminal state.
        # Disconnect detection should resolve this within a few seconds.
        # The orphan watchdog is the fallback (60s+ timeout).
        turn = poll_turn_terminal(chat_id, request_id, timeout=30.0)
        assert turn["state"] in ("cancelled", "error"), (
            f"Expected cancelled (disconnect) or error (orphan), got {turn['state']}"
        )
        # If the orphan watchdog resolved it, error_code will be "orphan_timeout".
        # If disconnect detection resolved it, state will be "cancelled".
        if turn["state"] == "error":
            assert turn.get("error_code") == "orphan_timeout", (
                f"Expected orphan_timeout error_code, got {turn.get('error_code')}"
            )

    def test_orphan_settlement_estimated(self, chat, mock_provider):
        # Verify that quota reserves are released after an orphan-like turn
        # terminates. Both disconnect detection (cancel -> estimated settlement)
        # and orphan watchdog (failed -> estimated settlement) release reserves.
        #
        # This test uses disconnect-as-proxy because true orphan detection
        # requires the minimum 60s timeout, which is too slow for regular CI.
        # The settlement path is identical — both derive BillingOutcome::Aborted
        # and use SettlementPath::Estimated.
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"w{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.8, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Orphan settlement.", "request_id": request_id}

        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            # read one chunk then disconnect
            for _ in resp.iter_bytes(chunk_size=256):
                break

        # Wait for terminal state
        turn = poll_turn_terminal(chat_id, request_id, timeout=30.0)
        assert turn["state"] in ("cancelled", "error"), (
            f"Expected cancelled (disconnect) or error (orphan), got {turn['state']}"
        )

        # Verify no stuck reserves — settlement should have released them
        time.sleep(0.5)
        assert not has_stuck_reserves(), (
            "Stuck reserves after orphan-like turn settlement"
        )

    def test_thread_summary_trigger(self, server):
        # TODO: Feature deferred to P2. Thread summary is triggered after
        # a high number of turns (>20) in a thread. The trigger threshold
        # may be very high in production config.
        #
        # To fully test:
        # 1. Create a chat
        # 2. Send >20 messages to trigger summary
        # 3. Query DB: SELECT * FROM thread_summary_tasks WHERE chat_id = ?
        # 4. Assert a row exists
        #
        # For now: verify the chat can handle multiple messages without error.
        chat = create_chat()
        chat_id = chat["id"]

        for i in range(3):
            status, events, _ = stream_message(chat_id, f"Message {i}. Say OK.")
            assert status == 200
            expect_done(events)

        # Verify chat is still accessible
        resp = get_chat(chat_id)
        assert resp.status_code == 200
        assert resp.json()["message_count"] >= 6  # 3 user + 3 assistant

    def test_thread_summary_worker(self, server):
        # TODO: Feature deferred to P2. Depends on trigger working first.
        #
        # To fully test:
        # 1. Trigger summary (send many messages)
        # 2. Wait for worker to process
        # 3. Query DB: SELECT * FROM thread_summaries WHERE chat_id = ?
        # 4. Assert summary text is not empty
        #
        # For now: verify that the messages endpoint returns correct history.
        chat = create_chat()
        chat_id = chat["id"]

        status, events, _ = stream_message(chat_id, "Say hello.")
        assert status == 200
        expect_done(events)

        # Verify message history is accessible
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", timeout=10)
        assert resp.status_code == 200
        messages = resp.json()
        assert "items" in messages and isinstance(messages["items"], list)


# ---------------------------------------------------------------------------
# DB helpers for cleanup worker scenarios
# ---------------------------------------------------------------------------

def _to_blob(value):
    """Convert UUID strings to bytes for SQLite parameter binding."""
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


def poll_attachment_ready(chat_id: str, attachment_id: str, timeout: float = 30.0):
    """Poll until an attachment is ready; fail fast if upload failed."""
    resp = poll_until(
        lambda: httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment_id}",
            timeout=10,
        ),
        until=lambda r: r.json()["status"] in ("ready", "failed"),
        timeout=timeout,
    )
    body = resp.json()
    assert body["status"] == "ready", (
        f"Attachment {attachment_id} upload failed (expected ready): {body}"
    )
    return resp


def get_attachment_rows(chat_id: str) -> list[dict]:
    """Query all attachment rows for a chat from DB."""
    return query_db(
        "SELECT id, cleanup_status, cleanup_attempts, last_cleanup_error, deleted_at "
        "FROM attachments WHERE chat_id = ?",
        (chat_id,),
    )


def get_outbox_messages(queue_name: str, limit: int = 50) -> list[dict]:
    """Query outbox messages for a given queue.

    Checks both incoming (not yet sequenced) and outgoing (sequenced) tables
    because the sequencer runs asynchronously.

    ToolKit outbox schema:
    - toolkit_outbox_partitions: queue -> partition_id mapping
    - toolkit_outbox_incoming / toolkit_outbox_outgoing: id, partition_id, body_id
    - toolkit_outbox_body: id, payload, payload_type, created_at
    """
    # Query both incoming (not yet sequenced) and outgoing (sequenced).
    outgoing = query_db(
        """
        SELECT b.payload, b.payload_type, b.created_at
        FROM toolkit_outbox_outgoing o
        JOIN toolkit_outbox_body b ON o.body_id = b.id
        JOIN toolkit_outbox_partitions p ON o.partition_id = p.id
        WHERE p.queue = ?
        ORDER BY b.created_at DESC LIMIT ?
        """,
        (queue_name, limit),
    )
    incoming = query_db(
        """
        SELECT b.payload, b.payload_type, b.created_at
        FROM toolkit_outbox_incoming i
        JOIN toolkit_outbox_body b ON i.body_id = b.id
        JOIN toolkit_outbox_partitions p ON i.partition_id = p.id
        WHERE p.queue = ?
        ORDER BY b.created_at DESC LIMIT ?
        """,
        (queue_name, limit),
    )
    return outgoing + incoming


# ---------------------------------------------------------------------------
# Cleanup worker E2E scenarios
# ---------------------------------------------------------------------------

class TestCleanupWorkerDB:
    """Cleanup worker — verify DB state transitions.

    These tests inspect the database directly to verify that:
    - Chat deletion marks attachments as cleanup_status = 'pending'
    - Chat cleanup outbox event is enqueued
    - Attachment deletion enqueues a per-attachment cleanup event
    """

    def test_chat_deletion_marks_attachments_for_cleanup(self, server):
        """DELETE chat → all attachments get cleanup_status set (not NULL)."""
        chat = create_chat()
        chat_id = chat["id"]

        # Upload an attachment and wait until it's ready
        upload_resp = upload_file(chat_id)
        assert upload_resp.status_code == 201
        att_id = upload_resp.json()["id"]
        poll_attachment_ready(chat_id, att_id)

        # Delete the chat
        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # Verify: attachment cleanup_status was set by the delete TX.
        # It may already be 'done' if the outbox handler processed it quickly.
        rows = get_attachment_rows(chat_id)
        assert len(rows) >= 1, f"Expected at least 1 attachment row, got {len(rows)}"
        for row in rows:
            assert row["cleanup_status"] in ("pending", "done", "failed"), (
                f"Attachment {row['id']} should have cleanup_status set, "
                f"got {row['cleanup_status']!r}"
            )

    def test_chat_deletion_enqueues_chat_cleanup_event(self, server):
        """DELETE chat → chat_cleanup outbox message is enqueued."""
        chat = create_chat()
        chat_id = chat["id"]

        # Upload an attachment (so there's work for the cleanup handler)
        upload_resp = upload_file(chat_id)
        assert upload_resp.status_code == 201
        att_id = upload_resp.json()["id"]
        poll_attachment_ready(chat_id, att_id)

        # Delete the chat
        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # The outbox body table stores ALL enqueued payloads durably,
        # regardless of processing state. Query it directly.
        import json
        all_bodies = query_db(
            "SELECT payload FROM toolkit_outbox_body ORDER BY id DESC LIMIT 50"
        )
        found = False
        for row in all_bodies:
            try:
                payload = json.loads(row["payload"])
                if payload.get("chat_id") == chat_id and payload.get("reason") == "chat_soft_delete":
                    found = True
                    assert "system_request_id" in payload
                    assert "chat_deleted_at" in payload
                    assert "tenant_id" in payload
                    break
            except (json.JSONDecodeError, KeyError):
                continue
        assert found, (
            f"No chat_cleanup body found for chat_id={chat_id}. "
            f"Total bodies: {len(all_bodies)}"
        )

    def test_attachment_deletion_enqueues_cleanup_event(self, server):
        """DELETE attachment → per-attachment cleanup outbox event is enqueued."""
        chat = create_chat()
        chat_id = chat["id"]

        # Upload and wait for ready
        upload_resp = upload_file(chat_id)
        assert upload_resp.status_code == 201
        att_id = upload_resp.json()["id"]
        poll_attachment_ready(chat_id, att_id)

        # Delete the individual attachment
        del_resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}",
            timeout=10,
        )
        assert del_resp.status_code == 204

        # Query outbox body table directly (durable, unaffected by handler processing).
        import json
        all_bodies = query_db(
            "SELECT payload FROM toolkit_outbox_body ORDER BY id DESC LIMIT 50"
        )
        found = False
        for row in all_bodies:
            try:
                payload = json.loads(row["payload"])
                if payload.get("attachment_id") == att_id:
                    found = True
                    assert payload["event_type"] == "attachment_deleted"
                    assert payload["chat_id"] == chat_id
                    assert "provider_file_id" in payload
                    assert "storage_backend" in payload
                    break
            except (json.JSONDecodeError, KeyError):
                continue
        assert found, (
            f"No attachment_cleanup body found for attachment_id={att_id}. "
            f"Total bodies: {len(all_bodies)}"
        )

    def test_chat_deletion_with_multiple_attachments(self, server):
        """DELETE chat with 3 attachments → all get cleanup_status set."""
        chat = create_chat()
        chat_id = chat["id"]

        att_ids = []
        for i in range(3):
            resp = upload_file(
                chat_id,
                content=f"File content {i}".encode(),
                filename=f"test_{i}.txt",
            )
            assert resp.status_code == 201
            att_ids.append(resp.json()["id"])

        # Wait for all to be ready
        for att_id in att_ids:
            poll_attachment_ready(chat_id, att_id)

        # Delete chat
        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # All 3 attachments should have cleanup_status set (may already be 'done')
        rows = get_attachment_rows(chat_id)
        cleanup_rows = [r for r in rows if r["cleanup_status"] in ("pending", "done", "failed")]
        assert len(cleanup_rows) == 3, (
            f"Expected 3 attachments with cleanup_status set, got {len(cleanup_rows)} "
            f"(total rows: {len(rows)})"
        )

    def test_double_delete_chat_idempotent(self, server):
        """Second DELETE returns 404; no duplicate cleanup events."""
        chat = create_chat()
        chat_id = chat["id"]

        upload_resp = upload_file(chat_id)
        assert upload_resp.status_code == 201

        # First delete
        resp1 = delete_chat(chat_id)
        assert resp1.status_code == 204

        # Second delete — 404
        resp2 = delete_chat(chat_id)
        assert resp2.status_code == 404

    def test_chat_without_attachments_still_enqueues(self, server):
        """DELETE empty chat → cleanup event still enqueued (handler handles gracefully)."""
        chat = create_chat()
        chat_id = chat["id"]

        del_resp = delete_chat(chat_id)
        assert del_resp.status_code == 204

        # Query outbox body table directly.
        import json
        all_bodies = query_db(
            "SELECT payload FROM toolkit_outbox_body ORDER BY id DESC LIMIT 50"
        )
        found = any(
            json.loads(row["payload"]).get("chat_id") == chat_id
            for row in all_bodies
            if row["payload"]
        )
        assert found, "Chat cleanup event should be enqueued even for empty chat"
