"""Tests for turn mutation operations — retry, edit, delete, concurrency, replaced_by tracking."""

import threading
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    BARE_MODEL,
    RESOURCE_ATTACHMENT,
    RESOURCE_CHAT,
    STANDARD_MODEL,
    TINY_CTX_MODEL,
    USER_A_ID,
    USER_B_ID,
    OpenStream,
    assert_no_reserves,
    assert_problem,
    delta_text,
    exec_db,
    expect_done,
    expect_stream_started,
    list_messages,
    open_stream,
    parse_sse,
    poll_turn,
    provider_file_id,
    provider_input,
    query_db,
    slow_scenario,
    stream_message,
    turn_count,
    usage_events,
    uuid_from_db,
)
from .mock_provider.responses import MockEvent, Scenario
from .test_attachments import MAX_IMAGES_PER_MESSAGE, _upload_ready, make_minimal_png


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def complete_turn(chat_id: str, content: str = "Say OK.") -> str:
    """Send a message and return its request_id once the turn is done."""
    rid = str(uuid.uuid4())
    status, events, _ = stream_message(chat_id, content, request_id=rid)
    assert status == 200, f"stream_message failed: {status}"
    expect_done(events)
    poll_turn(chat_id, rid, ("done",))
    return rid


def turn_url(chat_id: str, rid: str) -> str:
    return f"{API_PREFIX}/chats/{chat_id}/turns/{rid}"


def retry(chat_id: str, rid: str) -> httpx.Response:
    return httpx.post(
        f"{turn_url(chat_id, rid)}/retry",
        headers={"Accept": "text/event-stream"}, timeout=90,
    )


def edit(chat_id: str, rid: str, content: str) -> httpx.Response:
    return httpx.patch(
        turn_url(chat_id, rid), json={"content": content},
        headers={"Accept": "text/event-stream"}, timeout=90,
    )


def cancel_turn(chat_id: str, mock_provider, content: str = "Cancel me.") -> tuple[str, str]:
    """Disconnect after two deltas; return (request_id, received text)."""
    rid = str(uuid.uuid4())
    mock_provider.set_next_scenario(slow_scenario(20, slow=0.3))
    with open_stream(chat_id, content, request_id=rid) as s:
        seen = []
        s.read_until(lambda e: e.event == "delta" and (seen.append(e) or len(seen) == 2))
        received = delta_text(s.events)
    assert poll_turn(chat_id, rid)["state"] == "cancelled"
    return rid, received


def _require_offline(request):
    if request.config.getoption("mode") == "online":
        pytest.skip("requires mock provider (offline mode)")


# ---------------------------------------------------------------------------
# Tests: retry
# ---------------------------------------------------------------------------

class TestTurnRetry:
    """POST /turns/{request_id}/retry constraints and effect."""

    @pytest.mark.timeout(30)
    def test_retry_running_turn_400(self, request, chat, mock_provider):
        """Retrying a running turn is 400 failed_precondition (turn_state/STATE)."""
        _require_offline(request)
        chat_id = chat["id"]
        rid = str(uuid.uuid4())
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))

        with open_stream(chat_id, "Slow turn.", request_id=rid) as s:
            s.read_until_started()
            resp = retry(chat_id, rid)
            assert_problem(
                resp, 400, "failed_precondition",
                violation_subject="turn_state", violation_type="STATE",
            )
            assert httpx.get(turn_url(chat_id, rid)).json()["state"] == "running"
            expect_done(s.drain())

    @pytest.mark.timeout(30)
    def test_retry_non_latest_turn_409(self, chat):
        """Retrying a turn that is not the latest is 409 NOT_LATEST_TURN; nothing changes."""
        chat_id = chat["id"]
        rid1 = complete_turn(chat_id, "First turn.")
        complete_turn(chat_id, "Second turn.")
        before = list_messages(chat_id)

        assert_problem(retry(chat_id, rid1), 409, "aborted", reason="NOT_LATEST_TURN")
        assert list_messages(chat_id) == before

    @pytest.mark.timeout(30)
    @pytest.mark.parametrize("mutation", ["retry", "edit"])
    @pytest.mark.parametrize("earlier_turns", [0, 1])
    def test_mutation_sends_the_question_once(
        self, request, chat, mock_provider, mutation, earlier_turns,
    ):
        """The provider request of a retry or edit holds the history before
        the turn and the (new) question once, with or without earlier turns."""
        _require_offline(request)
        chat_id = chat["id"]
        history = []
        for i in range(earlier_turns):
            complete_turn(chat_id, f"Earlier {i}.")
            history += [("user", f"Earlier {i}."), ("assistant", "Hello! How can I help?")]
        rid = complete_turn(chat_id, "Mutate me.")

        mock_provider.clear_captured_requests()
        if mutation == "retry":
            question = "Mutate me."
            resp = retry(chat_id, rid)
        else:
            question = "Mutated."
            resp = edit(chat_id, rid, question)
        assert resp.status_code == 200, resp.text
        expect_done(parse_sse(resp.text))
        (req,) = mock_provider.get_captured_requests()
        assert provider_input(req) == [*history, ("user", question)]

    @pytest.mark.timeout(30)
    def test_retry_over_context_budget_400(self, chat_with_model, mock_provider):
        """A 6000-byte question sent on gpt-5.2 is over the context budget of
        gpt-4.1-mini-tiny-ctx (2500 tokens with the system prompt, see
        TestTurnEdit::test_edit_content_over_context_budget_400) and within
        its max_input_tokens. With the chat switched to that model in the DB,
        a retry is 400 out_of_range CONTEXT_BUDGET_EXCEEDED, the provider is
        not called; context assembly runs after the retry committed, so the
        old turn is replaced and the new turn fails with
        `context_length_exceeded`, without an answer or a usage event."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        content = "x" * 6_000
        rid = complete_turn(chat_id, content)
        assert exec_db("UPDATE chats SET model = ? WHERE id = ?", (TINY_CTX_MODEL, chat_id)) == 1

        mock_provider.clear_captured_requests()
        resp = retry(chat_id, rid)
        assert_problem(resp, 400, "out_of_range", field_reason="CONTEXT_BUDGET_EXCEEDED")
        assert mock_provider.get_captured_requests() == []

        live = query_db(
            "SELECT request_id FROM chat_turns WHERE chat_id = ? AND deleted_at IS NULL",
            (chat_id,),
        )
        assert len(live) == 1, live
        new_rid = uuid_from_db(live[0]["request_id"])
        assert new_rid != rid
        turn = poll_turn(chat_id, new_rid)
        assert (turn["state"], turn["error_code"]) == ("error", "context_length_exceeded"), turn
        assert [(m["role"], m["content"]) for m in list_messages(chat_id)] == [("user", content)]
        assert usage_events(new_rid) == []
        assert_no_reserves(USER_A_ID)

    @pytest.mark.timeout(30)
    def test_retry_replaces_the_answer(self, chat):
        """After retry the chat holds the original user message and exactly one new answer."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Retry me.")

        resp = retry(chat_id, rid)
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        new_rid = expect_stream_started(events).data["request_id"]
        expect_done(events)

        messages = list_messages(chat_id)
        assert [(m["role"], m["request_id"]) for m in messages] == [
            ("user", new_rid), ("assistant", new_rid),
        ]
        assert messages[0]["content"] == "Retry me."
        assert messages[1]["content"] == delta_text(events)

    @pytest.mark.timeout(30)
    def test_retry_failed_turn(self, request, chat, mock_provider):
        """08-12: a turn in `error` state can be retried; the retry produces one new answer."""
        _require_offline(request)
        chat_id = chat["id"]
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "Partial"})],
            terminal="failed",
            error={"code": "server_error", "message": "Mock provider error"},
        ))
        rid = str(uuid.uuid4())
        status, _, _ = stream_message(chat_id, "Trigger error.", request_id=rid)
        assert status == 200
        assert poll_turn(chat_id, rid)["state"] == "error"

        resp = retry(chat_id, rid)
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        new_rid = expect_stream_started(events).data["request_id"]
        expect_done(events)

        assistant = [m for m in list_messages(chat_id) if m["role"] == "assistant"]
        assert [m["request_id"] for m in assistant] == [new_rid]

    @pytest.mark.timeout(30)
    def test_retry_cancelled_turn(self, request, chat, mock_provider):
        """A cancelled turn can be retried; its partial answer is replaced by one new answer."""
        _require_offline(request)
        chat_id = chat["id"]
        rid, _ = cancel_turn(chat_id, mock_provider)

        resp = retry(chat_id, rid)
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        new_rid = expect_stream_started(events).data["request_id"]
        expect_done(events)

        messages = list_messages(chat_id)
        assert [(m["role"], m["request_id"]) for m in messages] == [
            ("user", new_rid), ("assistant", new_rid),
        ]
        assert messages[1]["content"] == delta_text(events)


# ---------------------------------------------------------------------------
# Tests: edit
# ---------------------------------------------------------------------------

class TestTurnEdit:
    """PATCH /turns/{request_id} replaces the last user message and its answer."""

    @pytest.mark.timeout(30)
    def test_edit_replaces_user_message_and_answer(self, chat):
        chat_id = chat["id"]
        complete_turn(chat_id, "Keep this turn.")
        rid = complete_turn(chat_id, "Original question.")
        before = list_messages(chat_id)

        resp = edit(chat_id, rid, "Edited question.")
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        new_rid = expect_stream_started(events).data["request_id"]
        expect_done(events)

        messages = list_messages(chat_id)
        assert messages[:2] == before[:2], "the earlier turn must be untouched"
        assert [(m["role"], m["request_id"]) for m in messages[2:]] == [
            ("user", new_rid), ("assistant", new_rid),
        ]
        assert messages[2]["content"] == "Edited question."
        assert messages[3]["content"] == delta_text(events)
        old_ids = {m["id"] for m in before[2:]}
        assert old_ids.isdisjoint(m["id"] for m in messages)

    @pytest.mark.timeout(30)
    def test_edit_empty_content_400(self, chat):
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Question.")
        before = list_messages(chat_id)

        assert_problem(edit(chat_id, rid, ""), 400, "invalid_argument", field_reason="EMPTY_CONTENT")
        assert list_messages(chat_id) == before

    @pytest.mark.timeout(30)
    def test_edit_whitespace_only_content_400(self, chat):
        """Edit content of only whitespace is empty after trimming: 400
        invalid_argument EMPTY_CONTENT on `content`; the turn is kept."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Question.")
        before = list_messages(chat_id)

        body = assert_problem(
            edit(chat_id, rid, "  \n\t "), 400, "invalid_argument", field_reason="EMPTY_CONTENT",
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["content"], body
        assert list_messages(chat_id) == before
        assert poll_turn(chat_id, rid)["state"] == "done"

    @pytest.mark.timeout(30)
    def test_edit_body_errors(self, chat):
        """A body without `content` (schema-invalid) is 422 invalid_argument;
        malformed JSON is 400 invalid_argument. The turn is kept."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Question.")
        before = list_messages(chat_id)

        resp = httpx.patch(turn_url(chat_id, rid), json={}, timeout=30)
        assert_problem(resp, 422, "invalid_argument")
        resp = httpx.patch(
            turn_url(chat_id, rid), content=b"{not json",
            headers={"Content-Type": "application/json"}, timeout=30,
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="json_syntax_error")
        assert list_messages(chat_id) == before

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.timeout(30)
    def test_edit_content_over_max_input_tokens_400(self, chat_with_model, mock_provider):
        """Edit content of 12000 bytes on gpt-4.1-mini-tiny-ctx is estimated at
        (3000 + 500) * 1.1 = 3850 tokens > max_input_tokens 3000 (see
        test_streaming.py TestStreamInputLimits): 400 out_of_range
        INPUT_TOO_LONG; the turn is kept and the provider is not called."""
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        rid = complete_turn(chat_id, "Question.")
        before = list_messages(chat_id)

        mock_provider.clear_captured_requests()
        resp = edit(chat_id, rid, "x" * 12_000)
        assert_problem(resp, 400, "out_of_range", field_reason="INPUT_TOO_LONG")
        assert mock_provider.get_captured_requests() == []
        assert list_messages(chat_id) == before
        assert poll_turn(chat_id, rid)["state"] == "done"

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.timeout(30)
    def test_edit_content_over_context_budget_400(self, chat_with_model, mock_provider):
        """Edit content of 6000 bytes on gpt-4.1-mini-tiny-ctx (2200 tokens)
        passes max_input_tokens but, with the system prompt (587 tokens),
        exceeds the context budget min(3000, 4096 - 1024) - 500 = 2500 (see
        test_streaming.py TestStreamInputLimits). Context assembly
        runs after the edit committed (DESIGN §3.9, preflight-before-mutation
        order): 400 out_of_range CONTEXT_BUDGET_EXCEEDED, the provider is not
        called, the old turn is replaced and the new turn fails with
        `context_length_exceeded` and no answer."""
        chat_id = chat_with_model(TINY_CTX_MODEL)["id"]
        rid = complete_turn(chat_id, "Question.")
        content = "x" * 6_000

        mock_provider.clear_captured_requests()
        resp = edit(chat_id, rid, content)
        assert_problem(resp, 400, "out_of_range", field_reason="CONTEXT_BUDGET_EXCEEDED")
        assert mock_provider.get_captured_requests() == []

        live = query_db(
            "SELECT request_id FROM chat_turns WHERE chat_id = ? AND deleted_at IS NULL",
            (chat_id,),
        )
        assert len(live) == 1, live
        new_rid = uuid_from_db(live[0]["request_id"])
        assert new_rid != rid
        assert httpx.get(turn_url(chat_id, rid)).status_code == 404
        turn = poll_turn(chat_id, new_rid)
        assert (turn["state"], turn["error_code"]) == ("error", "context_length_exceeded"), turn
        assert turn.get("assistant_message_id") is None, turn
        assert [(m["role"], m["content"], m["request_id"]) for m in list_messages(chat_id)] == [
            ("user", content, new_rid),
        ]
        # A turn that never reached the provider is not billed: no usage event.
        assert usage_events(new_rid) == []
        assert_no_reserves(USER_A_ID)

    @pytest.mark.timeout(30)
    def test_edit_non_latest_turn_409(self, chat):
        chat_id = chat["id"]
        rid1 = complete_turn(chat_id, "First turn.")
        complete_turn(chat_id, "Second turn.")
        before = list_messages(chat_id)

        assert_problem(edit(chat_id, rid1, "Changed."), 409, "aborted", reason="NOT_LATEST_TURN")
        assert list_messages(chat_id) == before

    @pytest.mark.timeout(30)
    def test_edit_running_turn_400(self, request, chat, mock_provider):
        """Editing a running turn is 400 failed_precondition (turn_state/STATE);
        the turn keeps streaming."""
        _require_offline(request)
        chat_id = chat["id"]
        rid = str(uuid.uuid4())
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))

        with open_stream(chat_id, "Slow turn.", request_id=rid) as s:
            s.read_until_started()
            assert_problem(
                edit(chat_id, rid, "Changed."), 400, "failed_precondition",
                violation_subject="turn_state", violation_type="STATE",
            )
            assert httpx.get(turn_url(chat_id, rid)).json()["state"] == "running"
            expect_done(s.drain())
        assert [m["content"] for m in list_messages(chat_id) if m["role"] == "user"] == [
            "Slow turn.",
        ]


class TestUnknownTurn:
    """Retry, edit and delete of a request_id that has no turn in the chat: 404 not_found."""

    @pytest.mark.timeout(30)
    def test_retry_unknown_turn_404(self, chat):
        complete_turn(chat["id"])
        assert_problem(retry(chat["id"], str(uuid.uuid4())), 404, "not_found")

    @pytest.mark.timeout(30)
    def test_edit_unknown_turn_404(self, chat):
        complete_turn(chat["id"])
        assert_problem(edit(chat["id"], str(uuid.uuid4()), "Changed."), 404, "not_found")

    @pytest.mark.timeout(30)
    def test_delete_unknown_turn_404(self, chat):
        complete_turn(chat["id"])
        resp = httpx.delete(turn_url(chat["id"], str(uuid.uuid4())), timeout=10)
        assert_problem(resp, 404, "not_found")


# ---------------------------------------------------------------------------
# Tests: delete
# ---------------------------------------------------------------------------

class TestTurnDelete:
    """DELETE /turns/{request_id} constraints and behavior."""

    @pytest.mark.timeout(30)
    def test_delete_last_turn_204(self, chat):
        """Deleting the last (and only) turn returns 204 and removes messages."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Only turn.")

        resp = httpx.delete(turn_url(chat_id, rid), timeout=10)
        assert resp.status_code == 204
        assert list_messages(chat_id) == []

    @pytest.mark.timeout(30)
    def test_delete_running_turn_400(self, request, chat, mock_provider):
        """Deleting a running turn is 400 failed_precondition (turn_state/STATE)."""
        _require_offline(request)
        chat_id = chat["id"]
        rid = str(uuid.uuid4())
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))

        with open_stream(chat_id, "Slow turn.", request_id=rid) as s:
            s.read_until_started()
            resp = httpx.delete(turn_url(chat_id, rid), timeout=10)
            assert_problem(
                resp, 400, "failed_precondition",
                violation_subject="turn_state", violation_type="STATE",
            )
            assert httpx.get(turn_url(chat_id, rid)).json()["state"] == "running"
            expect_done(s.drain())

    @pytest.mark.timeout(30)
    def test_delete_non_latest_turn_409(self, chat):
        """Deleting a turn that is not the latest is 409 NOT_LATEST_TURN; nothing changes."""
        chat_id = chat["id"]
        rid1 = complete_turn(chat_id, "First turn.")
        complete_turn(chat_id, "Second turn.")
        before = list_messages(chat_id)

        resp = httpx.delete(turn_url(chat_id, rid1), timeout=10)
        assert_problem(resp, 409, "aborted", reason="NOT_LATEST_TURN")
        assert list_messages(chat_id) == before

    @pytest.mark.timeout(30)
    def test_soft_deleted_turn_excluded_from_messages(self, chat):
        """After deleting the last turn, its messages disappear from GET /messages."""
        chat_id = chat["id"]
        rid1 = complete_turn(chat_id, "First turn.")
        rid2 = complete_turn(chat_id, "Second turn.")

        resp = httpx.delete(turn_url(chat_id, rid2), timeout=10)
        assert resp.status_code == 204

        messages = list_messages(chat_id)
        assert [(m["role"], m["request_id"]) for m in messages] == [
            ("user", rid1), ("assistant", rid1),
        ]

    @pytest.mark.timeout(30)
    def test_get_deleted_turn_404(self, chat):
        chat_id = chat["id"]
        rid = complete_turn(chat_id)
        assert httpx.delete(turn_url(chat_id, rid), timeout=10).status_code == 204

        assert_problem(httpx.get(turn_url(chat_id, rid)), 404, "not_found")

    @pytest.mark.timeout(30)
    def test_second_delete_turn_409_not_latest(self, chat):
        """A deleted turn is no longer the latest one: deleting it again is 409 NOT_LATEST_TURN."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id)
        assert httpx.delete(turn_url(chat_id, rid), timeout=10).status_code == 204

        resp = httpx.delete(turn_url(chat_id, rid), timeout=10)
        assert_problem(resp, 409, "aborted", reason="NOT_LATEST_TURN")

    @pytest.mark.timeout(30)
    def test_deleted_turn_not_sent_to_provider(self, request, chat, mock_provider):
        """The content of a deleted turn is not part of the next provider request."""
        _require_offline(request)
        chat_id = chat["id"]
        complete_turn(chat_id, "KEEP-7f3a question.")
        rid = complete_turn(chat_id, "FORGET-9c1d question.")
        assert httpx.delete(turn_url(chat_id, rid), timeout=10).status_code == 204

        mock_provider.clear_captured_requests()
        complete_turn(chat_id, "Next question.")
        provider_input = str(mock_provider.get_last_request()["input"])
        assert "KEEP-7f3a" in provider_input
        assert "FORGET-9c1d" not in provider_input


# ---------------------------------------------------------------------------
# Tests: concurrent retries
# ---------------------------------------------------------------------------

class TestConcurrentRetries:
    """Two retries of the same turn — exactly one wins."""

    @pytest.mark.timeout(30)
    def test_concurrent_retries_one_wins(self, request, chat, mock_provider):
        """Two simultaneous retries of the same turn: one streams (200), the
        other is 409 NOT_LATEST_TURN.

        The winner streams a slow answer, so it is still running when the
        loser is checked. The rig's SQLite pool has one connection
        (base.yaml `max_conns: 1`), so the two mutation transactions run one
        after the other: the loser sees the winner's new turn as the latest
        one. GENERATION_IN_PROGRESS needs two mutation transactions in flight
        at once; only its error mapping is unit-tested (api/rest/error.rs).
        """
        _require_offline(request)
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Retryable turn.")
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))

        results = [None, None]

        def send_retry(idx):
            results[idx] = retry(chat_id, rid)

        threads = [threading.Thread(target=send_retry, args=(i,)) for i in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=25)
        assert [t.is_alive() for t in threads] == [False, False], "a retry did not return"
        assert None not in results, results  # a retry raised in its thread

        by_status = sorted(results, key=lambda r: r.status_code)
        assert [r.status_code for r in by_status] == [200, 409], (
            f"expected one 200 and one 409, got {[r.status_code for r in results]}"
        )
        assert_problem(by_status[1], 409, "aborted", reason="NOT_LATEST_TURN")
        winner = parse_sse(by_status[0].text)
        new_rid = expect_stream_started(winner).data["request_id"]
        expect_done(winner)
        assert [(m["role"], m["request_id"]) for m in list_messages(chat_id)] == [
            ("user", new_rid), ("assistant", new_rid),
        ]

    @pytest.mark.timeout(30)
    def test_retry_while_retry_running_409_not_latest(self, request, chat, mock_provider):
        """A retry of the old turn while its first retry is streaming is 409 NOT_LATEST_TURN."""
        _require_offline(request)
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Retryable turn.")

        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))
        with OpenStream(f"{turn_url(chat_id, rid)}/retry", None) as first:
            first.read_until_started()
            assert_problem(retry(chat_id, rid), 409, "aborted", reason="NOT_LATEST_TURN")
            expect_done(first.drain())


# ---------------------------------------------------------------------------
# Tests: replaced_by_request_id tracking
# ---------------------------------------------------------------------------

class TestReplacedByRequestId:
    """After retry, the original turn's replaced_by_request_id points to the new turn."""

    @pytest.mark.timeout(30)
    def test_replaced_by_request_id_set(self, chat):
        chat_id = chat["id"]
        rid1 = complete_turn(chat_id, "Original turn.")

        resp = retry(chat_id, rid1)
        assert resp.status_code == 200, f"Retry failed: {resp.status_code} {resp.text}"
        retry_events = parse_sse(resp.text)
        ss = expect_stream_started(retry_events)
        assert ss.data.get("is_new_turn") is True
        new_rid = ss.data["request_id"]
        expect_done(retry_events)

        rows = query_db(
            "SELECT replaced_by_request_id FROM chat_turns WHERE request_id = ?",
            (rid1,),
        )
        assert len(rows) == 1, f"Turn {rid1} not found in DB"
        assert uuid_from_db(rows[0]["replaced_by_request_id"]) == new_rid


# ---------------------------------------------------------------------------
# Tests: mutation preflight (chat model, attachments)
# ---------------------------------------------------------------------------

def mutate(mutation: str, chat_id: str, rid: str) -> httpx.Response:
    """Retry the turn, or edit it to "Edited question."."""
    return {
        "retry": lambda: retry(chat_id, rid),
        "edit": lambda: edit(chat_id, rid, "Edited question."),
    }[mutation]()


def assert_mutation_rejected(chat_id: str, rid: str, before: list[dict], mock_provider) -> None:
    """The rejected mutation changed nothing: same messages, the turn is
    still the chat's only turn and `done`, and the provider was not called."""
    assert list_messages(chat_id) == before
    assert turn_count(chat_id) == 1
    assert poll_turn(chat_id, rid)["state"] == "done"
    assert mock_provider.get_captured_requests() == []


@pytest.mark.usefixtures("offline_only")
@pytest.mark.parametrize("mutation", ["retry", "edit", "delete"])
class TestMutationOtherRequester:
    """20-11: a turn of the caller's chat that another user started (a
    shared chat, not reachable through the API in P1; DB seed of
    `chat_turns.requester_user_id`) cannot be retried, edited or deleted:
    domain/service/turn_service.rs `validate_mutation` checks the requester."""

    @pytest.mark.timeout(30)
    def test_mutation_of_other_users_turn_403(self, chat, mock_provider, mutation):
        """403 permission_denied with reason AUTHZ_DENIED; nothing changes
        and the provider is not called."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Question.")
        before = list_messages(chat_id)
        assert exec_db(
            "UPDATE chat_turns SET requester_user_id = ? WHERE chat_id = ? AND request_id = ?",
            (USER_B_ID, chat_id, rid),
        ) == 1

        mock_provider.clear_captured_requests()
        if mutation == "delete":
            resp = httpx.delete(turn_url(chat_id, rid), timeout=10)
        else:
            resp = mutate(mutation, chat_id, rid)
        assert_problem(resp, 403, "permission_denied", reason="AUTHZ_DENIED")
        assert_mutation_rejected(chat_id, rid, before, mock_provider)


@pytest.mark.usefixtures("offline_only")
@pytest.mark.parametrize("mutation", ["retry", "edit"])
class TestMutationChatModelLeftCatalog:
    """Retry and edit of a chat whose model is no longer in the catalog (DB
    seed, as for a send: test_streaming.py TestChatModelLeftCatalog)."""

    @pytest.mark.timeout(30)
    def test_mutation_with_model_missing_from_catalog_400(self, chat, mock_provider, mutation):
        """400 invalid_argument INVALID_MODEL on `model` (chat resource),
        before the turn is replaced."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Question.")
        before = list_messages(chat_id)
        assert exec_db(
            "UPDATE chats SET model = ? WHERE id = ?", ("gpt-removed-from-catalog", chat_id),
        ) == 1

        mock_provider.clear_captured_requests()
        body = assert_problem(
            mutate(mutation, chat_id, rid), 400, "invalid_argument",
            field_reason="INVALID_MODEL", resource_type=RESOURCE_CHAT,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["model"], body
        assert_mutation_rejected(chat_id, rid, before, mock_provider)


@pytest.mark.usefixtures("offline_only")
@pytest.mark.parametrize("mutation", ["retry", "edit"])
class TestMutationAttachments:
    """The new user message of a retry or edit keeps the attachments of the
    one it replaces (turn_service.rs, `copy_for_retry`), and the image
    checks of a send run again before the turn is replaced
    (stream_service `preflight_mutation`)."""

    @pytest.mark.timeout(40)
    def test_mutation_copies_attachments_except_deleted(self, chat_with_model, mock_provider,
                                                        mutation):
        """A turn with two documents and an image; one document is then
        soft-deleted (DB seed of `deleted_at`: a referenced attachment
        cannot be deleted over the API). The new user message references the
        other document and the image, and the new provider request carries
        the image as `input_image` and `file_search` on the chat's store."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]  # vision, file_search
        keep = _upload_ready(chat_id, "keep.txt", b"Kept document.", "text/plain")
        gone = _upload_ready(chat_id, "gone.txt", b"Deleted document.", "text/plain")
        image = _upload_ready(chat_id, "red.png", make_minimal_png(), "image/png")
        rid = str(uuid.uuid4())
        status, events, raw = stream_message(
            chat_id, "Question.", request_id=rid, attachment_ids=[keep, gone, image],
        )
        assert status == 200, raw
        expect_done(events)
        poll_turn(chat_id, rid, ("done",))
        assert exec_db("UPDATE attachments SET deleted_at = created_at WHERE id = ?", (gone,)) == 1

        mock_provider.clear_captured_requests()
        resp = mutate(mutation, chat_id, rid)
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        new_rid = expect_stream_started(events).data["request_id"]
        expect_done(events)

        user = list_messages(chat_id)[0]
        assert (user["role"], user["request_id"]) == ("user", new_rid), user
        assert sorted(a["attachment_id"] for a in user["attachments"]) == sorted([keep, image]), user
        (req,) = mock_provider.get_captured_requests()
        text = {"retry": "Question.", "edit": "Edited question."}[mutation]
        assert [i for i in req["input"] if i.get("role") == "user"][-1]["content"] == [
            {"type": "input_text", "text": text},
            {"type": "input_image", "file_id": provider_file_id(image)},
        ]
        (store,) = query_db(
            "SELECT vector_store_id FROM chat_vector_stores WHERE chat_id = ?", (chat_id,),
        )
        assert [(t["type"], t["vector_store_ids"]) for t in req["tools"]] == [
            ("file_search", [store["vector_store_id"]]),
        ], req["tools"]

    @pytest.mark.timeout(30)
    def test_image_turn_on_model_without_vision_400(self, chat_with_model, mock_provider, mutation):
        """A turn with an image on gpt-5.2; the chat model is then switched
        to gpt-5-bare (no VISION_INPUT) in the DB: 400 invalid_argument
        VISION_NOT_SUPPORTED on `content_type` (attachment resource), as for
        a send (01-06), before the turn is replaced."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        image = _upload_ready(chat_id, "red.png", make_minimal_png(), "image/png")
        rid = str(uuid.uuid4())
        status, events, raw = stream_message(
            chat_id, "Question.", request_id=rid, attachment_ids=[image],
        )
        assert status == 200, raw
        expect_done(events)
        poll_turn(chat_id, rid, ("done",))
        before = list_messages(chat_id)
        assert exec_db("UPDATE chats SET model = ? WHERE id = ?", (BARE_MODEL, chat_id)) == 1

        mock_provider.clear_captured_requests()
        body = assert_problem(
            mutate(mutation, chat_id, rid), 400, "invalid_argument",
            field_reason="VISION_NOT_SUPPORTED", resource_type=RESOURCE_ATTACHMENT,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["content_type"], body
        assert_mutation_rejected(chat_id, rid, before, mock_provider)

    @pytest.mark.timeout(40)
    def test_more_images_than_allowed_400(self, chat, mock_provider, mutation):
        """A turn with `max_images_per_message` (4) images; a fifth image is
        then linked to its user message in the DB: 400 out_of_range
        TOO_MANY_IMAGES on `image_count` (attachment resource), as for a
        send (04-11), before the turn is replaced."""
        chat_id = chat["id"]  # vision-capable default model
        images = [
            _upload_ready(chat_id, f"img{i}.png", make_minimal_png(color=(i * 40, 0, 0)), "image/png")
            for i in range(MAX_IMAGES_PER_MESSAGE + 1)
        ]
        rid = str(uuid.uuid4())
        status, events, raw = stream_message(
            chat_id, "Question.", request_id=rid, attachment_ids=images[:-1],
        )
        assert status == 200, raw
        expect_done(events)
        poll_turn(chat_id, rid, ("done",))
        before = list_messages(chat_id)
        assert exec_db(
            "INSERT INTO message_attachments "
            "(tenant_id, chat_id, message_id, attachment_id, created_at) "
            "SELECT tenant_id, chat_id, message_id, ?, created_at FROM message_attachments "
            "WHERE message_id = ? LIMIT 1",
            (images[-1], before[0]["id"]),
        ) == 1
        before = list_messages(chat_id)

        mock_provider.clear_captured_requests()
        body = assert_problem(
            mutate(mutation, chat_id, rid), 400, "out_of_range",
            field_reason="TOO_MANY_IMAGES", resource_type=RESOURCE_ATTACHMENT,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["image_count"], body
        assert_mutation_rejected(chat_id, rid, before, mock_provider)
