"""Tests verifying context assembly sends conversation history to the LLM.

These tests validate that the context assembly pipeline (system prompt,
recent messages, tools) works correctly end-to-end by observing:
- Input token growth across turns (proves history is sent)
- Model ability to recall earlier conversation content
- Correct behavior with multi-turn context
"""

import uuid

import httpx

from .conftest import API_PREFIX, PROVIDER_DEFAULT_MODEL, parse_sse, expect_done, expect_stream_started

import pytest


@pytest.mark.multi_provider
class TestSystemPrompt:
    """Verify system prompt is delivered to the LLM.

    The test config sets: 'When the user says exactly PING, respond with exactly PONG.'
    If the system prompt is missing, the model has no reason to reply 'PONG'.
    """

    def test_ping_pong_proves_system_prompt(self, provider_chat):
        """Send 'PING' — model must reply 'PONG' per system prompt rule."""
        _resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/messages:stream",
            json={"content": "PING"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        events = parse_sse(_resp.text) if _resp.status_code == 200 else []
        expect_done(events)

        text = "".join(e.data["content"] for e in events if e.event == "delta")
        assert "PONG" in text.upper(), (
            f"System prompt instructs model to reply 'PONG' to 'PING'. Got: {text!r}"
        )

    @pytest.mark.multi_provider
    def test_ping_pong_across_models(self, chat_with_model):
        """System prompt rule works for both premium and standard models."""
        for model in PROVIDER_DEFAULT_MODEL.values():
            chat = chat_with_model(model)
            _resp = httpx.post(
                f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
                json={"content": "PING"},
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )
            events = parse_sse(_resp.text) if _resp.status_code == 200 else []
            expect_done(events)

            text = "".join(e.data["content"] for e in events if e.event == "delta")
            assert "PONG" in text.upper(), (
                f"[{model}] System prompt should make model reply 'PONG'. Got: {text!r}"
            )


@pytest.mark.multi_provider
class TestContextInputTokenGrowth:
    """Input tokens should increase as conversation context grows."""

    def test_input_tokens_increase_with_turns(self, provider_chat):
        """Turn 2 input_tokens > turn 1 input_tokens, proving history is sent."""
        chat_id = provider_chat["id"]

        # Turn 1
        _r1 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What is the capital of France?"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        ev1 = parse_sse(_r1.text) if _r1.status_code == 200 else []
        done1 = expect_done(ev1)
        assert "effective_model" in done1.data, "done must have effective_model"
        assert "selected_model" in done1.data, "done must have selected_model"
        assert done1.data.get("quota_decision") in ("allow", "downgrade"), f"unexpected quota_decision: {done1.data.get('quota_decision')}"
        assert done1.data.get("usage", {}).get("output_tokens", 0) > 0, "done usage must have output_tokens > 0"
        input_tokens_1 = done1.data["usage"]["input_tokens"]

        # Turn 2 — context now includes turn 1 (user + assistant)
        _r2 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "And what about Germany?"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        ev2 = parse_sse(_r2.text) if _r2.status_code == 200 else []
        done2 = expect_done(ev2)
        assert "effective_model" in done2.data, "done must have effective_model"
        assert "selected_model" in done2.data, "done must have selected_model"
        assert done2.data.get("quota_decision") in ("allow", "downgrade"), f"unexpected quota_decision: {done2.data.get('quota_decision')}"
        assert done2.data.get("usage", {}).get("output_tokens", 0) > 0, "done usage must have output_tokens > 0"
        input_tokens_2 = done2.data["usage"]["input_tokens"]

        # Turn 2 must have strictly more input tokens (it includes turn 1 history)
        assert input_tokens_2 > input_tokens_1, (
            f"Turn 2 input_tokens ({input_tokens_2}) should be greater than "
            f"turn 1 ({input_tokens_1}) because conversation history is included"
        )

    def test_input_tokens_grow_over_three_turns(self, provider_chat):
        """Input tokens should monotonically increase across 3 turns."""
        chat_id = provider_chat["id"]
        input_tokens = []

        prompts = [
            "Name a fruit that starts with A.",
            "Name one that starts with B.",
            "Name one that starts with C.",
        ]

        for prompt in prompts:
            _r = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/messages:stream",
                json={"content": prompt},
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )
            assert _r.status_code == 200
            events = parse_sse(_r.text)
            done = expect_done(events)
            assert "effective_model" in done.data, "done must have effective_model"
            assert "selected_model" in done.data, "done must have selected_model"
            assert done.data.get("quota_decision") in ("allow", "downgrade"), f"unexpected quota_decision: {done.data.get('quota_decision')}"
            assert done.data.get("usage", {}).get("output_tokens", 0) > 0, "done usage must have output_tokens > 0"
            input_tokens.append(done.data["usage"]["input_tokens"])

        # Each turn should have more input tokens than the previous
        for i in range(1, len(input_tokens)):
            assert input_tokens[i] > input_tokens[i - 1], (
                f"Turn {i + 1} input_tokens ({input_tokens[i]}) should be greater than "
                f"turn {i} ({input_tokens[i - 1]})"
            )


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestContextRecall:
    """Model should recall information from earlier turns, proving context is sent."""

    def test_recall_specific_number(self, provider_chat):
        """Model must recall a specific number from an earlier turn."""
        chat_id = provider_chat["id"]

        # Turn 1: tell the model a specific fact
        _r1 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Remember this number: 73921. Just confirm you got it."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        s1 = _r1.status_code
        ev1 = parse_sse(_r1.text) if s1 == 200 else []
        assert s1 == 200
        expect_done(ev1)

        # Turn 2: ask it to recall
        _r2 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What was the number I told you to remember? Reply with just the number."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        s2 = _r2.status_code
        ev2 = parse_sse(_r2.text) if s2 == 200 else []
        assert s2 == 200
        expect_done(ev2)

        text = "".join(e.data["content"] for e in ev2 if e.event == "delta")
        assert "73921" in text, (
            f"Model should recall '73921' from conversation history. Got: {text!r}"
        )

    def test_recall_after_intervening_turn(self, provider_chat):
        """Model recalls info from turn 1 even after an unrelated turn 2."""
        chat_id = provider_chat["id"]

        # Turn 1: establish a fact
        _r1 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "The secret word is PELICAN. Acknowledge it."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert _r1.status_code == 200
        expect_done(parse_sse(_r1.text))

        # Turn 2: unrelated topic
        _r2 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What is 5 + 3? Reply with just the number."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert _r2.status_code == 200
        expect_done(parse_sse(_r2.text))

        # Turn 3: recall turn 1
        _r3 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What was the secret word I told you earlier? Reply with just the word."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        ev3 = parse_sse(_r3.text) if _r3.status_code == 200 else []
        expect_done(ev3)

        text = "".join(e.data["content"] for e in ev3 if e.event == "delta")
        assert "PELICAN" in text.upper(), (
            f"Model should recall 'PELICAN' from turn 1. Got: {text!r}"
        )


@pytest.mark.multi_provider
class TestContextMessageTokens:
    """Verify input_tokens reflect growing context."""

    def test_message_input_tokens_increase(self, provider_chat):
        """Assistant message input_tokens should grow with each turn."""
        chat_id = provider_chat["id"]

        # 3 turns
        for prompt in ["Say A.", "Say B.", "Say C."]:
            _r = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/messages:stream",
                json={"content": prompt},
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )
            _events = parse_sse(_r.text) if _r.status_code == 200 else []
            expect_done(_events)

        # Fetch assistant messages via REST API
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        asst_msgs = [m for m in msgs if m["role"] == "assistant"]

        assert len(asst_msgs) == 3
        tokens = [m["input_tokens"] for m in asst_msgs]

        # Each subsequent assistant message should have more input tokens
        for i in range(1, len(tokens)):
            assert tokens[i] > tokens[i - 1], (
                f"Assistant msg {i + 1} input_tokens ({tokens[i]}) should be > "
                f"msg {i} ({tokens[i - 1]}). All: {tokens}"
            )
