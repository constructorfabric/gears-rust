"""E2E tests for code interpreter tool support (XLSX upload → code_interpreter).

Verifies:
- XLSX files are accepted and reach 'ready' status (no vector-store indexing)
- XLSX uploads route to code_interpreter, not file_search (provider request tools)
- Messages with XLSX attachments produce code_interpreter tool events in SSE
- Provider request includes code_interpreter tool with container.file_ids
- Non-XLSX documents still route to file_search
"""

import io
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    STANDARD_MODEL,
    assert_problem,
    expect_done,
    expect_stream_started,
    parse_sse,
    poll_turn,
    query_db,
    stream_message,
)
from .mock_provider.responses import MockEvent, Scenario
from .test_attachments import _upload_ready


@pytest.fixture
def openai_chat(chat_with_model):
    """Chat on gpt-5.2 (OpenAI provider). azure-gpt-4.1 supports
    code_interpreter too; its accounting runs in test_code_interpreter_usage.py."""
    return chat_with_model(STANDARD_MODEL)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Minimal valid XLSX: an empty workbook (ZIP with required OpenXML entries).
# Built from the minimum entries needed for Excel/OpenAI to accept it.
def _make_minimal_xlsx() -> bytes:
    """Generate a minimal valid .xlsx file using zipfile + XML."""
    import zipfile

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(
            "[Content_Types].xml",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" '
            'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" '
            'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            "</Types>",
        )
        zf.writestr(
            "_rels/.rels",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
            "</Relationships>",
        )
        zf.writestr(
            "xl/_rels/workbook.xml.rels",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
            "</Relationships>",
        )
        zf.writestr(
            "xl/workbook.xml",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
            'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
            "<sheets>"
            '<sheet name="Sheet1" sheetId="1" r:id="rId1"/>'
            "</sheets>"
            "</workbook>",
        )
        zf.writestr(
            "xl/worksheets/sheet1.xml",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
            "<sheetData>"
            '<row r="1"><c r="A1" t="inlineStr"><is><t>Header</t></is></c><c r="B1"><v>42</v></c></row>'
            "</sheetData>"
            "</worksheet>",
        )
    return buf.getvalue()


XLSX_CONTENT_TYPE = (
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
)


# ---------------------------------------------------------------------------
# Upload acceptance tests
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestXlsxUploadAccepted:
    """XLSX files should be accepted and reach 'ready' status."""

    def test_xlsx_upload_accepted(self, openai_chat):
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201, (
            f"XLSX upload rejected: {resp.status_code} {resp.text}"
        )
        body = resp.json()
        assert body["filename"] == "data.xlsx"
        assert body["content_type"] == XLSX_CONTENT_TYPE
        assert body["kind"] == "document"

    @pytest.mark.usefixtures("offline_only")
    def test_xlsx_rejected_without_code_interpreter(self, chat_with_model, mock_provider):
        """An XLSX is only usable by code_interpreter: on a model without it
        (gpt-5-nano) the upload is 400 invalid_argument and nothing is stored
        or sent to the provider (offline only: checks the mock's traffic)."""
        chat_id = chat_with_model("gpt-5-nano")["id"]
        mock_provider.clear_captured_requests()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(_make_minimal_xlsx()), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        body = assert_problem(resp, 400, "invalid_argument")
        assert body["detail"] == "Code interpreter is currently unavailable", body
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []
        assert mock_provider.get_post_paths() == []

    def test_xlsx_reaches_ready(self, openai_chat):
        chat_id = openai_chat["id"]
        att_id = _upload_ready(chat_id, "report.xlsx", _make_minimal_xlsx(), XLSX_CONTENT_TYPE)

        detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10).json()
        assert detail["status"] == "ready"
        # doc_summary is not generated (ADR-0007): the field is omitted.
        assert "doc_summary" not in detail, detail


# ---------------------------------------------------------------------------
# Purpose routing tests
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestXlsxPurposeRouting:
    """XLSX routes to code_interpreter, TXT routes to file_search.

    Purpose is verified indirectly via the provider request tools:
    XLSX → code_interpreter tool, TXT → file_search tool.
    """

    @pytest.fixture(autouse=True)
    def _clear_and_skip(self, mock_provider, request):
        mock_provider.clear_captured_requests()
        if request.config.getoption("mode") == "online":
            pytest.skip("purpose routing verification requires offline mode")

    def test_xlsx_triggers_code_interpreter_not_file_search(self, openai_chat, mock_provider):
        """XLSX attachment should produce code_interpreter tool, not file_search."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "data.xlsx", xlsx, XLSX_CONTENT_TYPE)

        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(
            chat_id, "CODEINTERP: analyze", attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        req = mock_provider.get_last_request()
        assert req is not None
        tools = req.get("tools", [])
        tool_types = [t.get("type") for t in tools]
        assert "code_interpreter" in tool_types, (
            f"Expected code_interpreter in tools: {tool_types}"
        )
        assert "file_search" not in tool_types, (
            f"XLSX should not trigger file_search: {tool_types}"
        )

    def test_txt_triggers_file_search_not_code_interpreter(self, openai_chat, mock_provider):
        """TXT attachment should produce file_search tool, not code_interpreter."""
        chat_id = openai_chat["id"]

        att_id = _upload_ready(chat_id, "notes.txt", b"plain text content", "text/plain")

        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(
            chat_id, "FILESEARCH: summarize", attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        req = mock_provider.get_last_request()
        assert req is not None
        tools = req.get("tools", [])
        tool_types = [t.get("type") for t in tools]
        assert "file_search" in tool_types, (
            f"Expected file_search in tools: {tool_types}"
        )
        assert "code_interpreter" not in tool_types, (
            f"TXT should not trigger code_interpreter: {tool_types}"
        )
        assert "include" not in req, req.get("include")


# ---------------------------------------------------------------------------
# XLSX upload via octet-stream (extension-based MIME inference)
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestXlsxOctetStreamInference:
    """XLSX files sent as application/octet-stream should be inferred from extension."""

    def test_xlsx_octet_stream_accepted(self, openai_chat):
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), "application/octet-stream")},
            timeout=60,
        )
        assert resp.status_code == 201, (
            f"XLSX via octet-stream rejected: {resp.status_code} {resp.text}"
        )
        body = resp.json()
        assert body["kind"] == "document"
        assert body["content_type"] == (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        ), f"MIME not normalized from octet-stream: {body['content_type']}"


# ---------------------------------------------------------------------------
# Streaming tests — code_interpreter tool events
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestCodeInterpreterToolEvents:
    """XLSX attachment + message → code_interpreter tool events in SSE."""

    @pytest.mark.usefixtures("offline_only")
    def test_code_interpreter_has_start_and_done(self, openai_chat):
        """The mock's one code interpreter call (`CODEINTERP:*`) is sent as two
        `code_interpreter` tool events: phase `start`, then `done`."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "analysis.xlsx", xlsx, XLSX_CONTENT_TYPE)

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "CODEINTERP: What is the total?", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status = resp.status_code
        raw = resp.text
        events = parse_sse(raw) if status == 200 else []
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        expect_done(events)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data

        tools = [(t.data["name"], t.data["phase"]) for t in events if t.event == "tool"]
        assert tools == [("code_interpreter", "start"), ("code_interpreter", "done")], tools

    @pytest.mark.usefixtures("offline_only")
    def test_code_interpreter_done_has_output(self, openai_chat):
        """The code_interpreter `done` event carries the logs output. The
        provider sends it in `response.output_item.done`; the mock
        `CODEINTERP:*` logs are "Total: 42\nAverage: 7.0"."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "metrics.xlsx", xlsx, XLSX_CONTENT_TYPE)

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "CODEINTERP: Compute the average.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status = resp.status_code
        raw = resp.text
        events = parse_sse(raw) if status == 200 else []
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        expect_done(events)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data

        done_events = [
            t.data for t in events
            if t.event == "tool" and t.data["phase"] == "done"
        ]
        assert len(done_events) == 1, [e.event for e in events]
        assert done_events[0]["name"] == "code_interpreter", done_events
        assert done_events[0]["details"] == {"output": "Total: 42\nAverage: 7.0"}, done_events

    def test_code_interpreter_stream_has_deltas(self, openai_chat):
        """Stream with code_interpreter should still have delta text events."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "data.xlsx", xlsx, XLSX_CONTENT_TYPE)

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "CODEINTERP: Summarize the data.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status = resp.status_code
        raw = resp.text
        events = parse_sse(raw) if status == 200 else []
        assert status == 200
        done = expect_done(events)
        ss = expect_stream_started(events)
        assert "request_id" in ss.data
        assert "message_id" in ss.data

        deltas = [e for e in events if e.event == "delta"]
        assert len(deltas) > 0, "Expected delta events in code_interpreter response"
        text = "".join(
            e.data.get("content", "") for e in deltas if isinstance(e.data, dict)
        )
        assert len(text.strip()) > 0, "Assembled text from deltas is empty"

        # Usage should be present
        usage = done.data.get("usage", {})
        assert usage.get("input_tokens", 0) > 0
        assert usage.get("output_tokens", 0) > 0


# ---------------------------------------------------------------------------
# Provider request verification (offline only)
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestCodeInterpreterProviderRequest:
    """Verify the provider request body includes code_interpreter tool."""

    @pytest.fixture(autouse=True)
    def _clear_and_skip(self, mock_provider, request):
        mock_provider.clear_captured_requests()
        if request.config.getoption("mode") == "online":
            pytest.skip("provider request capture requires offline mode")

    def test_code_interpreter_tool_in_request(self, openai_chat, mock_provider):
        """Provider request should include code_interpreter tool with container.file_ids."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "data.xlsx", xlsx, XLSX_CONTENT_TYPE)

        mock_provider.clear_captured_requests()

        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: Analyze the data.",
            attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        req = mock_provider.get_last_request()
        assert req is not None, "No request captured by mock provider"

        tools = req.get("tools", [])
        ci_tools = [t for t in tools if t.get("type") == "code_interpreter"]
        assert len(ci_tools) == 1, (
            f"Expected exactly one code_interpreter tool, got {len(ci_tools)}. "
            f"Tools: {tools}"
        )

        # Verify container.file_ids is present and non-empty
        container = ci_tools[0].get("container", {})
        assert container.get("type") == "auto", (
            f"Expected container.type='auto', got: {container}"
        )
        provider_file_id = query_db(
            "SELECT provider_file_id FROM attachments WHERE id = ?", (att_id,),
        )[0]["provider_file_id"]
        assert container.get("file_ids") == [provider_file_id], container
        # Without it the provider sends no code_interpreter_call outputs.
        assert req.get("include") == ["code_interpreter_call.outputs"], req.get("include")


# ---------------------------------------------------------------------------
# Mixed attachments: XLSX + text file
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestMixedAttachments:
    """Upload both XLSX and text file to same chat, verify both purposes work."""

    @pytest.fixture(autouse=True)
    def _clear_and_skip(self, mock_provider, request):
        mock_provider.clear_captured_requests()
        if request.config.getoption("mode") == "online":
            pytest.skip("provider request capture requires offline mode")

    def test_mixed_xlsx_and_txt_both_tools_in_request(self, openai_chat, mock_provider):
        """When both XLSX and TXT are attached, request should have both tools."""
        chat_id = openai_chat["id"]

        # Upload text file (file_search purpose)
        txt_id = _upload_ready(chat_id, "report.txt", b"Revenue report: Q1 was strong.", "text/plain")

        # Upload XLSX file (code_interpreter purpose)
        xlsx = _make_minimal_xlsx()
        xlsx_id = _upload_ready(chat_id, "data.xlsx", xlsx, XLSX_CONTENT_TYPE)

        mock_provider.clear_captured_requests()

        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: Compare the report with the spreadsheet data.",
            attachment_ids=[txt_id, xlsx_id],
        )
        assert status == 200
        expect_done(events)

        req = mock_provider.get_last_request()
        assert req is not None, "No request captured"

        tools = req.get("tools", [])
        tool_types = [t.get("type") for t in tools]

        assert "code_interpreter" in tool_types, (
            f"Expected code_interpreter in tools: {tool_types}"
        )
        assert "file_search" in tool_types, (
            f"Expected file_search in tools: {tool_types}"
        )


# ---------------------------------------------------------------------------
# Event ordering
# ---------------------------------------------------------------------------


@pytest.mark.openai
class TestCodeInterpreterEventOrdering:
    """SSE event ordering: tool events must appear before done."""

    @pytest.mark.usefixtures("offline_only")
    def test_tool_events_before_done(self, openai_chat):
        """The events are relayed in the provider's order (grammar
        `stream_started ping* (delta|tool)* citations? done`): the mock
        `CODEINTERP:*` answer runs the code interpreter first and then
        streams three text deltas, so the stream is `stream_started`, the
        tool `start` and `done`, the three deltas, `done`."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "data.xlsx", xlsx, XLSX_CONTENT_TYPE)

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "CODEINTERP: Process the spreadsheet.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status = resp.status_code
        raw = resp.text
        events = parse_sse(raw) if status == 200 else []
        assert status == 200
        expect_done(events)

        sequence = [
            (e.event, e.data["phase"] if e.event == "tool" else None) for e in events
        ]
        assert sequence == [
            ("stream_started", None),
            ("tool", "start"), ("tool", "done"),
            ("delta", None), ("delta", None), ("delta", None),
            ("done", None),
        ], sequence


class TestCodeInterpreterPerMessageLimit:
    """At most `quota.code_interpreter_max_calls_per_message` (10, QuotaConfig
    default in config.rs, not overridden in base.yaml) code interpreter calls
    per message. Mock only: the limit is defence in depth, a real provider
    stops at the model's `max_tool_calls` (2, base.yaml) first."""

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.timeout(30)
    def test_eleventh_code_interpreter_call_fails_the_turn(self, openai_chat, mock_provider):
        """The provider starts an eleventh code interpreter call in one answer:
        the client gets the tool events of the ten allowed calls (the eleventh
        `start` is not forwarded), then SSE `error`
        `code_interpreter_calls_exceeded`; the turn fails with that code."""
        chat_id = openai_chat["id"]
        att_id = _upload_ready(chat_id, "data.xlsx", _make_minimal_xlsx(), XLSX_CONTENT_TYPE)

        def call(i: int) -> list[MockEvent]:
            item_id = f"ci_mock_{i}"
            return [
                MockEvent("response.code_interpreter_call.in_progress", {
                    "item_id": item_id, "output_index": i,
                }),
                MockEvent("response.output_item.done", {
                    "output_index": i,
                    "item": {
                        "type": "code_interpreter_call", "id": item_id,
                        "status": "completed", "code": "print(1)",
                        "outputs": [{"type": "logs", "logs": "1"}],
                    },
                }),
            ]

        mock_provider.set_next_scenario(Scenario(events=[
            *(ev for i in range(11) for ev in call(i)),
            MockEvent("response.output_text.delta", {"delta": "Too many runs"}),
            MockEvent("response.output_text.done", {"text": "Too many runs"}),
        ]))
        rid = str(uuid.uuid4())
        status, events, raw = stream_message(
            chat_id, "Analyze the data.", attachment_ids=[att_id], request_id=rid,
        )
        assert status == 200, raw
        phases = [(e.data["name"], e.data["phase"]) for e in events if e.event == "tool"]
        assert phases == [("code_interpreter", "start"), ("code_interpreter", "done")] * 10, phases
        assert events[-1].event == "error", [e.event for e in events]
        assert events[-1].data["code"] == "code_interpreter_calls_exceeded", events[-1].data
        turn = poll_turn(chat_id, rid)
        assert (turn["state"], turn["error_code"]) == (
            "error", "code_interpreter_calls_exceeded",
        ), turn


# ---------------------------------------------------------------------------
# Online-only: real provider XLSX analysis
# ---------------------------------------------------------------------------


@pytest.mark.openai
@pytest.mark.online_only
class TestCodeInterpreterOnline:
    """Online test: upload real XLSX and verify end-to-end code interpreter."""

    def test_xlsx_code_interpreter_produces_answer(self, openai_chat):
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        att_id = _upload_ready(chat_id, "data.xlsx", xlsx, XLSX_CONTENT_TYPE)

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Read the attached spreadsheet and tell me what value is in cell B1.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status = resp.status_code
        raw = resp.text
        events = parse_sse(raw) if status == 200 else []
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        expect_done(events)

        # Collect response text
        delta_text = "".join(
            e.data.get("content", "")
            for e in events
            if e.event == "delta" and isinstance(e.data, dict)
        )
        assert len(delta_text) > 0, "Expected non-empty response from LLM"
