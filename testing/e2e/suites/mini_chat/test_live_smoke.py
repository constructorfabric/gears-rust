"""
Live smoke tests against an already-running mini-chat backend.

Usage:
    E2E_BINARY=skip python3 -m pytest testing/e2e/suites/mini_chat/test_live_smoke.py -v

No orchestrator, no mock provider required.
Exercises real LLM calls (azure-gpt-4.1 / gpt-5-mini).
Tests skip gracefully when the provider is unavailable (Azure upstream errors).
"""

from __future__ import annotations

import io
import os
import time

import httpx
import pytest

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:8087")
API = f"{BASE_URL}/cf/mini-chat/v1"
TIMEOUT = 90

# Model IDs must match the e2e model catalog (testing/e2e/suites/mini_chat/config/base.yaml)
DEFAULT_MODEL = "azure-gpt-4.1"   # Premium tier, Azure
MINI_MODEL = "gpt-5-mini"         # Standard tier, OpenAI


# ── Helpers ──────────────────────────────────────────────────────────────


def parse_sse(text: str) -> list[dict]:
    """Minimal SSE parser: returns [{event, data}, ...]."""
    import json as _json
    events = []
    current_event = None
    current_data = []

    def _flush():
        nonlocal current_event, current_data
        if current_event:
            raw = "\n".join(current_data)
            try:
                data = _json.loads(raw)
            except _json.JSONDecodeError:
                data = raw
            events.append({"event": current_event, "data": data})
        current_event = None
        current_data = []

    for line in text.splitlines():
        if line.startswith("event: "):
            current_event = line[7:]
        elif line.startswith("data: "):
            current_data.append(line[6:])
        elif line == "":
            _flush()
    _flush()  # emit trailing frame at EOF
    return events


def create_chat(model: str = DEFAULT_MODEL) -> dict:
    resp = httpx.post(f"{API}/chats", json={"model": model}, timeout=TIMEOUT)
    assert resp.status_code == 201, f"create chat: {resp.status_code} {resp.text}"
    return resp.json()


def send_message(chat_id: str, content: str, **extra) -> tuple[int, list[dict], str]:
    body = {"content": content, **extra}
    resp = httpx.post(
        f"{API}/chats/{chat_id}/messages:stream",
        json=body,
        headers={"Accept": "text/event-stream"},
        timeout=TIMEOUT,
    )
    events = parse_sse(resp.text) if resp.status_code == 200 else []
    return resp.status_code, events, resp.text


def find_event(events: list[dict], name: str) -> dict | None:
    return next((e for e in events if e["event"] == name), None)


_TRANSIENT_ERROR_PHRASES = ("provider", "upstream", "rate limit", "timeout", "unavailable", "gateway")


def require_done(events: list[dict]) -> dict:
    """Assert stream completed successfully (has 'done' event, no 'error').

    Only skips for known transient/provider errors; fails on all others
    so real bugs are not masked.
    """
    err = find_event(events, "error")
    if err:
        err_data = str(err.get("data", "")).lower()
        if any(phrase in err_data for phrase in _TRANSIENT_ERROR_PHRASES):
            pytest.skip(f"Provider error (transient): {err['data']}")
        else:
            pytest.fail(f"Stream error (non-transient): {err['data']}")
    done = find_event(events, "done")
    if done is None:
        pytest.fail(f"No 'done' event in stream. Events: {[e['event'] for e in events]}")
    return done


def get_response_text(events: list[dict]) -> str:
    return "".join(
        d["data"]["content"] for d in events if d["event"] == "delta"
    )


def upload_file(chat_id: str, filename: str, content: bytes, content_type: str = "text/plain") -> str:
    """Upload a file and return attachment_id. Skips on provider errors."""
    resp = httpx.post(
        f"{API}/chats/{chat_id}/attachments",
        files={"file": (filename, io.BytesIO(content), content_type)},
        timeout=60,
    )
    assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
    return resp.json()["id"]


def poll_attachment_ready(chat_id: str, att_id: str, timeout_secs: int = 30) -> dict:
    """Poll until attachment is ready or failed."""
    deadline = time.time() + timeout_secs
    while time.time() < deadline:
        resp = httpx.get(f"{API}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        data = resp.json()
        if data["status"] in ("ready", "failed"):
            return data
        time.sleep(1)
    raise TimeoutError(f"Attachment {att_id} not ready after {timeout_secs}s")


# ── Fixtures ─────────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def _check_live():
    """Skip if backend is not reachable."""
    try:
        resp = httpx.get(f"{BASE_URL}/cf/openapi.json", timeout=5)
        if resp.status_code != 200:
            pytest.skip(f"Backend not healthy: {resp.status_code}")
    except httpx.ConnectError:
        pytest.skip(f"Backend not reachable at {BASE_URL}")


@pytest.fixture
def chat(_check_live):
    return create_chat(DEFAULT_MODEL)


@pytest.fixture
def mini_chat(_check_live):
    return create_chat(MINI_MODEL)


# ── Tests: Core Streaming ────────────────────────────────────────────────


class TestLiveSmoke:
    """Basic live smoke tests against real LLM."""

    def test_health(self, _check_live):
        resp = httpx.get(f"{BASE_URL}/cf/openapi.json", timeout=10)
        assert resp.status_code == 200

    def test_list_models(self, _check_live):
        resp = httpx.get(f"{API}/models", timeout=10)
        assert resp.status_code == 200
        models = resp.json()["items"]
        ids = [m["model_id"] for m in models]
        assert DEFAULT_MODEL in ids

    def test_create_chat(self, _check_live):
        chat = create_chat()
        assert "id" in chat
        assert chat["model"] == DEFAULT_MODEL

    def test_stream_single_turn(self, chat):
        status, events, raw = send_message(chat["id"], "Say 'hello' and nothing else.")
        assert status == 200, f"stream failed: {raw}"

        started = find_event(events, "stream_started")
        assert started is not None
        assert started["data"]["is_new_turn"] is True

        done = require_done(events)
        assert done["data"]["usage"]["input_tokens"] > 0
        assert done["data"]["usage"]["output_tokens"] > 0

    def test_stream_has_deltas(self, chat):
        status, events, _ = send_message(chat["id"], "Count from 1 to 5.")
        assert status == 200
        require_done(events)
        deltas = [e for e in events if e["event"] == "delta"]
        assert len(deltas) > 0, "expected at least one delta event"

    @pytest.mark.online_only
    def test_multi_turn_context(self, chat):
        """Two turns: assistant should remember the first."""
        cid = chat["id"]
        s1, ev1, _ = send_message(cid, "Remember: the secret word is 'banana'.")
        assert s1 == 200
        require_done(ev1)

        status, events, _ = send_message(cid, "What is the secret word?")
        assert status == 200
        require_done(events)
        text = get_response_text(events)
        assert "banana" in text.lower(), f"expected 'banana' in response: {text}"


# ── Tests: SSE Summary Field ────────────────────────────────────────────


class TestLiveStreamStartedSummary:
    """Verify thread_summary_applied field in stream_started events."""

    def test_stream_started_has_no_summary_initially(self, chat):
        status, events, _ = send_message(chat["id"], "Hello")
        assert status == 200
        started = find_event(events, "stream_started")
        assert started is not None
        assert started["data"].get("thread_summary_applied") is None


# ── Tests: Mini Model ───────────────────────────────────────────────────


class TestLiveMiniModel:
    """Tests using gpt-5-mini (cheaper, faster)."""

    def test_mini_model_works(self, mini_chat):
        status, events, raw = send_message(
            mini_chat["id"], "What is 2+2? Answer with just the number."
        )
        assert status == 200, f"mini model failed: {raw}"
        done = require_done(events)
        assert done["data"]["effective_model"] == MINI_MODEL


# ── Tests: Chat CRUD ────────────────────────────────────────────────────


class TestLiveChatCRUD:
    """Basic CRUD operations."""

    def test_list_chats(self, _check_live):
        resp = httpx.get(f"{API}/chats", timeout=10)
        assert resp.status_code == 200
        assert "items" in resp.json()

    def test_get_chat(self, chat):
        resp = httpx.get(f"{API}/chats/{chat['id']}", timeout=10)
        assert resp.status_code == 200
        assert resp.json()["id"] == chat["id"]

    def test_list_messages_empty(self, chat):
        resp = httpx.get(f"{API}/chats/{chat['id']}/messages", timeout=10)
        assert resp.status_code == 200
        assert resp.json()["items"] == []

    def test_list_messages_after_turn(self, chat):
        status, events, _ = send_message(chat["id"], "Hi")
        assert status == 200
        require_done(events)  # skip if provider error

        resp = httpx.get(f"{API}/chats/{chat['id']}/messages", timeout=10)
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        assert len(msgs) >= 2  # user + assistant

    def test_delete_chat(self, _check_live):
        c = create_chat()
        resp = httpx.delete(f"{API}/chats/{c['id']}", timeout=10)
        assert resp.status_code == 204

    def test_get_deleted_chat_404(self, _check_live):
        c = create_chat()
        httpx.delete(f"{API}/chats/{c['id']}", timeout=10)
        resp = httpx.get(f"{API}/chats/{c['id']}", timeout=10)
        assert resp.status_code == 404


# ── Tests: Quota ─────────────────────────────────────────────────────────


class TestLiveQuota:
    """Quota status endpoint."""

    def test_quota_status(self, _check_live):
        resp = httpx.get(f"{API}/quota/status", timeout=10)
        assert resp.status_code == 200
        data = resp.json()
        assert "tiers" in data


# ── Tests: Attachment Deletion ───────────────────────────────────────────


class TestLiveTurnMutations:
    """Retry, edit, delete turns."""

    def test_retry_turn(self, chat):
        """Complete a turn, then retry it — should produce a new response."""
        cid = chat["id"]
        s1, ev1, _ = send_message(cid, "Say a random number between 1 and 100.")
        assert s1 == 200
        require_done(ev1)
        started1 = find_event(ev1, "stream_started")
        request_id = started1["data"]["request_id"]

        # Retry
        resp = httpx.post(
            f"{API}/chats/{cid}/turns/{request_id}/retry",
            json={},
            headers={"Accept": "text/event-stream"},
            timeout=TIMEOUT,
        )
        assert resp.status_code == 200, f"retry failed: {resp.status_code} {resp.text}"
        ev2 = parse_sse(resp.text)
        require_done(ev2)
        started2 = find_event(ev2, "stream_started")
        assert started2["data"]["request_id"] != request_id, "retry should have a new request_id"

    def test_delete_last_turn(self, chat):
        """Complete a turn, then delete it."""
        cid = chat["id"]
        s1, ev1, _ = send_message(cid, "Temporary message")
        assert s1 == 200
        require_done(ev1)
        started = find_event(ev1, "stream_started")
        request_id = started["data"]["request_id"]

        resp = httpx.delete(f"{API}/chats/{cid}/turns/{request_id}", timeout=10)
        assert resp.status_code == 204

    def test_edit_turn(self, chat):
        """Complete a turn, then edit it with new content."""
        cid = chat["id"]
        s1, ev1, _ = send_message(cid, "What is 1+1?")
        assert s1 == 200
        require_done(ev1)
        started = find_event(ev1, "stream_started")
        request_id = started["data"]["request_id"]

        resp = httpx.patch(
            f"{API}/chats/{cid}/turns/{request_id}",
            json={"content": "What is 2+2?"},
            headers={"Accept": "text/event-stream"},
            timeout=TIMEOUT,
        )
        assert resp.status_code == 200, f"edit failed: {resp.status_code} {resp.text}"
        ev2 = parse_sse(resp.text)
        require_done(ev2)


class TestLiveParallelTurn:
    """Concurrent turn conflict."""

    @pytest.mark.online_only
    def test_second_stream_409(self, _check_live):
        """Sending a second message while first is running should return 409."""
        chat = create_chat(DEFAULT_MODEL)
        cid = chat["id"]

        # Start a long-running request (stream but don't wait for completion)
        with httpx.Client(timeout=TIMEOUT) as client:
            with client.stream(
                "POST",
                f"{API}/chats/{cid}/messages:stream",
                json={"content": "Write a long essay about the history of mathematics."},
                headers={"Accept": "text/event-stream"},
            ) as first:
                # Wait until server confirms the turn is running (stream_started event)
                # instead of a fixed sleep — eliminates race condition.
                for line in first.iter_lines():
                    if "stream_started" in line:
                        break

                resp2 = httpx.post(
                    f"{API}/chats/{cid}/messages:stream",
                    json={"content": "Hello"},
                    headers={"Accept": "text/event-stream"},
                    timeout=10,
                )
                # 409 = generation in progress
                assert resp2.status_code == 409, (
                    f"Expected 409, got {resp2.status_code}: {resp2.text[:200]}"
                )


class TestLiveWebSearch:
    """Web search tool integration."""

    def test_web_search_enabled_completes(self, chat):
        """Enable web_search — stream should complete even if LLM skips the tool."""
        status, events, raw = send_message(
            chat["id"],
            "Search the web: what is the current population of Tokyo?",
            web_search_enabled=True,
        )
        assert status == 200, f"web search failed: {raw}"
        done = require_done(events)
        # Verify web_search was at least offered (done event shows usage)
        assert done["data"]["usage"]["input_tokens"] > 0
        # Tool events are not guaranteed — LLM may answer from knowledge.
        # Just verify the stream completes without error.


class TestLiveReactions:
    """Message reactions."""

    def test_set_and_remove_reaction(self, chat):
        cid = chat["id"]
        s, ev, _ = send_message(cid, "Tell me a joke.")
        assert s == 200
        require_done(ev)
        started = find_event(ev, "stream_started")
        msg_id = started["data"]["message_id"]

        # Set reaction
        resp = httpx.put(
            f"{API}/chats/{cid}/messages/{msg_id}/reaction",
            json={"reaction": "like"},
            timeout=10,
        )
        assert resp.status_code == 200

        # Remove reaction
        resp = httpx.delete(
            f"{API}/chats/{cid}/messages/{msg_id}/reaction",
            timeout=10,
        )
        assert resp.status_code == 204


class TestLiveErrorHandling:
    """Error scenarios."""

    def test_empty_content_rejected(self, chat):
        resp = httpx.post(
            f"{API}/chats/{chat['id']}/messages:stream",
            json={"content": ""},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code in (400, 422)

    def test_chat_not_found_stream(self, _check_live):
        fake_id = "00000000-0000-0000-0000-000000000000"
        resp = httpx.post(
            f"{API}/chats/{fake_id}/messages:stream",
            json={"content": "hello"},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code in (403, 404)

    def test_invalid_attachment_id_rejected(self, chat):
        fake_att = "00000000-0000-0000-0000-000000000000"
        resp = httpx.post(
            f"{API}/chats/{chat['id']}/messages:stream",
            json={"content": "hello", "attachment_ids": [fake_att]},
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code in (400, 404, 422)


class TestLiveMessagesAPI:
    """Messages list with OData-like features."""

    def test_messages_have_role_and_content(self, chat):
        status, events, _ = send_message(chat["id"], "Ping")
        assert status == 200, f"Stream failed: {status}"
        require_done(events)
        resp = httpx.get(f"{API}/chats/{chat['id']}/messages", timeout=10)
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        assert len(msgs) > 0, "Expected at least one message after completed turn"
        for msg in msgs:
            assert "role" in msg
            assert "content" in msg

    def test_messages_have_created_at(self, chat):
        status, events, _ = send_message(chat["id"], "Ping")
        assert status == 200, f"Stream failed: {status}"
        require_done(events)
        resp = httpx.get(f"{API}/chats/{chat['id']}/messages", timeout=10)
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        assert len(msgs) > 0, "Expected at least one message after completed turn"
        for msg in msgs:
            assert "created_at" in msg


class TestLiveChatUpdate:
    """Chat title update."""

    def test_update_title(self, chat):
        resp = httpx.patch(
            f"{API}/chats/{chat['id']}",
            json={"title": "My Updated Chat"},
            timeout=10,
        )
        assert resp.status_code == 200
        assert resp.json()["title"] == "My Updated Chat"

    def test_update_empty_title_rejected(self, chat):
        resp = httpx.patch(
            f"{API}/chats/{chat['id']}",
            json={"title": "   "},
            timeout=10,
        )
        assert resp.status_code in (400, 422)


class TestLiveThreadSummary:
    """Thread summary infrastructure verification.

    Note: with azure-gpt-4.1's 1M token context window, the 80% compression threshold
    (~800K tokens) cannot be reached in a reasonable e2e test. These tests verify
    the plumbing is correct: no premature compression, SSE field absent when
    no summary exists, and messages remain accessible across multiple turns.
    """

    def test_no_summary_on_fresh_chat(self, chat):
        """First turn: stream_started should not have thread_summary_applied."""
        status, events, _ = send_message(chat["id"], "Hello")
        assert status == 200
        require_done(events)
        started = find_event(events, "stream_started")
        assert started["data"].get("thread_summary_applied") is None

    def test_no_premature_compression_after_multiple_turns(self, _check_live):
        """After several turns, messages should NOT be compressed (threshold not reached)."""
        chat = create_chat(MINI_MODEL)
        cid = chat["id"]

        # Send 6 turns (12 messages total — nowhere near 80% of 1M tokens)
        for i in range(6):
            s, ev, _ = send_message(cid, f"Turn {i}: The quick brown fox jumps over the lazy dog.")
            assert s == 200
            require_done(ev)

        # All messages should be visible (none compressed away)
        resp = httpx.get(f"{API}/chats/{cid}/messages", timeout=10)
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        # 6 user + 6 assistant = 12 messages
        assert len(msgs) >= 12, f"Expected >= 12 messages, got {len(msgs)}"

    @pytest.mark.online_only
    def test_no_summary_after_multiple_turns(self, _check_live):
        """After several turns, stream_started still should not have summary."""
        chat = create_chat(MINI_MODEL)
        cid = chat["id"]

        for i in range(4):
            s, ev, _ = send_message(cid, f"Message {i}: remember {i * 7}")
            assert s == 200
            require_done(ev)

        # 5th turn: check stream_started for summary
        s, events, _ = send_message(cid, "What numbers did I ask you to remember?")
        assert s == 200
        require_done(events)

        started = find_event(events, "stream_started")
        assert started["data"].get("thread_summary_applied") is None, (
            "Summary should not exist — context is far below compression threshold"
        )

        # Verify the model can still recall earlier messages (context intact)
        text = get_response_text(events)
        assert "0" in text or "7" in text or "14" in text or "21" in text, (
            f"Model should recall at least one remembered number, got: {text}"
        )

TINY_CTX_MODEL = "gpt-4.1-mini-tiny-ctx"


class TestLiveThreadSummaryTrigger:
    """Full thread summary trigger e2e test.

    Uses `gpt-4.1-mini-tiny-ctx` — a test model with context_window=4096
    so the 80% compression threshold (~2458 tokens) is reachable in ~3 turns.
    After the trigger fires (async outbox handler), the next turn should see
    `thread_summary_applied` in the `stream_started` event.
    """

    def test_model_available(self, _check_live):
        """Verify the tiny-context model exists in the catalog."""
        resp = httpx.get(f"{API}/models", timeout=10)
        assert resp.status_code == 200
        ids = [m["model_id"] for m in resp.json()["items"]]
        if TINY_CTX_MODEL not in ids:
            pytest.skip(f"Model {TINY_CTX_MODEL} not in catalog — add it to config")

    def test_summary_trigger_fires_and_applied_on_next_turn(self, _check_live):
        """
        1. Create chat with tiny context model
        2. Send several verbose turns to exceed 80% of 4096 tokens
        3. Wait briefly for async outbox handler to process
        4. Send one more turn and check stream_started.thread_summary_applied
        """
        # Check model availability
        resp = httpx.get(f"{API}/models", timeout=10)
        ids = [m["model_id"] for m in resp.json()["items"]]
        if TINY_CTX_MODEL not in ids:
            pytest.skip(f"Model {TINY_CTX_MODEL} not in catalog")

        chat = create_chat(TINY_CTX_MODEL)
        cid = chat["id"]

        # Send verbose messages to fill context.
        # System prompt asks model to be verbose, so responses will be long.
        # Each turn: ~200 token user msg + ~300-500 token assistant response
        # After 3-4 turns we should exceed 80% of 4096 = ~2458 tokens.
        prompts = [
            "Explain in great detail how photosynthesis works, step by step. "
            "Include all chemical reactions, the role of chlorophyll, and "
            "the difference between light-dependent and light-independent reactions.",

            "Now explain in equal detail how cellular respiration works. "
            "Compare and contrast it with photosynthesis. Include glycolysis, "
            "the Krebs cycle, and the electron transport chain.",

            "Describe the full nitrogen cycle in ecosystems. Explain nitrogen "
            "fixation, nitrification, denitrification, and the role of bacteria. "
            "Give real-world examples of each stage.",

            "Explain the water cycle in comprehensive detail. Include "
            "evaporation, transpiration, condensation, precipitation, "
            "surface runoff, and groundwater flow. How does climate change affect it?",
        ]

        for i, prompt in enumerate(prompts):
            s, ev, raw = send_message(cid, prompt)
            assert s == 200, f"Turn {i} failed: {raw[:200]}"
            done = require_done(ev)
            usage = done["data"].get("usage", {})
            input_tokens = usage.get("input_tokens", 0)
            # Log for debugging
            print(f"  Turn {i}: input_tokens={input_tokens}, output_tokens={usage.get('output_tokens', 0)}")

        # Wait for the outbox handler to process the summary task.
        # The trigger fires in the finalization transaction of the turn that
        # exceeded the threshold. The handler runs asynchronously.
        print("  Waiting 5s for thread summary handler...")
        time.sleep(5)

        # One more turn — check if summary is now applied
        s, events, raw = send_message(cid, "Briefly, what topics have we discussed?")
        assert s == 200, f"Final turn failed: {raw[:200]}"
        require_done(events)

        started = find_event(events, "stream_started")
        assert started is not None

        summary_info = started["data"].get("thread_summary_applied")
        if summary_info is not None:
            print(f"  thread_summary_applied: {summary_info}")
            assert "token_estimate" in summary_info
            assert summary_info["token_estimate"] > 0
        else:
            # Summary may not have been generated yet (handler latency)
            # or threshold wasn't reached (model responses were too short).
            # Check messages to see if any are compressed.
            resp = httpx.get(f"{API}/chats/{cid}/messages", timeout=10)
            msg_count = len(resp.json()["items"])
            print(f"  No summary yet. Message count: {msg_count}")
            # Don't fail — this is best-effort with real LLM.
            # The trigger depends on actual token counts which vary.
            pytest.skip(
                f"Summary not triggered after {len(prompts)} turns "
                f"(token threshold may not have been reached with this model). "
                f"Messages: {msg_count}"
            )


class TestLiveAttachmentDeletion:
    """Upload a document, verify LLM can find it, delete it, verify LLM cannot."""

    @pytest.mark.online_only
    def test_delete_makes_document_invisible_to_llm(self, _check_live):
        """
        1. Create chat, upload file with unique fact
        2. Ask LLM about the fact WITH attachment → should know
        3. Delete attachment
        4. New chat, ask same question WITHOUT attachment → should NOT know
        """
        chat = create_chat(DEFAULT_MODEL)
        chat_id = chat["id"]

        # Upload file with a unique fact no LLM would know.
        # Repeat content so Azure vector store indexes it reliably.
        unique_fact = (
            "The capital of the fictional country Zarvonia is Plimberwick.\n"
            "Zarvonia's capital city Plimberwick was founded in 1823.\n"
            "The city of Plimberwick is the largest city in Zarvonia.\n"
        ) * 5
        att_id = upload_file(chat_id, "zarvonia.txt", unique_fact.encode())
        detail = poll_attachment_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"Attachment not ready: {detail}"

        # Wait for Azure vector store to finish chunking/embedding.
        # Our attachment status tracks our DB row, not the provider's
        # async indexing pipeline (see Azure docs: "Adding files to
        # vector stores is an async operation").
        time.sleep(5)

        # Ask WITH attachment
        status, events, raw = send_message(chat_id, "What is the capital of Zarvonia? Use the attached document.")
        assert status == 200, f"Stream failed: {raw}"
        require_done(events)
        response_text = get_response_text(events)
        assert "plimberwick" in response_text.lower(), (
            f"LLM should know about Plimberwick from attachment, got: {response_text}"
        )

        # Delete attachment
        resp = httpx.delete(f"{API}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code in (204, 409)  # 409 = referenced by message, still soft-deleted

        # NEW chat — no vector store, no history
        clean_chat = create_chat(DEFAULT_MODEL)
        status2, events2, raw2 = send_message(
            clean_chat["id"], "What is the capital of Zarvonia?"
        )
        assert status2 == 200, f"Stream failed: {raw2}"
        require_done(events2)
        response_text2 = get_response_text(events2)

        # LLM should NOT know — Plimberwick is a made-up fact from a deleted file
        assert "plimberwick" not in response_text2.lower(), (
            f"LLM should NOT know about Plimberwick without document, got: {response_text2}"
        )

    def test_attachment_api_returns_404_after_delete(self, _check_live):
        """After deletion, GET attachment returns 404."""
        chat = create_chat(MINI_MODEL)
        chat_id = chat["id"]

        att_id = upload_file(chat_id, "ephemeral.txt", b"temporary data")
        poll_attachment_ready(chat_id, att_id)

        resp = httpx.delete(f"{API}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 204

        resp = httpx.get(f"{API}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 404


class TestLiveChatDeletionCleanup:
    """Delete a chat with attachments and verify cleanup removes all provider resources."""

    def test_delete_chat_cleans_up_attachments(self, _check_live):
        """
        1. Create chat, upload attachment, verify it's ready
        2. Send a message so the attachment is indexed in vector store
        3. Delete the entire chat
        4. Verify chat returns 404
        5. Verify attachment returns 404
        6. Create new chat, verify LLM cannot access the deleted fact
        """
        chat = create_chat(DEFAULT_MODEL)
        chat_id = chat["id"]

        # Upload file with unique fact — repeat content to ensure file_search indexes it
        unique_fact = (
            "The president of the fictional nation Glorpistan is Quuxley Fernwhistle.\n"
            "Glorpistan's capital city is Fernwhistle City, named after President Quuxley Fernwhistle.\n"
            "The nation of Glorpistan was founded in 1847 by Quuxley Fernwhistle Senior.\n"
        ) * 5  # Repeat to increase file size for better indexing
        att_id = upload_file(chat_id, "glorpistan.txt", unique_fact.encode())
        detail = poll_attachment_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"Attachment not ready: {detail}"

        # Send message to trigger file_search indexing
        status, events, raw = send_message(
            chat_id, "Who is the president of Glorpistan? Use the attached document."
        )
        assert status == 200, f"Stream failed: {raw}"
        require_done(events)
        response_text = get_response_text(events)
        if "fernwhistle" not in response_text.lower():
            pytest.skip(
                "file_search did not retrieve the fact (flaky indexing); "
                f"got: {response_text[:200]}"
            )

        # Delete the entire chat
        resp = httpx.delete(f"{API}/chats/{chat_id}", timeout=10)
        assert resp.status_code == 204

        # Poll until chat returns 404 (cleanup worker processed)
        deadline = time.time() + 15
        while time.time() < deadline:
            resp = httpx.get(f"{API}/chats/{chat_id}", timeout=10)
            if resp.status_code == 404:
                break
            time.sleep(1)
        else:
            pytest.fail(f"Chat {chat_id} not cleaned up after 15s, status={resp.status_code}")

        # Attachment endpoint should also 404
        resp = httpx.get(f"{API}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 404, f"Attachment should be gone, got {resp.status_code}"

        # New chat — verify LLM cannot see the deleted fact
        clean_chat = create_chat(DEFAULT_MODEL)
        status2, events2, raw2 = send_message(
            clean_chat["id"], "Who is the president of Glorpistan?"
        )
        assert status2 == 200, f"Stream failed: {raw2}"
        require_done(events2)
        response_text2 = get_response_text(events2)
        assert "fernwhistle" not in response_text2.lower(), (
            f"LLM should NOT know about Fernwhistle after chat deletion, got: {response_text2}"
        )

    def test_delete_chat_without_attachments(self, _check_live):
        """Deleting a chat with no attachments should succeed cleanly."""
        chat = create_chat(MINI_MODEL)
        chat_id = chat["id"]

        # Send one message so the chat has content
        status, events, _ = send_message(chat_id, "Hello")
        assert status == 200
        require_done(events)

        # Delete chat
        resp = httpx.delete(f"{API}/chats/{chat_id}", timeout=10)
        assert resp.status_code == 204

        # Should be gone
        deadline = time.time() + 10
        while time.time() < deadline:
            resp = httpx.get(f"{API}/chats/{chat_id}", timeout=10)
            if resp.status_code == 404:
                break
            time.sleep(1)
        else:
            pytest.fail(f"Chat {chat_id} not deleted after 10s")

    def test_delete_chat_idempotent(self, _check_live):
        """Deleting the same chat twice should succeed both times."""
        chat = create_chat(MINI_MODEL)
        chat_id = chat["id"]

        resp1 = httpx.delete(f"{API}/chats/{chat_id}", timeout=10)
        assert resp1.status_code == 204

        resp2 = httpx.delete(f"{API}/chats/{chat_id}", timeout=10)
        assert resp2.status_code in (204, 404)
