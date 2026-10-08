"""Thread summary: trigger at finalization, summary worker, use in context assembly.

The chat uses the small-context catalog model (config/base.yaml,
`gpt-4.1-mini-tiny-ctx`: context_window 4096, max_output_tokens 1024,
max_input_tokens 3000, fixed_overhead_tokens 500). The trigger fires when
the estimated context reaches compression_threshold_pct (60, base.yaml) of
min(max_input_tokens, context_window - max_output_tokens) = 3000, that is
1800 tokens. Each message
is estimated at about 550 tokens and the system prompt at about 590, so the
first turn (about 1140) stays below and the second (about 2250) reaches it.

The summary model is `summary_model_id` (gpt-5-mini). The mock answers the
non-streaming summary request with `MOCK-SUMMARY <n> user and <m> assistant
messages` (mock_provider/responses.py).
"""

from __future__ import annotations

import json
import time
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    CATALOG_SYSTEM_PROMPT,
    STANDARD_MODEL,
    TENANT_A_ID,
    THREAD_SUMMARY_QUEUE,
    TINY_CTX_MODEL,
    TOKEN_USER_A,
    TOKEN_USER_B,
    USAGE_QUEUE,
    USER_A_ID,
    USER_B_ID,
    auth_headers,
    exec_db,
    expect_done,
    expect_stream_started,
    list_messages,
    parse_sse,
    outbox_payloads,
    poll_turn,
    provider_input,
    query_db,
    stream_message,
    usage_events,
    uuid_from_db,
    wait_for,
)
from .mock_provider.responses import (
    SUMMARY_OUTPUT_TOKENS, SUMMARY_REASONING_TOKENS, mock_summary_text,
)
from .test_cleanup import _dead_letters, _processed_seq

SUMMARY_MODEL_PROVIDER_ID = "gpt-5-mini"  # provider_model_id of summary_model_id

# `token_estimate` of a stored summary: the summary response's output tokens
# without its reasoning tokens (thread_summary_worker.rs
# `estimate_summary_tokens`).
SUMMARY_TOKEN_ESTIMATE = SUMMARY_OUTPUT_TOKENS - SUMMARY_REASONING_TOKENS
assert SUMMARY_TOKEN_ESTIMATE > 0

# Subject of the summary request: the platform default subject
# (toolkit_security::constants::DEFAULT_SUBJECT_ID, libs/toolkit-security/src/constants.rs).
# It is also user A's id (config/base.yaml), so the summary test runs in a
# chat of user B, where the two differ.
DEFAULT_SUBJECT_ID = "11111111-6a88-4768-9dfc-6bcd5187d9ed"
assert DEFAULT_SUBJECT_ID == USER_A_ID != USER_B_ID

# Prefix of the summary message in the provider input
# (SUMMARY_PREAMBLE, domain/service/context_assembly.rs).
SUMMARY_PREAMBLE = (
    "This conversation has earlier messages that have been summarized. "
    "The summary below covers the earlier portion of the conversation. "
    "Recent messages follow after.\n\n"
)


def _require_offline(request):
    if request.config.getoption("mode") == "online":
        pytest.skip("inspects the mock provider's captured requests")


def _complete_turn(chat_id: str, content: str, request_id: str | None = None,
                   *, token: str = TOKEN_USER_A):
    rid = request_id or str(uuid.uuid4())
    status, events, raw = stream_message(chat_id, content, request_id=rid, token=token)
    assert status == 200, raw
    expect_done(events)
    poll_turn(chat_id, rid, ("done",), token=token)
    # The turn's usage event is enqueued in its finalization transaction,
    # the same one that would enqueue a summary task.
    assert len(usage_events(rid)) == 1, usage_events(rid)
    return events


def _summary_tasks(chat_id: str) -> list[dict]:
    """Thread-summary tasks enqueued for the chat (conftest "Outbox capture").

    The task is enqueued in the transaction that finalizes the turn
    (finalization_service.rs), so once the turn is `done` it is either
    captured or was not enqueued at all."""
    return [
        p for p in outbox_payloads(chat_id, THREAD_SUMMARY_QUEUE)
        if p.get("chat_id") == chat_id
    ]


def _summary_rows(chat_id: str) -> list[dict]:
    return query_db(
        "SELECT summary_text, token_estimate, summarized_up_to_message_id "
        "FROM thread_summaries WHERE chat_id = ?",
        (chat_id,),
    )


def _summary_requests(mock_provider, chat_id: str) -> list[dict]:
    """Thread summary requests the mock received for the chat."""
    return [
        r for r in mock_provider.get_summary_requests()
        if (r.get("metadata") or {}).get("chat_id") == chat_id
    ]


def _wait_for_summary(chat_id: str, timeout: float = 30.0) -> dict:
    deadline = time.monotonic() + timeout
    rows = _summary_rows(chat_id)
    while not rows and time.monotonic() < deadline:
        time.sleep(0.2)
        rows = _summary_rows(chat_id)
    assert len(rows) == 1, f"no thread summary for chat {chat_id} within {timeout}s"
    return rows[0]


class TestThreadSummary:
    """16-05, 19-09, 19-10."""

    @pytest.mark.timeout(60)
    def test_summary_replaces_summarized_messages(self, request, chat_with_model, mock_provider):
        """The turn that reaches the threshold schedules a summary of the
        messages before it (the finalized turn stays out: retry, edit and
        delete may still replace it); the worker stores the summary and marks
        those messages compressed; the next turn sends the summary instead of
        them, followed by the unsummarized turn.

        The chat belongs to user B: the summary runs as the platform default
        subject, which is user A's id in this rig."""
        _require_offline(request)
        resp = httpx.post(
            f"{API_PREFIX}/chats", json={"model": TINY_CTX_MODEL},
            headers=auth_headers(TOKEN_USER_B), timeout=10,
        )
        assert resp.status_code == 201, resp.text
        chat_id = resp.json()["id"]

        _complete_turn(chat_id, "First question.", token=TOKEN_USER_B)
        assert _summary_tasks(chat_id) == [], "one turn is below the threshold"

        _complete_turn(chat_id, "Second question.", token=TOKEN_USER_B)
        tasks = _summary_tasks(chat_id)
        assert len(tasks) == 1, tasks
        summary = _wait_for_summary(chat_id)

        messages = list_messages(chat_id, token=TOKEN_USER_B)
        assert [m["role"] for m in messages] == ["user", "assistant"] * 2
        assert summary["summary_text"] == "MOCK-SUMMARY 1 user and 1 assistant messages"
        assert summary["token_estimate"] == SUMMARY_TOKEN_ESTIMATE
        # The frontier is the first turn's answer, not the finalized second turn.
        assert uuid_from_db(summary["summarized_up_to_message_id"]) == messages[1]["id"]
        assert tasks[0]["frozen_target_message_id"] == messages[1]["id"], tasks
        compressed = query_db(
            "SELECT is_compressed FROM messages WHERE chat_id = ? AND deleted_at IS NULL "
            "ORDER BY created_at, id",
            (chat_id,),
        )
        assert [r["is_compressed"] for r in compressed] == [1, 1, 0, 0]
        # The summary's usage event, enqueued in the transaction that stored
        # the summary: a system task, with no user and no turn.
        (summary_usage,) = [
            p for p in outbox_payloads(chat_id, USAGE_QUEUE)
            if p.get("chat_id") == chat_id
            and p.get("system_task_type") == "thread_summary_update"
        ]
        assert summary_usage["requester_type"] == "system", summary_usage
        assert "user_id" not in summary_usage and "turn_id" not in summary_usage, summary_usage

        # The summary request: non-streaming, on the summary model, with the
        # first turn in the prompt and without the second.
        summary_requests = _summary_requests(mock_provider, chat_id)
        assert len(summary_requests) == 1, summary_requests
        assert summary_requests[0]["model"] == SUMMARY_MODEL_PROVIDER_ID
        # A system task: the tenant with the default subject (not the chat
        # owner, user B), request_type summary.
        assert summary_requests[0]["user"] == (
            TENANT_A_ID.replace("-", "") + DEFAULT_SUBJECT_ID.replace("-", "")
        )
        assert summary_requests[0]["metadata"] == {
            "tenant_id": TENANT_A_ID,
            "user_id": DEFAULT_SUBJECT_ID,
            "chat_id": chat_id,
            "request_type": "summary",
            "feature": "none",
        }, summary_requests[0]["metadata"]
        prompt = provider_input(summary_requests[0])[0][1]
        for line in ("User: First question.", f"Assistant: {messages[1]['content']}"):
            assert line in prompt, (line, prompt)
        assert "Second question." not in prompt, prompt

        mock_provider.clear_captured_requests()
        events = _complete_turn(chat_id, "Third question.", token=TOKEN_USER_B)
        assert expect_stream_started(events).data["thread_summary_applied"] == {
            "token_estimate": SUMMARY_TOKEN_ESTIMATE,
        }
        captured = mock_provider.get_captured_requests()
        assert len(captured) == 1, captured
        assert captured[0]["instructions"] == CATALOG_SYSTEM_PROMPT
        # The second turn is not summarized, and on this small-context model
        # it does not fit next to the summary: truncation drops it as a whole
        # turn (question and answer), never an answer without its question.
        assert provider_input(captured[0]) == [
            ("user", SUMMARY_PREAMBLE + summary["summary_text"]),
            ("user", "Third question."),
        ]

    @pytest.mark.timeout(60)
    def test_retry_after_summary_does_not_resend_replaced_answer(
        self, request, chat_with_model, mock_provider,
    ):
        """Retrying the turn that triggered the summary: the summary does not
        contain that turn, so it is kept, and the retried request carries the
        summary, the original question and nothing of the replaced answer."""
        _require_offline(request)
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        _complete_turn(chat_id, "First question.")
        second_rid = str(uuid.uuid4())
        _complete_turn(chat_id, "Second question.", request_id=second_rid)
        summary = _wait_for_summary(chat_id)
        replaced_answer = list_messages(chat_id)[3]["content"]

        mock_provider.clear_captured_requests()
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{second_rid}/retry",
            headers={"Accept": "text/event-stream"}, timeout=90,
        )
        assert resp.status_code == 200, resp.text
        expect_done(parse_sse(resp.text))

        captured = mock_provider.get_captured_requests()
        assert len(captured) == 1, captured
        assert provider_input(captured[0]) == [
            ("user", SUMMARY_PREAMBLE + summary["summary_text"]),
            ("user", "Second question."),
        ]
        assert replaced_answer not in str(captured[0]), "replaced answer resent"
        assert _summary_rows(chat_id) == [summary], "a summary that does not cover the turn is kept"

    @pytest.mark.timeout(60)
    @pytest.mark.parametrize("mutation", ["retry", "edit", "delete"])
    def test_mutation_of_summarized_turn_drops_summary(
        self, request, chat_with_model, mock_provider, mutation,
    ):
        """After DELETE of the turn that triggered the summary, the first
        turn is the latest again, and the summary covers it. A retry, edit or
        delete of that turn deletes the summary in the mutation transaction:
        a retry or edit sends the (new) question without the summary, and no
        summary row is left."""
        _require_offline(request)
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        first_rid = str(uuid.uuid4())
        _complete_turn(chat_id, "First question.", request_id=first_rid)
        second_rid = str(uuid.uuid4())
        _complete_turn(chat_id, "Second question.", request_id=second_rid)
        summary = _wait_for_summary(chat_id)
        assert uuid_from_db(summary["summarized_up_to_message_id"]) == list_messages(chat_id)[1]["id"]

        turn_url = f"{API_PREFIX}/chats/{chat_id}/turns"
        resp = httpx.delete(f"{turn_url}/{second_rid}", timeout=10)
        assert resp.status_code == 204, resp.text
        # The deleted turn is after the summary frontier: the summary stays.
        assert _summary_rows(chat_id) == [summary]

        mock_provider.clear_captured_requests()
        if mutation == "delete":
            resp = httpx.delete(f"{turn_url}/{first_rid}", timeout=10)
            assert resp.status_code == 204, resp.text
            assert _summary_rows(chat_id) == []
            return

        if mutation == "retry":
            question = "First question."
            resp = httpx.post(
                f"{turn_url}/{first_rid}/retry",
                headers={"Accept": "text/event-stream"}, timeout=90,
            )
        else:
            question = "First question, edited."
            resp = httpx.patch(
                f"{turn_url}/{first_rid}", json={"content": question},
                headers={"Accept": "text/event-stream"}, timeout=90,
            )
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        expect_done(events)
        assert expect_stream_started(events).data.get("thread_summary_applied") is None
        assert _summary_rows(chat_id) == []

        captured = mock_provider.get_captured_requests()
        assert len(captured) == 1, captured
        assert provider_input(captured[0]) == [("user", question)]
        assert summary["summary_text"] not in str(captured[0]), "stale summary sent"


    @pytest.mark.timeout(90)
    def test_retry_of_summarized_turn_restores_earlier_history(
        self, request, chat_with_model, mock_provider,
    ):
        """Three turns: the summary after the third covers the first two.
        After DELETE of the third turn, a retry of the second drops the
        summary and clears `is_compressed`: the retried request carries the
        first turn as history, then the second question."""
        _require_offline(request)
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        _complete_turn(chat_id, "First question.")
        second_rid = str(uuid.uuid4())
        _complete_turn(chat_id, "Second question.", request_id=second_rid)
        _wait_for_summary(chat_id)
        third_rid = str(uuid.uuid4())
        _complete_turn(chat_id, "Third question.", request_id=third_rid)
        messages = list_messages(chat_id)
        # The next summary moves the frontier to the second turn's answer.
        summary = wait_for(
            lambda: [
                r for r in _summary_rows(chat_id)
                if uuid_from_db(r["summarized_up_to_message_id"]) == messages[3]["id"]
            ],
            "the summary of the first two turns", timeout=30,
        )[0]

        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/turns/{third_rid}", timeout=10)
        assert resp.status_code == 204, resp.text
        assert _summary_rows(chat_id) == [summary]

        mock_provider.clear_captured_requests()
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{second_rid}/retry",
            headers={"Accept": "text/event-stream"}, timeout=90,
        )
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        expect_done(events)
        assert expect_stream_started(events).data.get("thread_summary_applied") is None
        assert _summary_rows(chat_id) == []

        captured = mock_provider.get_captured_requests()
        assert len(captured) == 1, captured
        assert provider_input(captured[0]) == [
            ("user", "First question."),
            ("assistant", messages[1]["content"]),
            ("user", "Second question."),
        ]

# ThreadSummaryWorkerConfig default `max_attempts` (src/config/background.rs),
# not overridden in config/base.yaml.
SUMMARY_MAX_ATTEMPTS = 3


class TestThreadSummaryFailures:
    """The summary request fails at the provider."""

    @pytest.mark.timeout(60)
    def test_failed_summary_request_is_retried(self, request, chat_with_model, mock_provider):
        """A provider 500 on the summary request: the task is retried and the
        second request stores the summary."""
        _require_offline(request)
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        mock_provider.set_summary_fault(chat_id, 500, count=1)
        _complete_turn(chat_id, "First question.")
        _complete_turn(chat_id, "Second question.")

        summary = _wait_for_summary(chat_id)
        assert summary["summary_text"] == "MOCK-SUMMARY 1 user and 1 assistant messages"
        assert len(_summary_requests(mock_provider, chat_id)) == 2

    @pytest.mark.timeout(60)
    def test_context_length_exceeded_drops_oldest_messages(
        self, request, chat_with_model, mock_provider,
    ):
        """A summary request answered 400 `context_length_exceeded` is sent
        again without the oldest messages (thread_summary_worker.rs: drop
        max(ceil(n / 5), 2) of n, keep at least 2), and that summary is
        stored with the same frontier.

        Six messages to summarize: three turns on gpt-5.2 (large context, no
        summary), then the chat is switched to the tiny-context model (DB
        seed, as in test_turn_mutations.py 08-29); its next turn reaches the
        threshold and schedules a summary of those six messages."""
        _require_offline(request)
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        for i in range(3):
            _complete_turn(chat_id, f"Question {i + 1}.")
        assert _summary_tasks(chat_id) == []
        assert exec_db("UPDATE chats SET model = ? WHERE id = ?", (TINY_CTX_MODEL, chat_id)) == 1
        mock_provider.set_summary_fault(chat_id, 400, {"error": {
            "message": "This model's maximum context length is 4096 tokens.",
            "type": "invalid_request_error", "param": "input", "code": "context_length_exceeded",
        }})
        _complete_turn(chat_id, "Question 4.")

        summary = _wait_for_summary(chat_id)
        first, second = _summary_requests(mock_provider, chat_id)
        assert mock_summary_text(provider_input(first)[0][1]) == (
            "MOCK-SUMMARY 3 user and 3 assistant messages"
        )
        retried_prompt = provider_input(second)[0][1]
        assert "User: Question 1." not in retried_prompt, retried_prompt
        assert "User: Question 2." in retried_prompt, retried_prompt
        assert summary["summary_text"] == "MOCK-SUMMARY 2 user and 2 assistant messages"
        messages = list_messages(chat_id)
        assert uuid_from_db(summary["summarized_up_to_message_id"]) == messages[5]["id"]

    @pytest.mark.timeout(60)
    def test_summary_failing_every_attempt_changes_nothing(
        self, request, chat_with_model, mock_provider,
    ):
        """The summary request fails on each of the `max_attempts` (3)
        deliveries: the task is dropped, no summary is stored, no message is
        marked compressed, and the next turn is sent without a summary."""
        _require_offline(request)
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        mock_provider.set_summary_fault(chat_id, 500, count=SUMMARY_MAX_ATTEMPTS)
        _complete_turn(chat_id, "First question.")
        _complete_turn(chat_id, "Second question.")

        deadline = time.monotonic() + 30
        while (
            len(_summary_requests(mock_provider, chat_id)) < SUMMARY_MAX_ATTEMPTS
            and time.monotonic() < deadline
        ):
            time.sleep(0.2)
        assert len(_summary_requests(mock_provider, chat_id)) == SUMMARY_MAX_ATTEMPTS
        # The third failure rejects the task: it is dead-lettered, and the
        # processor offset of its partition is past it in the same
        # transaction, so it is never delivered again (see test_cleanup.py
        # check_vector_store_delete_500_is_dead_lettered).
        (dead,) = wait_for(
            lambda: [
                d for d in _dead_letters(chat_id)
                if "frozen_target_message_id" in json.loads(d["payload"])
            ],
            "the dead-lettered summary task",
        )
        assert f"max attempts ({SUMMARY_MAX_ATTEMPTS})" in dead["last_error"], dead
        assert _processed_seq(dead["partition_id"]) >= dead["seq"], dead
        assert len(_summary_requests(mock_provider, chat_id)) == SUMMARY_MAX_ATTEMPTS
        assert _summary_rows(chat_id) == []
        compressed = query_db(
            "SELECT is_compressed FROM messages WHERE chat_id = ? AND deleted_at IS NULL",
            (chat_id,),
        )
        assert [r["is_compressed"] for r in compressed] == [0, 0, 0, 0]

        mock_provider.clear_captured_requests()
        events = _complete_turn(chat_id, "Third question.")
        assert expect_stream_started(events).data.get("thread_summary_applied") is None
        (req,) = mock_provider.get_captured_requests()
        assert all(not text.startswith(SUMMARY_PREAMBLE) for _, text in provider_input(req))
        assert provider_input(req)[-1] == ("user", "Third question.")
