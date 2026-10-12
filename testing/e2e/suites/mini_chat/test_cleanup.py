"""Tests for cleanup — chat deletion, attachment cleanup outbox events.

Background cleanup is not visible over HTTP. These tests check the effects:
DELETE status codes, 404 after deletion, attachment `cleanup_status` and the
enqueued cleanup outbox messages (conftest "Outbox capture").

Orphan-watchdog finalization is covered by unit tests (turn_repo.rs,
finalization_service.rs): the minimum watchdog timeout (90 s) does not fit
the E2E time budget.
"""

from __future__ import annotations

import io
import json
import time
import uuid

import httpx
import pytest

from .mock_provider.server import FILES_PATH, VECTOR_STORES_PATH
from .conftest import (
    API_PREFIX,
    ATTACHMENT_CLEANUP_QUEUE,
    DEFAULT_MODEL,
    STANDARD_MODEL,
    USER_A_ID,
    assert_no_reserves,
    assert_problem,
    chat_cleanup_payloads,
    expect_done,
    find_period,
    get_quota_status,
    open_stream,
    outbox_payloads,
    provider_file_id,
    provider_usage,
    query_db,
    slow_scenario,
    wait_cleanup_terminal,
)


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


def upload_ready(chat_id: str, content: bytes = b"Hello, world!", filename: str = "test.txt") -> str:
    """Upload a document; the synchronous upload answers 201 `ready`. Return its id."""
    resp = upload_file(chat_id, content, filename)
    assert resp.status_code == 201, resp.text
    assert resp.json()["status"] == "ready", resp.json()
    return resp.json()["id"]


# ---------------------------------------------------------------------------
# Provider-side cleanup (mock provider, offline only)
# ---------------------------------------------------------------------------

def _wait_for(predicate, what: str, timeout: float = 20.0, interval: float = 0.2):
    """Poll `predicate()` until it returns a truthy value; return that value."""
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise AssertionError(f"timed out after {timeout}s waiting for {what}")
        time.sleep(interval)


def _cleanup_status(attachment_id: str) -> str | None:
    rows = query_db("SELECT cleanup_status FROM attachments WHERE id = ?", (attachment_id,))
    assert len(rows) == 1, rows
    return rows[0]["cleanup_status"]


def _file_deletes(mock_provider, file_id: str) -> list[str]:
    """Paths of DELETE /files/{file_id} requests (not vector-store file removals).

    Filtered by file id: the cleanup of an earlier test's chat or attachment
    may still be running."""
    return [
        p for m, p in mock_provider.get_request_paths()
        if m == "DELETE" and f"/files/{file_id}" in p and "/vector_stores/" not in p
    ]


def _vector_store_rows(chat_id: str) -> list[dict]:
    return query_db(
        "SELECT vector_store_id FROM chat_vector_stores WHERE chat_id = ?", (chat_id,),
    )


def _chat_with_ready_docs(model: str, n: int) -> tuple[str, list[str]]:
    chat_id = create_chat(model)["id"]
    att_ids = [upload_ready(chat_id, f"doc {i}".encode(), f"doc{i}.txt") for i in range(n)]
    return chat_id, att_ids


def check_chat_cleanup_404_is_success(mock_provider, model: str) -> None:
    chat_id, (att_id,) = _chat_with_ready_docs(model, 1)
    file_id = provider_file_id(att_id)

    mock_provider.set_fault("DELETE", rf"{FILES_PATH}/{file_id}", 404)
    assert delete_chat(chat_id).status_code == 204

    assert wait_cleanup_terminal([att_id]) == {att_id: "done"}
    row = query_db("SELECT cleanup_attempts FROM attachments WHERE id = ?", (att_id,))[0]
    assert row["cleanup_attempts"] == 0, row
    # The one delete that was made got the 404.
    assert len(_file_deletes(mock_provider, file_id)) == 1, mock_provider.get_request_paths()


def check_attachment_cleanup_404_is_success(mock_provider, model: str) -> None:
    chat_id, (att_id,) = _chat_with_ready_docs(model, 1)
    file_id = provider_file_id(att_id)

    mock_provider.set_fault("DELETE", rf"{FILES_PATH}/{file_id}", 404)
    resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
    assert resp.status_code == 204

    _wait_for(lambda: _file_deletes(mock_provider, file_id), "the provider file delete", timeout=10.0)
    # The handler marks the row right after the provider answered.
    assert wait_cleanup_terminal([att_id], timeout=5.0) == {att_id: "done"}
    assert len(_file_deletes(mock_provider, file_id)) == 1, mock_provider.get_request_paths()


def check_vector_store_deleted_after_files(mock_provider, model: str) -> None:
    chat_id, att_ids = _chat_with_ready_docs(model, 2)
    file_ids = [provider_file_id(a) for a in att_ids]
    vs_rows = _vector_store_rows(chat_id)
    assert len(vs_rows) == 1 and vs_rows[0]["vector_store_id"], vs_rows
    vs_id = vs_rows[0]["vector_store_id"]

    mock_provider.clear_captured_requests()
    assert delete_chat(chat_id).status_code == 204
    assert set(wait_cleanup_terminal(att_ids).values()) == {"done"}
    _wait_for(lambda: not _vector_store_rows(chat_id), "chat_vector_stores row removal")

    # Only the requests for this chat's files and vector store: the cleanup of
    # an earlier test's chat may still be running.
    paths = [(m, p.split("?")[0]) for m, p in mock_provider.get_request_paths()]
    file_deletes = {
        fid: [
            i for i, (m, p) in enumerate(paths)
            if m == "DELETE" and f"/files/{fid}" in p and "/vector_stores/" not in p
        ]
        for fid in file_ids
    }
    vs_deletes = [
        i for i, (m, p) in enumerate(paths)
        if m == "DELETE" and p.rstrip("/").endswith(f"/vector_stores/{vs_id}")
    ]
    assert [len(v) for v in file_deletes.values()] == [1, 1], paths
    assert len(vs_deletes) == 1, paths
    assert max(i for v in file_deletes.values() for i in v) < vs_deletes[0], paths


# Default `cleanup_worker.max_attempts` (base.yaml does not override it).
CLEANUP_MAX_ATTEMPTS = 5


def _dead_letters(chat_id: str) -> list[dict]:
    """Dead-lettered outbox messages whose payload mentions `chat_id`."""
    return query_db(
        "SELECT partition_id, seq, payload, last_error FROM toolkit_outbox_dead_letters "
        # CAST: see outbox_payloads in conftest.py (LIKE never matches a
        # BLOB on SQLite built with SQLITE_LIKE_DOESNT_MATCH_BLOBS).
        "WHERE CAST(payload AS TEXT) LIKE ?",
        (f"%{chat_id}%",),
    )


def _processed_seq(partition_id: int) -> int:
    """The outbox processor's committed offset of a partition: messages with
    `seq` up to it are never delivered again."""
    (row,) = query_db(
        "SELECT processed_seq FROM toolkit_outbox_processor WHERE partition_id = ?",
        (partition_id,),
    )
    return row["processed_seq"]


def check_attachment_cleanup_403_ends_failed(mock_provider, model: str) -> None:
    chat_id, (att_id,) = _chat_with_ready_docs(model, 1)
    file_id = provider_file_id(att_id)

    # More faults than attempts: every delete of this file answers 403.
    mock_provider.set_fault("DELETE", rf"{FILES_PATH}/{file_id}", 403, count=50)
    resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
    assert resp.status_code == 204

    assert wait_cleanup_terminal([att_id], timeout=30.0) == {att_id: "failed"}
    row = query_db(
        "SELECT cleanup_attempts, last_cleanup_error FROM attachments WHERE id = ?", (att_id,),
    )[0]
    assert row["cleanup_attempts"] == CLEANUP_MAX_ATTEMPTS, row
    assert "403" in row["last_cleanup_error"], row
    assert len(_file_deletes(mock_provider, file_id)) == CLEANUP_MAX_ATTEMPTS, (
        mock_provider.get_request_paths()
    )


def _vs_deletes(mock_provider, vs_id: str) -> int:
    return sum(
        1 for m, p in mock_provider.get_request_paths()
        if m == "DELETE" and p.split("?")[0].rstrip("/").endswith(f"/vector_stores/{vs_id}")
    )


def check_vector_store_delete_500_is_dead_lettered(mock_provider, model: str) -> None:
    chat_id, (att_id,) = _chat_with_ready_docs(model, 1)
    vs_id = _vector_store_rows(chat_id)[0]["vector_store_id"]
    assert vs_id

    mock_provider.set_fault("DELETE", rf"{VECTOR_STORES_PATH}/{vs_id}", 500, count=50)
    assert delete_chat(chat_id).status_code == 204

    assert wait_cleanup_terminal([att_id]) == {att_id: "done"}
    dead = _wait_for(lambda: _dead_letters(chat_id), "the chat cleanup dead letter", timeout=30.0)
    assert len(dead) == 1, dead
    assert json.loads(dead[0]["payload"])["reason"] == "chat_soft_delete", dead
    assert "max attempts (5)" in dead[0]["last_error"], dead
    assert _vs_deletes(mock_provider, vs_id) == CLEANUP_MAX_ATTEMPTS, (
        mock_provider.get_request_paths()
    )
    # The row stays for a dead-letter replay.
    assert len(_vector_store_rows(chat_id)) == 1
    # Not retried again: the processor delivers only messages past its
    # partition offset, and it moves the offset past a rejected message in
    # the transaction that writes the dead letter (toolkit-db outbox,
    # strategy.rs `HandlerResult::Reject`). With the offset past the message
    # no later delivery can exist, so the delete count is final; no wait on
    # a retry backoff is needed.
    assert _processed_seq(dead[0]["partition_id"]) >= dead[0]["seq"], dead
    assert _vs_deletes(mock_provider, vs_id) == CLEANUP_MAX_ATTEMPTS


# Provider-side cleanup for both storage backends (openai, azure).

@pytest.mark.usefixtures("offline_only")
class TestProviderCleanupOpenAI:
    """Provider-side cleanup of an OpenAI-backed chat (storage_backend = provider id)."""

    @pytest.mark.timeout(40)
    def test_chat_cleanup_provider_404_is_success(self, mock_provider):
        """19-03: the provider answers 404 to the file delete of a deleted chat:
        the attachment cleanup ends in `done`, not `failed` or retrying."""
        check_chat_cleanup_404_is_success(mock_provider, STANDARD_MODEL)

    @pytest.mark.timeout(40)
    def test_vector_store_deleted_after_files(self, mock_provider):
        """19-04: chat cleanup deletes both provider files of the chat before
        its vector store, then removes the chat_vector_stores row."""
        check_vector_store_deleted_after_files(mock_provider, STANDARD_MODEL)

    @pytest.mark.timeout(40)
    def test_attachment_cleanup_provider_404_is_success(self, mock_provider):
        """19-03: the provider answers 404 to the file delete of a deleted
        attachment: the attachment cleanup ends in `done`."""
        check_attachment_cleanup_404_is_success(mock_provider, STANDARD_MODEL)

    @pytest.mark.timeout(60)
    def test_attachment_cleanup_provider_403_ends_failed(self, mock_provider):
        """19-17: every file delete of a deleted attachment answers 403:
        the cleanup is retried `max_attempts` times and ends in `failed`."""
        check_attachment_cleanup_403_ends_failed(mock_provider, STANDARD_MODEL)

    @pytest.mark.timeout(60)
    def test_vector_store_delete_500_is_dead_lettered(self, mock_provider):
        """19-18: every vector store delete of a deleted chat answers 500:
        after `max_attempts` deliveries the chat cleanup message is
        dead-lettered and not retried again; the chat_vector_stores row stays."""
        check_vector_store_delete_500_is_dead_lettered(mock_provider, STANDARD_MODEL)


@pytest.mark.usefixtures("offline_only")
class TestProviderCleanupAzure:
    """The same scenarios for an Azure-backed chat (storage_backend "azure")."""

    @pytest.mark.timeout(40)
    def test_chat_cleanup_provider_404_is_success(self, mock_provider):
        """19-03 (azure): see TestProviderCleanupOpenAI."""
        check_chat_cleanup_404_is_success(mock_provider, DEFAULT_MODEL)

    @pytest.mark.timeout(40)
    def test_vector_store_deleted_after_files(self, mock_provider):
        """19-04 (azure): see TestProviderCleanupOpenAI."""
        check_vector_store_deleted_after_files(mock_provider, DEFAULT_MODEL)

    @pytest.mark.timeout(40)
    def test_attachment_cleanup_provider_404_is_success(self, mock_provider):
        """19-03 (azure): see TestProviderCleanupOpenAI."""
        check_attachment_cleanup_404_is_success(mock_provider, DEFAULT_MODEL)

    @pytest.mark.timeout(60)
    def test_attachment_cleanup_provider_403_ends_failed(self, mock_provider):
        """19-17 (azure): every file delete of a deleted attachment answers 403:
        the cleanup is retried `max_attempts` times and ends in `failed`."""
        check_attachment_cleanup_403_ends_failed(mock_provider, DEFAULT_MODEL)

    @pytest.mark.timeout(60)
    def test_vector_store_delete_500_is_dead_lettered(self, mock_provider):
        """19-18 (azure): every vector store delete of a deleted chat answers 500:
        after `max_attempts` deliveries the chat cleanup message is
        dead-lettered and not retried again; the chat_vector_stores row stays."""
        check_vector_store_delete_500_is_dead_lettered(mock_provider, DEFAULT_MODEL)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

@pytest.mark.usefixtures("offline_only")
class TestCleanup:
    """Chat deletion — observable effects."""

    @pytest.mark.timeout(30)
    @pytest.mark.usefixtures("same_utc_day")
    def test_running_turn_completes_and_is_billed_after_chat_delete(self, mock_provider):
        """19-16 (DESIGN, chat deletion): a turn running when its chat is
        deleted is not cancelled: the stream ends with `done`, the turn is
        completed and its usage is charged."""
        chat_id = create_chat()["id"]  # azure-gpt-4.1
        rid = str(uuid.uuid4())
        used_before = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))
        mock_provider.clear_captured_requests()

        with open_stream(chat_id, "Keep answering.", request_id=rid) as s:
            s.read_until_started()
            assert delete_chat(chat_id).status_code == 204
            usage = provider_usage(mock_provider, expect_done(s.drain()).data["usage"])

        assert_no_reserves(USER_A_ID)
        rows = query_db("SELECT state FROM chat_turns WHERE request_id = ?", (rid,))
        assert rows == [{"state": "completed"}], rows
        # azure-gpt-4.1 multipliers (base.yaml): 3 and 15 credits_micro per token.
        cost = usage["input_tokens"] * 3 + usage["output_tokens"] * 15
        used_after = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]
        assert used_after - used_before == cost

    def test_deleted_chat_hides_chat_and_attachment(self, server):
        """After DELETE chat, the chat, its attachment and its messages return 404."""
        chat_id = create_chat()["id"]
        attachment_id = upload_ready(chat_id)
        att_url = f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment_id}"
        assert httpx.get(att_url, timeout=10).status_code == 200

        assert delete_chat(chat_id).status_code == 204

        assert_problem(get_chat(chat_id), 404, "not_found")
        assert_problem(httpx.get(att_url, timeout=10), 404, "not_found")
        assert_problem(
            httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", timeout=10), 404, "not_found",
        )


# ---------------------------------------------------------------------------
# Cleanup worker E2E scenarios
# ---------------------------------------------------------------------------

def get_attachment_rows(chat_id: str) -> list[dict]:
    """Query all attachment rows for a chat from DB."""
    return query_db(
        "SELECT id, cleanup_status, cleanup_attempts, last_cleanup_error, deleted_at "
        "FROM attachments WHERE chat_id = ?",
        (chat_id,),
    )


def _assert_file_deleted_once(request, mock_provider, file_id: str) -> None:
    """Offline: the cleanup sent exactly one DELETE for `file_id` and the
    mock no longer holds the file (the DELETE found it, not a 404)."""
    if request.config.getoption("mode") == "online":
        return
    deletes = _file_deletes(mock_provider, file_id)
    assert len(deletes) == 1, mock_provider.get_request_paths()
    assert deletes[0].split("?")[0].endswith(f"/files/{file_id}"), deletes
    assert file_id not in [f["id"] for f in mock_provider.get_uploaded_files()]


class TestCleanupWorkerDB:
    """Cleanup worker — DB state and outbox payloads.

    - Chat deletion ends the cleanup of each attachment in `done`
    - Chat deletion enqueues one chat cleanup event (also for an empty chat)
    - Attachment deletion enqueues a per-attachment cleanup event

    Every test waits for the cleanup it starts: a cleanup left running would
    send its DELETEs after the next test cleared the mock's files, get 404
    and still end in `done`.
    """

    @pytest.mark.timeout(40)
    def test_chat_deletion_marks_attachments_for_cleanup(self, request, server, mock_provider):
        """DELETE chat → the attachment's cleanup ends in `done` after one
        successful provider delete of its own file (cleanup_attempts stays 0)."""
        chat_id = create_chat()["id"]
        att_id = upload_ready(chat_id)
        file_id = provider_file_id(att_id)
        assert _cleanup_status(att_id) is None

        assert delete_chat(chat_id).status_code == 204

        assert wait_cleanup_terminal([att_id]) == {att_id: "done"}
        rows = get_attachment_rows(chat_id)
        assert [(r["cleanup_status"], r["cleanup_attempts"]) for r in rows] == [("done", 0)], rows
        _assert_file_deleted_once(request, mock_provider, file_id)

    def test_chat_deletion_enqueues_chat_cleanup_event(self, server):
        """DELETE chat → one chat cleanup outbox payload with the chat's
        tenant, deletion time and a system request id."""
        chat_id = create_chat()["id"]
        att_id = upload_ready(chat_id)  # work for the cleanup handler

        assert delete_chat(chat_id).status_code == 204

        payloads = chat_cleanup_payloads(chat_id)
        assert len(payloads) == 1, payloads
        for key in ("system_request_id", "chat_deleted_at", "tenant_id"):
            assert key in payloads[0], payloads[0]
        assert wait_cleanup_terminal([att_id]) == {att_id: "done"}

    def test_attachment_deletion_enqueues_cleanup_event(self, request, server, mock_provider):
        """DELETE attachment → one per-attachment cleanup outbox payload with
        the provider file id; the cleanup deletes that file."""
        chat_id = create_chat()["id"]
        att_id = upload_ready(chat_id)
        file_id = provider_file_id(att_id)

        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 204

        payloads = [
            p for p in outbox_payloads(att_id, ATTACHMENT_CLEANUP_QUEUE)
            if p.get("attachment_id") == att_id
        ]
        assert len(payloads) == 1, payloads
        payload = payloads[0]
        assert payload["event_type"] == "attachment_deleted"
        assert payload["chat_id"] == chat_id
        assert payload["provider_file_id"] == file_id, payload
        assert "storage_backend" in payload
        assert wait_cleanup_terminal([att_id]) == {att_id: "done"}
        _assert_file_deleted_once(request, mock_provider, file_id)

    @pytest.mark.timeout(40)
    def test_chat_deletion_with_multiple_attachments(self, request, server, mock_provider):
        """DELETE chat with 3 attachments → the cleanup of each one ends in
        `done`, each after one delete of its own provider file."""
        chat_id = create_chat()["id"]
        att_ids = [
            upload_ready(chat_id, f"File content {i}".encode(), f"test_{i}.txt")
            for i in range(3)
        ]
        file_ids = [provider_file_id(a) for a in att_ids]

        assert delete_chat(chat_id).status_code == 204

        assert wait_cleanup_terminal(att_ids) == {a: "done" for a in att_ids}
        for file_id in file_ids:
            _assert_file_deleted_once(request, mock_provider, file_id)

    def test_second_delete_chat_404_single_cleanup_event(self, server):
        """A second DELETE of a chat is 404 and enqueues no second cleanup event."""
        chat_id = create_chat()["id"]
        att_id = upload_ready(chat_id)

        assert delete_chat(chat_id).status_code == 204
        assert_problem(delete_chat(chat_id), 404, "not_found")
        assert len(chat_cleanup_payloads(chat_id)) == 1
        assert wait_cleanup_terminal([att_id]) == {att_id: "done"}

    def test_chat_without_attachments_still_enqueues(self, server):
        """DELETE of an empty chat still enqueues one chat cleanup event
        (reason `chat_soft_delete`)."""
        chat_id = create_chat()["id"]

        assert delete_chat(chat_id).status_code == 204

        payloads = chat_cleanup_payloads(chat_id)
        assert [p["reason"] for p in payloads] == ["chat_soft_delete"], payloads
