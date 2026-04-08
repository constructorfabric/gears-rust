#!/usr/bin/env python3
"""Mini-Chat smoke test script (stdlib only).

Prerequisites:
  1. Server is running:  make mini-chat
  2. Set your API key in config/mini-chat.yaml
     (static-credstore-plugin -> secrets -> value)

Usage (from the repo root):
  python3 modules/mini-chat/scripts/smoke-test-api.py                                  # run all steps
  python3 modules/mini-chat/scripts/smoke-test-api.py --no-sse                         # skip SSE streaming (no real API key)
  python3 modules/mini-chat/scripts/smoke-test-api.py --base-url http://host:port      # custom server address
  python3 modules/mini-chat/scripts/smoke-test-api.py --api-url-prefix /x              # custom route prefix
  python3 modules/mini-chat/scripts/smoke-test-api.py --indexing-delay 10              # seconds to wait for provider indexing (default 5)
"""

from __future__ import annotations

import io
import json
import sys
import time
import urllib.error
import urllib.request
import uuid
import zipfile
from typing import Any

NO_SSE = "--no-sse" in sys.argv

def _parse_arg(flag: str, default: str) -> str:
    for i, arg in enumerate(sys.argv):
        if arg == flag and i + 1 < len(sys.argv):
            return sys.argv[i + 1].strip()
    return default

def _parse_base_url() -> str:
    url = _parse_arg("--base-url", "http://127.0.0.1:8087")
    return url.rstrip("/")

def _parse_prefix() -> str:
    p = _parse_arg("--api-url-prefix", "/mini-chat")
    return p if p.startswith("/") else f"/{p}"

BASE = _parse_base_url()
PREFIX = _parse_prefix()
INDEXING_DELAY = int(_parse_arg("--indexing-delay", "5"))


# -- Helpers ------------------------------------------------------------------

def bold(text: str) -> None:
    print(f"\033[1m{text}\033[0m")


def ok(text: str) -> None:
    print(f"\033[32m✓ {text}\033[0m")


def fail(text: str) -> None:
    print(f"\033[31m✗ {text}\033[0m")
    sys.exit(1)


def skip(text: str) -> None:
    print(f"  (skipped — {text})")
    ok("Skipped")


def request(
    method: str,
    path: str,
    body: dict[str, Any] | None = None,
    *,
    expect_status: int | None = None,
) -> tuple[int, Any]:
    """Send an HTTP request and return (status_code, parsed_json | None)."""
    url = f"{BASE}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            status = resp.status
            raw = resp.read().decode()
            parsed = json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        status = e.code
        raw = e.read().decode()
        parsed = json.loads(raw) if raw else None

    if expect_status is not None and status != expect_status:
        fail(f"{method} {path} → {status} (expected {expect_status})\n  {parsed}")
    return status, parsed


def multipart_upload(
    path: str,
    filename: str,
    content: bytes,
    content_type: str,
    *,
    expect_status: int | None = None,
) -> tuple[int, Any]:
    """Upload a file via multipart/form-data and return (status_code, parsed_json | None)."""
    boundary = f"----SmokeBoundary{uuid.uuid4().hex}"
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        f"Content-Type: {content_type}\r\n"
        f"\r\n"
    ).encode() + content + f"\r\n--{boundary}--\r\n".encode()

    url = f"{BASE}{path}"
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Content-Type", f"multipart/form-data; boundary={boundary}")

    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            status = resp.status
            raw = resp.read().decode()
            parsed = json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        status = e.code
        raw = e.read().decode()
        try:
            parsed = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            parsed = raw

    if expect_status is not None and status != expect_status:
        fail(f"POST {path} (multipart) → {status} (expected {expect_status})\n  {parsed}")
    return status, parsed


class SseResult:
    """Parsed result of an SSE stream."""

    def __init__(self) -> None:
        self.lines: list[str] = []
        self.msg_id: str | None = None
        self.text: str = ""
        self.tools: list[dict[str, Any]] = []
        self.done_data: dict[str, Any] | None = None
        self.has_error: bool = False

    @property
    def output(self) -> str:
        return "\n".join(self.lines)

    @property
    def is_done(self) -> bool:
        return self.done_data is not None


def sse_request(path: str, body: dict[str, Any]) -> SseResult:
    """Send a POST request and read SSE events.

    Returns an ``SseResult`` with parsed text, tool calls, and metadata.
    """
    url = f"{BASE}{path}"
    data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method="POST")
    req.add_header("Content-Type", "application/json")

    result = SseResult()
    current_event: str | None = None
    text_parts: list[str] = []

    with urllib.request.urlopen(req, timeout=120) as resp:
        for raw_line in resp:
            line = raw_line.decode().rstrip("\n")
            result.lines.append(line)

            if line.startswith("event:"):
                current_event = line[len("event:"):].strip()
                continue

            if not line.startswith("data:"):
                continue

            payload_str = line[len("data:"):].strip()
            try:
                payload = json.loads(payload_str)
            except json.JSONDecodeError:
                continue

            if current_event == "stream_started":
                result.msg_id = payload.get("message_id")

            elif current_event == "delta":
                chunk = payload.get("content", "")
                if chunk:
                    text_parts.append(chunk)

            elif current_event == "tool":
                result.tools.append(payload)

            elif current_event == "done":
                result.done_data = payload

            elif current_event == "error":
                result.has_error = True

    result.text = "".join(text_parts)
    return result


# -- Tests --------------------------------------------------------------------

def test_health() -> None:
    bold("Health check")
    request("GET", "/health", expect_status=200)
    ok("Server is healthy")


def test_list_models() -> tuple[str, str]:
    """Returns (first_model_id, first_model_display_name)."""
    bold("\n0. List models")
    _, data = request("GET", f"{PREFIX}/v1/models", expect_status=200)
    items = data["items"]
    print(json.dumps(data, indent=2))
    if not items:
        fail("No models returned")
    ok(f"Got {len(items)} models")
    return items[0]["model_id"], items[0].get("display_name", "")


def test_get_model(model_id: str) -> None:
    bold("\n0b. Get single model")
    _, data = request("GET", f"{PREFIX}/v1/models/{model_id}", expect_status=200)
    print(json.dumps(data, indent=2))
    if data["model_id"] != model_id:
        fail(f"Expected model_id={model_id}, got {data['model_id']}")
    ok(f"Got model detail: {model_id}")


def test_create_chat(model: str) -> str:
    bold("\n1. Create chat")
    _, data = request(
        "POST", f"{PREFIX}/v1/chats",
        {"title": "Smoke Test Chat", "model": model},
        expect_status=201,
    )
    print(json.dumps(data, indent=2))
    chat_id = data["id"]
    if not chat_id:
        fail("No chat id returned")
    ok(f"Created chat: {chat_id}")
    return chat_id


def test_get_chat(chat_id: str) -> None:
    bold("\n1b. Get single chat")
    _, data = request("GET", f"{PREFIX}/v1/chats/{chat_id}", expect_status=200)
    print(json.dumps(data, indent=2))
    if data["id"] != chat_id:
        fail(f"Expected id={chat_id}, got {data['id']}")
    ok(f"Got chat: {chat_id}")


def test_update_chat(chat_id: str) -> None:
    bold("\n1c. Update chat title")
    _, data = request(
        "PATCH", f"{PREFIX}/v1/chats/{chat_id}",
        {"title": "Updated Smoke Title"},
        expect_status=200,
    )
    print(json.dumps(data, indent=2))
    if data.get("title") != "Updated Smoke Title":
        fail(f"Expected title='Updated Smoke Title', got '{data.get('title')}'")
    ok("Updated chat title")


def test_list_chats(chat_id: str) -> None:
    bold("\n2. List chats")
    _, data = request("GET", f"{PREFIX}/v1/chats", expect_status=200)
    print(json.dumps(data, indent=2))
    found = any(c["id"] == chat_id for c in data["items"])
    if not found:
        fail(f"Chat {chat_id} not found in list")
    ok(f"Chat {chat_id} found in list")


def test_send_message(chat_id: str, content: str, label: str) -> str | None:
    bold(f"\n{label} (SSE stream)")
    if NO_SSE:
        skip("configure a real API key to test")
        return None
    sse = sse_request(
        f"{PREFIX}/v1/chats/{chat_id}/messages:stream",
        {"content": content},
    )
    print(sse.output)
    print(f"  → {sse.text}")
    if sse.tools:
        print(f"  tools: {[t.get('name', '?') for t in sse.tools]}")
    if sse.is_done:
        if not sse.msg_id:
            fail("Stream completed without stream_started.message_id")
        ok(f"Streamed successfully (msg_id: {sse.msg_id})")
    elif sse.has_error:
        fail("Stream returned error")
    else:
        fail("Unexpected SSE output")
    return sse.msg_id


def test_list_messages(chat_id: str, expected_count: int) -> list[dict[str, Any]]:
    """Returns the list of message dicts (empty list when skipped)."""
    bold("\n5. List messages")
    if NO_SSE:
        skip("no messages to list")
        return []
    _, data = request(
        "GET", f"{PREFIX}/v1/chats/{chat_id}/messages", expect_status=200,
    )
    print(json.dumps(data, indent=2))
    items = data["items"]
    count = len(items)
    if count != expected_count:
        fail(f"Expected {expected_count} messages, got {count}")
    ok(f"Got {count} messages ({expected_count // 2} user + {expected_count // 2} assistant)")
    return items


def test_reactions(chat_id: str, msg_id: str | None) -> None:
    bold("\n6. Reactions")
    if NO_SSE or not msg_id:
        skip("no messages to react to")
        return

    # 6a. Set like
    _, data = request(
        "PUT", f"{PREFIX}/v1/chats/{chat_id}/messages/{msg_id}/reaction",
        {"reaction": "like"},
        expect_status=200,
    )
    if data["reaction"] != "like":
        fail(f"Expected 'like', got '{data['reaction']}'")
    ok("6a. Set like reaction")

    # 6b. Change to dislike
    _, data = request(
        "PUT", f"{PREFIX}/v1/chats/{chat_id}/messages/{msg_id}/reaction",
        {"reaction": "dislike"},
        expect_status=200,
    )
    if data["reaction"] != "dislike":
        fail(f"Expected 'dislike', got '{data['reaction']}'")
    ok("6b. Changed to dislike")

    # 6c. Delete reaction
    status, _ = request(
        "DELETE", f"{PREFIX}/v1/chats/{chat_id}/messages/{msg_id}/reaction",
    )
    if status != 204:
        fail(f"Delete reaction returned {status}")
    ok("6c. Deleted reaction")


def test_get_turn(chat_id: str, request_id: str) -> None:
    bold("\n7. Get turn status")
    _, data = request(
        "GET", f"{PREFIX}/v1/chats/{chat_id}/turns/{request_id}",
        expect_status=200,
    )
    print(json.dumps(data, indent=2))
    if data["request_id"] != request_id:
        fail(f"Expected request_id={request_id}, got {data['request_id']}")
    state = data["state"]
    if state not in ("done", "running", "error", "cancelled"):
        fail(f"Unexpected turn state: {state}")
    ok(f"Got turn {request_id} (state={state})")


def test_delete_turn(chat_id: str, request_id: str) -> None:
    bold("\n8. Delete turn")
    status, data = request(
        "DELETE", f"{PREFIX}/v1/chats/{chat_id}/turns/{request_id}",
    )
    if status not in (200, 204):
        fail(f"Delete turn returned {status}")
    if status == 200 and data:
        print(json.dumps(data, indent=2))
    ok(f"Deleted turn {request_id}")


def test_turns(chat_id: str, messages: list[dict[str, Any]]) -> None:
    """Test turn API using request_ids extracted from messages."""
    if NO_SSE or not messages:
        bold("\n7-8. Turns")
        skip("no messages — turn tests require SSE")
        return

    # Collect unique request_ids (each turn has a user + assistant message pair)
    request_ids = list(dict.fromkeys(m["request_id"] for m in messages))

    # 7. Get turn status for each request_id
    for rid in request_ids:
        test_get_turn(chat_id, rid)

    # 8. Delete the last turn (most recent)
    test_delete_turn(chat_id, request_ids[-1])


def test_upload_attachment(chat_id: str) -> str:
    """Upload a small text file and return the attachment id."""
    bold("\n10. Upload attachment (text file)")
    content = b"Hello from smoke test!\nThis is a sample document for attachment testing."
    status, data = multipart_upload(
        f"{PREFIX}/v1/chats/{chat_id}/attachments",
        filename="smoke-test.txt",
        content=content,
        content_type="text/plain",
        expect_status=201,
    )
    print(json.dumps(data, indent=2))
    att_id = data["id"]
    if not att_id:
        fail("No attachment id returned")
    if data["filename"] != "smoke-test.txt":
        fail(f"Expected filename='smoke-test.txt', got '{data['filename']}'")
    if data["content_type"] != "text/plain":
        fail(f"Expected content_type='text/plain', got '{data['content_type']}'")
    if data["kind"] != "document":
        fail(f"Expected kind='document', got '{data['kind']}'")
    if data["size_bytes"] != len(content):
        fail(f"Expected size_bytes={len(content)}, got {data['size_bytes']}")
    if data["status"] not in ("pending", "uploaded", "ready"):
        fail(f"Unexpected status: {data['status']}")
    ok(f"Uploaded attachment: {att_id} (status={data['status']})")
    return att_id


def test_get_attachment(chat_id: str, attachment_id: str) -> None:
    bold("\n11. Get attachment")
    _, data = request(
        "GET", f"{PREFIX}/v1/chats/{chat_id}/attachments/{attachment_id}",
        expect_status=200,
    )
    print(json.dumps(data, indent=2))
    if data["id"] != attachment_id:
        fail(f"Expected id={attachment_id}, got {data['id']}")
    ok(f"Got attachment: {attachment_id}")


def test_upload_unsupported_type(chat_id: str) -> None:
    bold("\n12. Upload unsupported file type (expect 415)")
    status, data = multipart_upload(
        f"{PREFIX}/v1/chats/{chat_id}/attachments",
        filename="bad-file.exe",
        content=b"\x00\x01\x02\x03",
        content_type="application/octet-stream",
    )
    if status != 415:
        fail(f"Expected 415 for unsupported type, got {status}\n  {data}")
    ok("Correctly rejected unsupported file type (415)")


def test_send_message_with_attachment(
    chat_id: str, attachment_id: str,
) -> str | None:
    bold("\n13. Send message with attachment (SSE stream)")
    if NO_SSE:
        skip("configure a real API key to test")
        return None
    sse = sse_request(
        f"{PREFIX}/v1/chats/{chat_id}/messages:stream",
        {"content": "Summarize the attached file in one sentence.", "attachment_ids": [attachment_id]},
    )
    print(sse.output)
    print(f"  → {sse.text}")
    if sse.tools:
        print(f"  tools: {[t.get('name', '?') for t in sse.tools]}")
    if sse.is_done:
        ok(f"Streamed with attachment (msg_id: {sse.msg_id})")
    elif sse.has_error:
        fail("Stream returned error")
    else:
        fail("Unexpected SSE output")
    return sse.msg_id


def test_messages_have_attachments(chat_id: str) -> None:
    """Verify that listing messages includes attachment summaries."""
    bold("\n14. Verify attachments in message list")
    if NO_SSE:
        skip("no messages to check")
        return
    _, data = request(
        "GET", f"{PREFIX}/v1/chats/{chat_id}/messages", expect_status=200,
    )
    # Find user messages that should have attachments
    user_msgs_with_att = [
        m for m in data["items"]
        if m["role"] == "user" and m.get("attachments")
    ]
    if not user_msgs_with_att:
        fail("Expected at least one user message with attachments")
    att = user_msgs_with_att[0]["attachments"][0]
    print(f"  attachment_id: {att['attachment_id']}, kind: {att['kind']}, filename: {att['filename']}")
    ok(f"Found {len(user_msgs_with_att)} user message(s) with attachments")


def _build_minimal_xlsx() -> bytes:
    """Create a minimal valid XLSX file (stdlib only) with a small data table."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(
            "[Content_Types].xml",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
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
            '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"'
            ' xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
            "<sheets>"
            '<sheet name="Sales" sheetId="1" r:id="rId1"/>'
            "</sheets></workbook>",
        )
        zf.writestr(
            "xl/worksheets/sheet1.xml",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
            "<sheetData>"
            '<row r="1"><c r="A1" t="inlineStr"><is><t>Product</t></is></c>'
            '<c r="B1" t="inlineStr"><is><t>Revenue</t></is></c></row>'
            '<row r="2"><c r="A2" t="inlineStr"><is><t>Widget</t></is></c>'
            '<c r="B2"><v>1500</v></c></row>'
            '<row r="3"><c r="A3" t="inlineStr"><is><t>Gadget</t></is></c>'
            '<c r="B3"><v>2300</v></c></row>'
            "</sheetData></worksheet>",
        )
    return buf.getvalue()


def test_upload_xlsx_attachment(chat_id: str) -> str:
    """Upload an XLSX file and return the attachment id."""
    bold("\n17. Upload XLSX attachment (code interpreter)")
    content = _build_minimal_xlsx()
    _, data = multipart_upload(
        f"{PREFIX}/v1/chats/{chat_id}/attachments",
        filename="sales-data.xlsx",
        content=content,
        content_type="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        expect_status=201,
    )
    print(json.dumps(data, indent=2))
    att_id = data["id"]
    if not att_id:
        fail("No attachment id returned")
    if data["filename"] != "sales-data.xlsx":
        fail(f"Expected filename='sales-data.xlsx', got '{data['filename']}'")
    expected_ct = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    if data["content_type"] != expected_ct:
        fail(f"Expected content_type='{expected_ct}', got '{data['content_type']}'")
    if data["kind"] != "document":
        fail(f"Expected kind='document', got '{data['kind']}'")
    if data["size_bytes"] != len(content):
        fail(f"Expected size_bytes={len(content)}, got {data['size_bytes']}")
    ok(f"Uploaded XLSX attachment: {att_id}")
    return att_id


def test_send_message_with_xlsx(chat_id: str, attachment_id: str) -> str | None:
    """Send a message referencing the XLSX attachment (triggers code_interpreter)."""
    bold("\n18. Send message with XLSX attachment (code interpreter, SSE)")
    if NO_SSE:
        skip("configure a real API key to test")
        return None
    sse = sse_request(
        f"{PREFIX}/v1/chats/{chat_id}/messages:stream",
        {
            "content": "What is the total revenue? Just give me the number.",
            "attachment_ids": [attachment_id],
        },
    )
    print(sse.output)
    print(f"  → {sse.text}")
    tool_names = [t.get("name", "?") for t in sse.tools]
    if tool_names:
        print(f"  tools: {tool_names}")
    if sse.is_done:
        if "code_interpreter" not in tool_names:
            fail(
                f"Expected a code_interpreter tool event for the XLSX flow, got {tool_names}"
            )
        ok(f"Streamed with XLSX attachment (msg_id: {sse.msg_id})")
    elif sse.has_error:
        fail("Stream returned error")
    else:
        fail("Unexpected SSE output")
    return sse.msg_id


def test_delete_attachment_conflict(chat_id: str, attachment_id: str) -> None:
    """Attachment referenced by a message should return 409."""
    bold("\n15. Delete referenced attachment (expect 409)")
    if NO_SSE:
        skip("no message references — attachment not linked")
        return
    status, data = request(
        "DELETE", f"{PREFIX}/v1/chats/{chat_id}/attachments/{attachment_id}",
    )
    if status != 409:
        fail(f"Expected 409 Conflict for referenced attachment, got {status}\n  {data}")
    ok("Correctly refused to delete referenced attachment (409)")


def test_delete_attachment(chat_id: str, attachment_id: str) -> None:
    """Delete an unreferenced attachment."""
    bold("\n16. Delete unreferenced attachment")
    # Upload a fresh attachment that is NOT referenced by any message
    content = b"disposable content"
    _, upload_data = multipart_upload(
        f"{PREFIX}/v1/chats/{chat_id}/attachments",
        filename="disposable.txt",
        content=content,
        content_type="text/plain",
        expect_status=201,
    )
    temp_id = upload_data["id"]
    ok(f"Uploaded temp attachment: {temp_id}")

    status, _ = request(
        "DELETE", f"{PREFIX}/v1/chats/{chat_id}/attachments/{temp_id}",
    )
    if status != 204:
        fail(f"Delete attachment returned {status}")
    ok(f"Deleted attachment {temp_id}")

    # Verify it's gone (404)
    status, _ = request(
        "GET", f"{PREFIX}/v1/chats/{chat_id}/attachments/{temp_id}",
    )
    if status != 404:
        fail(f"Expected 404 after deletion, got {status}")
    ok("Confirmed attachment is gone (404)")


def test_delete_chat(chat_id: str) -> None:
    bold("\n9. Delete test chat")
    status, _ = request("DELETE", f"{PREFIX}/v1/chats/{chat_id}")
    if status != 204:
        fail(f"Delete returned {status}")
    ok(f"Deleted chat {chat_id}")


# -- Main --------------------------------------------------------------------

def main() -> None:
    test_health()

    model_id, _ = test_list_models()
    test_get_model(model_id)

    chat_id = test_create_chat(model_id)
    test_get_chat(chat_id)
    test_update_chat(chat_id)
    test_list_chats(chat_id)

    msg1_id = test_send_message(chat_id, "What is 2+2? Answer with just the number.", "3. Send message 1")
    msg2_id = test_send_message(chat_id, "And what is 3+3?", "4. Send message 2")

    messages = test_list_messages(chat_id, expected_count=4)
    test_reactions(chat_id, msg1_id)
    test_turns(chat_id, messages)

    # -- Attachments (file_search) --
    att_id = test_upload_attachment(chat_id)
    test_get_attachment(chat_id, att_id)
    test_upload_unsupported_type(chat_id)

    # Wait for provider-side vector store indexing
    if not NO_SSE and INDEXING_DELAY > 0:
        print(f"\n  ⏳ Waiting {INDEXING_DELAY}s for provider-side indexing...")
        time.sleep(INDEXING_DELAY)

    test_send_message_with_attachment(chat_id, att_id)
    test_messages_have_attachments(chat_id)
    test_delete_attachment_conflict(chat_id, att_id)
    test_delete_attachment(chat_id, att_id)

    test_delete_chat(chat_id)

    # -- XLSX / Code Interpreter (separate chat to avoid tool competition) --
    xlsx_chat_id = test_create_chat(model_id)
    xlsx_att_id = test_upload_xlsx_attachment(xlsx_chat_id)

    if not NO_SSE and INDEXING_DELAY > 0:
        print(f"\n  ⏳ Waiting {INDEXING_DELAY}s for provider-side file upload...")
        time.sleep(INDEXING_DELAY)

    test_send_message_with_xlsx(xlsx_chat_id, xlsx_att_id)
    test_delete_chat(xlsx_chat_id)

    bold("\nAll checks passed!")


if __name__ == "__main__":
    main()
