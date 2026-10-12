"""Canned response scenarios for the mock LLM provider."""

from __future__ import annotations

import fnmatch
from dataclasses import dataclass, field


@dataclass
class Usage:
    input_tokens: int = 50
    output_tokens: int = 12
    # `input_tokens_details.cached_tokens` / `output_tokens_details.reasoning_tokens`
    # of `response.usage` (subsets of input_tokens / output_tokens).
    cached_tokens: int = 0
    reasoning_tokens: int = 0
    # True: input_tokens is a floor that grows with the request's input items
    # (sse_builder._count_input_tokens). False: input_tokens is sent as is.
    scale_input: bool = True
    # True: `response.completed` has no `usage` field (a protocol violation;
    # the real API always sends it).
    omit: bool = False


@dataclass
class MockEvent:
    """A single SSE event in a scenario."""
    event_type: str  # e.g. "response.output_text.delta"
    data: dict = field(default_factory=dict)
    # Seconds to wait before sending this event (on top of `Scenario.slow`).
    delay: float = 0


@dataclass
class Scenario:
    """Ordered list of events the mock should emit, plus terminal metadata."""
    events: list[MockEvent] = field(default_factory=list)
    usage: Usage = field(default_factory=Usage)
    citations: list[dict] = field(default_factory=list)
    # Extra `response.completed` output items placed before the message item,
    # e.g. a `function_call` item. The real API also streams each output item
    # (`response.output_item.added` / `.done`, and for a function call
    # `response.function_call_arguments.delta` / `.done`); the mock sends
    # these items only in `response.completed`, where the gear reads
    # function calls.
    output_items: list[dict] = field(default_factory=list)
    # The whole `response.completed` output. When set, it replaces the
    # message item built from the deltas, `citations` and `output_items`.
    output: list[dict] | None = None
    # Terminal type: "completed" (default), "failed", "incomplete", or "none":
    # the stream ends (the connection closes) after `events` without any
    # terminal event.
    terminal: str = "completed"
    error: dict | None = None
    # `response.failed` carries `usage` (built from `usage`) instead of null;
    # the real API sends it when the model produced tokens before failing.
    failed_with_usage: bool = False
    incomplete_reason: str | None = None
    # Seconds to sleep between SSE events (0 = instant). Used for cancellation tests.
    slow: float = 0
    # Seconds to wait after the response headers and before the first SSE
    # event. Keeps the stream idle so the gear sends `ping` keepalives.
    initial_delay: float = 0
    # Seconds to wait before sending the response status line and headers.
    # Longer than OAGW `proxy_timeout_secs` makes the gateway time out.
    header_delay: float = 0
    # HTTP-level error: return this status code + JSON body instead of SSE stream.
    # When set, no SSE is produced — the mock returns a plain JSON error response.
    http_error_status: int | None = None
    http_error_body: dict | None = None
    # Extra response headers of the HTTP-level error (e.g. `Retry-After`).
    http_error_headers: dict[str, str] = field(default_factory=dict)
    # Send the SSE events without `event:` lines; each event is named only
    # by its `data.type`.
    omit_event_lines: bool = False


def _message_item(item_id: str, text: str | None,
                  annotations: list[dict] | None = None) -> dict:
    """A Responses API `message` output item with one `output_text` part;
    `text=None` is the item as `response.output_item.added` sends it."""
    if text is None:
        return {
            "type": "message", "id": item_id, "status": "in_progress",
            "role": "assistant", "content": [],
        }
    return {
        "type": "message", "id": item_id, "status": "completed", "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": annotations or []}],
    }


# `SEARCH:*` answer: two message items around the web search call. The one
# citation is on the second message; url_citation indices are character
# offsets into the `output_text` part that carries the annotation, so
# [3, 8) is "found" (in the whole answer it would be "rchin").
SEARCH_TEXT_1 = "Searching"
SEARCH_TEXT_2 = "...found results"
SEARCH_CALL_ITEM = {
    "type": "web_search_call",
    "id": "ws_mock_1",
    "status": "completed",
    "action": {"type": "search", "query": "mock query"},
}
# OpenAI url_citation: no text; the snippet comes from the text range.
SEARCH_CITATION = {
    "type": "url_citation",
    "url": "https://example.com",
    "title": "Mock Search Result",
    "start_index": 3,
    "end_index": 8,
}


# ── Built-in scenario registry ─────────────────────────────────────────────

SCENARIOS: dict[str, Scenario] = {
    "PING": Scenario(
        events=[
            MockEvent("response.output_text.delta", {"delta": "PONG"}),
            MockEvent("response.output_text.done", {"text": "PONG"}),
        ],
        usage=Usage(input_tokens=30, output_tokens=2),
    ),
    # A web search answer as the Responses API streams it: a message, the
    # web_search_call item, then a second message that carries the citation.
    "SEARCH:*": Scenario(
        events=[
            MockEvent("response.output_item.added", {
                "output_index": 0, "item": _message_item("msg_mock_ws_1", None),
            }),
            MockEvent("response.output_text.delta", {
                "item_id": "msg_mock_ws_1", "output_index": 0, "content_index": 0,
                "delta": SEARCH_TEXT_1,
            }),
            MockEvent("response.output_text.done", {
                "item_id": "msg_mock_ws_1", "output_index": 0, "content_index": 0,
                "text": SEARCH_TEXT_1,
            }),
            MockEvent("response.output_item.done", {
                "output_index": 0, "item": _message_item("msg_mock_ws_1", SEARCH_TEXT_1),
            }),
            MockEvent("response.output_item.added", {
                "output_index": 1,
                "item": {"type": "web_search_call", "id": "ws_mock_1", "status": "in_progress"},
            }),
            MockEvent("response.web_search_call.in_progress", {
                "item_id": "ws_mock_1", "output_index": 1,
            }),
            MockEvent("response.web_search_call.searching", {
                "item_id": "ws_mock_1", "output_index": 1,
            }),
            MockEvent("response.web_search_call.completed", {
                "item_id": "ws_mock_1", "output_index": 1,
            }),
            MockEvent("response.output_item.done", {
                "output_index": 1, "item": SEARCH_CALL_ITEM,
            }),
            MockEvent("response.output_item.added", {
                "output_index": 2, "item": _message_item("msg_mock_ws_2", None),
            }),
            MockEvent("response.output_text.delta", {
                "item_id": "msg_mock_ws_2", "output_index": 2, "content_index": 0,
                "delta": "...found",
            }),
            MockEvent("response.output_text.delta", {
                "item_id": "msg_mock_ws_2", "output_index": 2, "content_index": 0,
                "delta": " results",
            }),
            MockEvent("response.output_text.annotation.added", {
                "item_id": "msg_mock_ws_2", "output_index": 2, "content_index": 0,
                "annotation_index": 0, "annotation": SEARCH_CITATION,
            }),
            MockEvent("response.output_text.done", {
                "item_id": "msg_mock_ws_2", "output_index": 2, "content_index": 0,
                "text": SEARCH_TEXT_2,
            }),
            MockEvent("response.output_item.done", {
                "output_index": 2,
                "item": _message_item("msg_mock_ws_2", SEARCH_TEXT_2, [SEARCH_CITATION]),
            }),
        ],
        usage=Usage(input_tokens=80, output_tokens=15),
        output=[
            _message_item("msg_mock_ws_1", SEARCH_TEXT_1),
            SEARCH_CALL_ITEM,
            _message_item("msg_mock_ws_2", SEARCH_TEXT_2, [SEARCH_CITATION]),
        ],
    ),
    "FILESEARCH:*": Scenario(
        events=[
            MockEvent("response.file_search_call.searching", {}),
            # OpenAI sends no results in this event.
            MockEvent("response.file_search_call.completed", {
                "item_id": "fs_mock_1", "output_index": 0,
            }),
            MockEvent("response.output_text.delta", {"delta": "Based on docs"}),
            MockEvent("response.output_text.done", {"text": "Based on docs"}),
        ],
        usage=Usage(input_tokens=200, output_tokens=10),
        # OpenAI file_citation: file_id, filename and a position; no text or range.
        citations=[{
            "type": "file_citation",
            "file_id": "file_mock_1",
            "filename": "doc.pdf",
            "index": 13,
        }],
    ),
    "CODEINTERP:*": Scenario(
        events=[
            MockEvent("response.code_interpreter_call.in_progress", {
                "item_id": "ci_mock_1", "output_index": 0,
            }),
            MockEvent("response.code_interpreter_call.interpreting", {
                "item_id": "ci_mock_1", "output_index": 0,
            }),
            # No outputs here; they come in response.output_item.done, and
            # only when the request has include: ["code_interpreter_call.outputs"].
            MockEvent("response.code_interpreter_call.completed", {
                "item_id": "ci_mock_1", "output_index": 0,
            }),
            MockEvent("response.output_item.done", {
                "output_index": 0,
                "item": {
                    "type": "code_interpreter_call",
                    "id": "ci_mock_1",
                    "status": "completed",
                    "code": "print(total, average)",
                    "outputs": [
                        {"type": "logs", "logs": "Total: 42\nAverage: 7.0"},
                    ],
                },
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


# `usage` of the mock summary response: the summary model reasons, so its
# output_tokens include reasoning tokens that are not part of the text.
SUMMARY_OUTPUT_TOKENS = 89
SUMMARY_REASONING_TOKENS = 64


def mock_summary_text(prompt: str) -> str:
    """Summary the mock returns for a thread summary prompt.

    Counts the `User:` / `Assistant:` lines of the prompt, so a test can tell
    which messages the prompt contained.
    """
    lines = prompt.splitlines()
    users = sum(1 for line in lines if line.startswith("User: "))
    assistants = sum(1 for line in lines if line.startswith("Assistant: "))
    return f"MOCK-SUMMARY {users} user and {assistants} assistant messages"


def build_summary_response(body: dict, model: str, response_id: str) -> dict:
    """Non-streaming Responses API object for a thread summary request.

    The text has the `<analysis>` and `<summary>` blocks that the summary
    worker parses (thread_summary_worker.rs, `format_summary_output`).
    """
    summary = mock_summary_text(extract_last_user_message(body))
    text = f"<analysis>Mock analysis.</analysis>\n<summary>{summary}</summary>"
    return {
        "id": response_id,
        "object": "response",
        "status": "completed",
        "model": model,
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }],
        "usage": {
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": SUMMARY_OUTPUT_TOKENS,
            "output_tokens_details": {"reasoning_tokens": SUMMARY_REASONING_TOKENS},
            "total_tokens": 100 + SUMMARY_OUTPUT_TOKENS,
        },
    }


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
    item = event.data.get("item")
    if isinstance(item, dict) and isinstance(item.get("type"), str):
        et = item["type"]
    if "web_search" in et:
        return has_tool(body, "web_search")
    if "file_search" in et:
        return has_tool(body, "file_search")
    if "code_interpreter" in et:
        return has_tool(body, "code_interpreter")
    return True
