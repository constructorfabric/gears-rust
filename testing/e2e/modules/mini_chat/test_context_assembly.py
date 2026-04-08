"""Tests verifying context assembly sends conversation history to the LLM.

These tests validate that the context assembly pipeline (system prompt,
recent messages, tools) works correctly end-to-end by observing:
- Input token growth across turns (proves history is sent)
- Model ability to recall earlier conversation content
- Correct behavior with multi-turn context
"""

import uuid

import httpx

from .conftest import API_PREFIX, expect_done, stream_message

import pytest
pytestmark = pytest.mark.openai


class TestSystemPrompt:
    """Verify system prompt is delivered to the LLM.

    The test config sets: 'When the user says exactly PING, respond with exactly PONG.'
    If the system prompt is missing, the model has no reason to reply 'PONG'.
    """

    def test_ping_pong_proves_system_prompt(self, chat):
        """Send 'PING' — model must reply 'PONG' per system prompt rule."""
        _, events, _ = stream_message(chat["id"], "PING")
        expect_done(events)

        text = "".join(e.data["content"] for e in events if e.event == "delta")
        assert "PONG" in text.upper(), (
            f"System prompt instructs model to reply 'PONG' to 'PING'. Got: {text!r}"
        )

    @pytest.mark.multi_provider
    def test_ping_pong_across_models(self, chat_with_model):
        """System prompt rule works for both premium and standard models."""
        for model in ["gpt-5.2", "gpt-5-mini"]:
            chat = chat_with_model(model)
            _, events, _ = stream_message(chat["id"], "PING")
            expect_done(events)

            text = "".join(e.data["content"] for e in events if e.event == "delta")
            assert "PONG" in text.upper(), (
                f"[{model}] System prompt should make model reply 'PONG'. Got: {text!r}"
            )


class TestContextInputTokenGrowth:
    """Input tokens should increase as conversation context grows."""

    def test_input_tokens_increase_with_turns(self, chat):
        """Turn 2 input_tokens > turn 1 input_tokens, proving history is sent."""
        chat_id = chat["id"]

        # Turn 1
        _, ev1, _ = stream_message(chat_id, "What is the capital of France?")
        done1 = expect_done(ev1)
        input_tokens_1 = done1.data["usage"]["input_tokens"]

        # Turn 2 — context now includes turn 1 (user + assistant)
        _, ev2, _ = stream_message(chat_id, "And what about Germany?")
        done2 = expect_done(ev2)
        input_tokens_2 = done2.data["usage"]["input_tokens"]

        # Turn 2 must have strictly more input tokens (it includes turn 1 history)
        assert input_tokens_2 > input_tokens_1, (
            f"Turn 2 input_tokens ({input_tokens_2}) should be greater than "
            f"turn 1 ({input_tokens_1}) because conversation history is included"
        )

    def test_input_tokens_grow_over_three_turns(self, chat):
        """Input tokens should monotonically increase across 3 turns."""
        chat_id = chat["id"]
        input_tokens = []

        prompts = [
            "Name a fruit that starts with A.",
            "Name one that starts with B.",
            "Name one that starts with C.",
        ]

        for prompt in prompts:
            _, events, _ = stream_message(chat_id, prompt)
            done = expect_done(events)
            input_tokens.append(done.data["usage"]["input_tokens"])

        # Each turn should have more input tokens than the previous
        for i in range(1, len(input_tokens)):
            assert input_tokens[i] > input_tokens[i - 1], (
                f"Turn {i + 1} input_tokens ({input_tokens[i]}) should be greater than "
                f"turn {i} ({input_tokens[i - 1]})"
            )


@pytest.mark.online_only
class TestContextRecall:
    """Model should recall information from earlier turns, proving context is sent."""

    def test_recall_specific_number(self, chat):
        """Model must recall a specific number from an earlier turn."""
        chat_id = chat["id"]

        # Turn 1: tell the model a specific fact
        s1, ev1, _ = stream_message(
            chat_id, "Remember this number: 73921. Just confirm you got it."
        )
        assert s1 == 200
        expect_done(ev1)

        # Turn 2: ask it to recall
        s2, ev2, _ = stream_message(
            chat_id, "What was the number I told you to remember? Reply with just the number."
        )
        assert s2 == 200
        expect_done(ev2)

        text = "".join(e.data["content"] for e in ev2 if e.event == "delta")
        assert "73921" in text, (
            f"Model should recall '73921' from conversation history. Got: {text!r}"
        )

    def test_recall_after_intervening_turn(self, chat):
        """Model recalls info from turn 1 even after an unrelated turn 2."""
        chat_id = chat["id"]

        # Turn 1: establish a fact
        stream_message(chat_id, "The secret word is PELICAN. Acknowledge it.")

        # Turn 2: unrelated topic
        stream_message(chat_id, "What is 5 + 3? Reply with just the number.")

        # Turn 3: recall turn 1
        _, ev3, _ = stream_message(
            chat_id, "What was the secret word I told you earlier? Reply with just the word."
        )
        expect_done(ev3)

        text = "".join(e.data["content"] for e in ev3 if e.event == "delta")
        assert "PELICAN" in text.upper(), (
            f"Model should recall 'PELICAN' from turn 1. Got: {text!r}"
        )


class TestContextMessageTokens:
    """Verify input_tokens reflect growing context."""

    def test_message_input_tokens_increase(self, server):
        """Assistant message input_tokens should grow with each turn."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        assert resp.status_code == 201
        chat_id = resp.json()["id"]

        # 3 turns
        for prompt in ["Say A.", "Say B.", "Say C."]:
            _, events, _ = stream_message(chat_id, prompt)
            expect_done(events)

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
