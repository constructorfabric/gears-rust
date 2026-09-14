"""E2E tests for code interpreter tool support (XLSX upload → code_interpreter).

Verifies:
- XLSX files are accepted and reach 'ready' status (no vector-store indexing)
- XLSX uploads get purpose = 'code_interpreter' in the attachment response
- Messages with XLSX attachments produce code_interpreter tool events in SSE
- Provider request includes code_interpreter tool with container.file_ids
- Non-XLSX documents still route to file_search (purpose = 'file_search')
"""

import io
import time

import httpx
import pytest

from .conftest import API_PREFIX, STANDARD_MODEL, expect_done, expect_stream_started, parse_sse, poll_until, stream_message


@pytest.fixture
def openai_chat(chat_with_model):
    """Chat using the OpenAI model — code_interpreter is OpenAI-only."""
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

    def test_xlsx_reaches_ready(self, openai_chat):
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("report.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]

        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"
        assert detail.get("doc_summary") is None, "XLSX (code_interpreter) should not have doc_summary"


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

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(
            chat_id, "CODEINTERP: analyze", attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
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

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("notes.txt", io.BytesIO(b"plain text content"), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(
            chat_id, "FILESEARCH: summarize", attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
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

    def test_code_interpreter_tool_events_in_stream(self, openai_chat):
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        # Upload XLSX
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("sales.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"

        # Send message with the XLSX attachment
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "CODEINTERP: Analyze the data in the spreadsheet.", "attachment_ids": [att_id]},
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

        # Verify code_interpreter tool events appeared
        tool_events = [e for e in events if e.event == "tool"]
        ci_tools = [
            t for t in tool_events
            if isinstance(t.data, dict) and t.data.get("name") == "code_interpreter"
        ]
        assert len(ci_tools) >= 1, (
            f"Expected code_interpreter tool events. "
            f"Tool events: {[t.data for t in tool_events]}. "
            f"All event types: {[e.event for e in events]}"
        )

    def test_code_interpreter_has_start_and_done(self, openai_chat):
        """code_interpreter tool events should have both 'start' and 'done' phases."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("analysis.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

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

        ci_tools = [
            t for t in events
            if t.event == "tool"
            and isinstance(t.data, dict)
            and t.data.get("name") == "code_interpreter"
        ]
        phases = [t.data.get("phase") for t in ci_tools]
        assert "start" in phases, f"Missing 'start' phase. Phases: {phases}"
        assert "done" in phases, f"Missing 'done' phase. Phases: {phases}"

    def test_code_interpreter_done_has_output(self, openai_chat):
        """code_interpreter 'done' event should include output details."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("metrics.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

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
            t for t in events
            if t.event == "tool"
            and isinstance(t.data, dict)
            and t.data.get("name") == "code_interpreter"
            and t.data.get("phase") == "done"
        ]
        assert len(done_events) >= 1, "No code_interpreter done event"
        details = done_events[0].data.get("details", {})
        assert "output" in details, (
            f"code_interpreter done event missing 'output' in details: {details}"
        )

    def test_code_interpreter_stream_has_deltas(self, openai_chat):
        """Stream with code_interpreter should still have delta text events."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

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

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        mock_provider.clear_captured_requests()

        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: Analyze the data.",
            attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
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
        file_ids = container.get("file_ids", [])
        assert len(file_ids) > 0, (
            f"Expected non-empty file_ids in container: {container}"
        )

    def test_no_file_search_tool_for_xlsx(self, openai_chat, mock_provider):
        """XLSX attachments should NOT trigger file_search tool (only code_interpreter)."""
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        mock_provider.clear_captured_requests()

        status, _events, _ = stream_message(
            chat_id,
            "CODEINTERP: What is the sum?",
            attachment_ids=[att_id],
        )
        assert status == 200
        time.sleep(0.5)
        req = mock_provider.get_last_request()
        assert req is not None, "No request captured by mock provider"

        tools = req.get("tools", [])
        fs_tools = [t for t in tools if t.get("type") == "file_search"]
        assert len(fs_tools) == 0, (
            f"XLSX should not trigger file_search tool. Tools: {tools}"
        )


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
        resp_txt = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("report.txt", io.BytesIO(b"Revenue report: Q1 was strong."), "text/plain")},
            timeout=60,
        )
        assert resp_txt.status_code == 201
        txt_id = resp_txt.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{txt_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        # Upload XLSX file (code_interpreter purpose)
        xlsx = _make_minimal_xlsx()
        resp_xlsx = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp_xlsx.status_code == 201
        xlsx_id = resp_xlsx.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{xlsx_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        mock_provider.clear_captured_requests()

        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: Compare the report with the spreadsheet data.",
            attachment_ids=[txt_id, xlsx_id],
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
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

    def test_tool_events_before_done(self, openai_chat):
        chat_id = openai_chat["id"]
        xlsx = _make_minimal_xlsx()

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

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

        tool_events = [e for e in events if e.event == "tool"]
        assert len(tool_events) > 0, "Expected tool events for ordering check"
        done_idx = next(i for i, e in enumerate(events) if e.event == "done")
        for i, e in enumerate(events):
            if e.event == "tool":
                assert i < done_idx, (
                    f"tool event at index {i} should be before done at {done_idx}"
                )


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

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"

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
