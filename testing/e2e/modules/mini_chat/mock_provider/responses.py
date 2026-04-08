"""Canned response scenarios for the mock LLM provider."""

from __future__ import annotations

import fnmatch
from dataclasses import dataclass, field


@dataclass
class Usage:
    input_tokens: int = 50
    output_tokens: int = 12


@dataclass
class MockEvent:
    """A single SSE event in a scenario."""
    event_type: str  # e.g. "response.output_text.delta"
    data: dict = field(default_factory=dict)


@dataclass
class Scenario:
    """Ordered list of events the mock should emit, plus terminal metadata."""
    events: list[MockEvent] = field(default_factory=list)
    usage: Usage = field(default_factory=Usage)
    citations: list[dict] = field(default_factory=list)
    # Terminal type: "completed" (default), "failed", "incomplete"
    terminal: str = "completed"
    error: dict | None = None
    incomplete_reason: str | None = None
    # Seconds to sleep between SSE events (0 = instant). Used for cancellation tests.
    slow: float = 0


# ── Built-in scenario registry ─────────────────────────────────────────────

SCENARIOS: dict[str, Scenario] = {
    "PING": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "PONG"}),
            MockEvent("response.output_text.done", {"text": "PONG"}),
        ],
        usage=Usage(input_tokens=30, output_tokens=2),
    ),
    "SEARCH:*": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "Searching"}),
            MockEvent("response.web_search_call.searching", {}),
            MockEvent("response.output_text.delta", {"delta": "...found"}),
            MockEvent("response.web_search_call.completed", {}),
            MockEvent("response.output_text.delta", {"delta": " results"}),
            MockEvent("response.output_text.done", {"text": "Searching...found results"}),
        ],
        usage=Usage(input_tokens=80, output_tokens=15),
        citations=[{
            "type": "url_citation",
            "url": "https://example.com",
            "title": "Mock Search Result",
            "start_index": 0,
            "end_index": 9,
            "text": "Searching",
        }],
    ),
    "FILESEARCH:*": Scenario(
        events=[
            MockEvent("response.file_search_call.searching", {}),
            MockEvent("response.file_search_call.completed", {
                "results": [
                    {"file_id": "file_mock_1", "filename": "doc.pdf", "score": 0.95, "text": "Mock content"},
                ],
            }),
            MockEvent("response.output_text.delta", {"delta": "Based on docs"}),
            MockEvent("response.output_text.done", {"text": "Based on docs"}),
        ],
        usage=Usage(input_tokens=200, output_tokens=10),
        citations=[{
            "type": "file_citation",
            "file_id": "file_mock_1",
            "title": "doc.pdf",
            "start_index": 0,
            "end_index": 13,
            "text": "Based on docs",
        }],
    ),
    "CODEINTERP:*": Scenario(
        events=[
            MockEvent("response.code_interpreter_call.in_progress", {}),
            MockEvent("response.code_interpreter_call.interpreting", {}),
            MockEvent("response.code_interpreter_call.completed", {
                "outputs": [
                    {"type": "logs", "logs": "Total: 42\nAverage: 7.0"},
                ],
            }),
            MockEvent("response.output_text.delta", {"delta": "The spreadsheet "}),
            MockEvent("response.output_text.delta", {"delta": "analysis shows "}),
            MockEvent("response.output_text.delta", {"delta": "a total of 42."}),
            MockEvent("response.output_text.done", {"text": "The spreadsheet analysis shows a total of 42."}),
        ],
        usage=Usage(input_tokens=300, output_tokens=20),
    ),
    "Write*": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "The history of "}),
            MockEvent("response.output_text.delta", {"delta": "computing spans "}),
            MockEvent("response.output_text.delta", {"delta": "many decades "}),
            MockEvent("response.output_text.delta", {"delta": "of innovation "}),
            MockEvent("response.output_text.delta", {"delta": "and discovery. "}),
            MockEvent("response.output_text.delta", {"delta": "From Babbage "}),
            MockEvent("response.output_text.delta", {"delta": "to quantum. "}),
            MockEvent("response.output_text.done", {"text": "The history of computing spans many decades of innovation and discovery. From Babbage to quantum. "}),
        ],
        usage=Usage(input_tokens=100, output_tokens=50),
        slow=0.3,  # 300ms between events — gives client time to disconnect
    ),
    "ERROR": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "Partial"}),
        ],
        terminal="failed",
        error={"code": "server_error", "message": "Mock provider error"},
    ),
    "TRUNCATE": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "Truncated text"}),
            MockEvent("response.output_text.done", {"text": "Truncated text"}),
        ],
        terminal="incomplete",
        incomplete_reason="max_output_tokens",
        usage=Usage(input_tokens=50, output_tokens=100),
    ),
    "*": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "Hello! "}),
            MockEvent("response.output_text.delta", {"delta": "How can I help?"}),
            MockEvent("response.output_text.done", {"text": "Hello! How can I help?"}),
        ],
        usage=Usage(input_tokens=50, output_tokens=12),
    ),
}


def match_scenario(user_input: str) -> Scenario:
    """Match user input to a scenario: exact match -> glob -> default."""
    if user_input in SCENARIOS:
        return SCENARIOS[user_input]
    for pattern, scenario in SCENARIOS.items():
        if pattern != "*" and fnmatch.fnmatch(user_input, pattern):
            return scenario
    return SCENARIOS["*"]


def extract_last_user_message(body: dict) -> str:
    """Extract the last user message content from a Responses API request body."""
    input_field = body.get("input", "")
    if isinstance(input_field, str):
        return input_field
    if isinstance(input_field, list):
        for msg in reversed(input_field):
            if isinstance(msg, dict) and msg.get("role") == "user":
                content = msg.get("content", "")
                if isinstance(content, str):
                    return content
                if isinstance(content, list):
                    for part in content:
                        if isinstance(part, dict) and part.get("type") == "input_text":
                            return part.get("text", "")
                        if isinstance(part, str):
                            return part
    return ""


def has_tool(body: dict, tool_type: str) -> bool:
    """Check if the request body includes a specific tool type."""
    tools = body.get("tools", [])
    return any(
        isinstance(t, dict) and t.get("type", "").startswith(tool_type)
        for t in tools
    )


def should_include_tool_event(event: MockEvent, body: dict) -> bool:
    """Check if a tool event should be included based on request tools."""
    et = event.event_type
    if "web_search" in et:
        return has_tool(body, "web_search")
    if "file_search" in et:
        return has_tool(body, "file_search")
    if "code_interpreter" in et:
        return has_tool(body, "code_interpreter")
    return True
