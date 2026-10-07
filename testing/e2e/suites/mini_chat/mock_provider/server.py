# Updated: 2026-04-16 by Constructor Tech
"""Mock LLM provider HTTP server — speaks OpenAI Responses API + Files API."""

from __future__ import annotations

import email
import email.policy
import json
import queue
import re
import threading
import time
import urllib.parse
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .responses import (
    Scenario, build_summary_response, extract_last_user_message, match_scenario,
)
from .sse_builder import build_sse_chunks, sent_usage

_response_counter = 0
_counter_lock = threading.Lock()

# Request paths of the two providers of config/base.yaml, as the mock sees
# them after OAGW strips the upstream alias. The Azure Responses route is
# the configured `api_path` (the v1 API: no `api-version`); the Azure Files
# and Vector Stores routes carry the configured `api_version`. OpenAI routes
# are under /v1 and carry no query.
OPENAI_RESPONSES_PATH = "/v1/responses"
AZURE_RESPONSES_PATH = "/openai/v1/responses"
AZURE_API_VERSION = "2025-03-01-preview"


def path_error(raw_path: str) -> str | None:
    """Why `raw_path` is not a path the configured providers use, or None."""
    path, _, query = raw_path.partition("?")
    params = urllib.parse.parse_qs(query, keep_blank_values=True)
    if path.endswith("/responses"):
        if path not in (OPENAI_RESPONSES_PATH, AZURE_RESPONSES_PATH):
            return f"unknown Responses path {path!r}"
        if query:
            return f"unexpected query {query!r} on {path!r}"
        return None
    if path.startswith("/openai/"):
        if params != {"api-version": [AZURE_API_VERSION]}:
            return f"Azure path {path!r} needs exactly api-version={AZURE_API_VERSION}, got {query!r}"
        return None
    if path.startswith("/v1/"):
        return f"unexpected query {query!r} on {path!r}" if query else None
    return f"unknown path {path!r}"


# Path patterns for `set_fault` (matched in full against the path without
# the query): the Files and Vector Stores collections of both providers.
FILES_PATH = r"(?:/v1|/openai)/files"
VECTOR_STORES_PATH = r"(?:/v1|/openai)/vector_stores"


def _not_found(message: str) -> dict:
    return {"error": {"message": message, "type": "invalid_request_error", "param": None, "code": None}}


def _invalid(message: str, param: str | None = None) -> dict:
    return {"error": {"message": message, "type": "invalid_request_error", "param": param, "code": None}}


# The real API answers `in_progress` when a file is added to a vector store
# and reports `completed` on a later status read.
DEFAULT_INDEXING_STATUSES = ("in_progress", "completed")


def _is_summary_request(raw: bytes) -> bool:
    """A non-streaming Responses request: the thread summary worker's."""
    try:
        body = json.loads(raw)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return False
    return isinstance(body, dict) and body.get("stream") is False and "input" in body


def _multipart_form(content_type: str, raw: bytes) -> dict[str, tuple[bytes, str | None]]:
    """{field name: (value, filename)} of a multipart/form-data body."""
    msg = email.message_from_bytes(
        f"Content-Type: {content_type}\r\n\r\n".encode() + raw,
        policy=email.policy.HTTP,
    )
    form: dict[str, tuple[bytes, str | None]] = {}
    if not msg.is_multipart():
        return form
    for part in msg.iter_parts():
        name = part.get_param("name", header="content-disposition")
        if name:
            form[name] = (part.get_payload(decode=True) or b"", part.get_filename())
    return form


def _next_response_id() -> str:
    global _response_counter
    with _counter_lock:
        _response_counter += 1
        return f"resp_mock_{_response_counter}"


class _Handler(BaseHTTPRequestHandler):
    """Handle Responses API (SSE) and Files API (JSON)."""

    def log_message(self, format, *args):
        pass

    def _log_path(self):
        server: MockProviderServer = self.server  # type: ignore[assignment]
        server.log_request_path(self.command, self.path)

    def _inject_fault(self) -> bool:
        """Answer with a fault registered via `set_fault`; True if one matched."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        fault = server.take_fault(self.command, self.path)
        if fault is None:
            return False
        status, body = fault
        self._json_response(status, body)
        return True

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(content_length) if content_length > 0 else b"{}"
        server: MockProviderServer = self.server  # type: ignore[assignment]
        server.log_request_path(self.command, self.path, background=_is_summary_request(raw))

        if self._reject_unknown_path() or self._inject_fault():
            return
        # Vector-store paths first: `/vector_stores/{id}/files` also contains "/files".
        if self.path.split("?")[0].endswith("/responses"):
            self._handle_responses(raw)
        elif "/vector_stores/" in self.path and "/files" in self.path:
            self._handle_vector_store_file_add(raw)
        elif "/vector_stores" in self.path:
            self._handle_vector_store_create(raw)
        elif "/files" in self.path and "/content" not in self.path:
            self._handle_file_upload(raw)
        else:
            self.send_error(404, "Not found")

    def do_GET(self):
        self._log_path()
        if self._reject_unknown_path() or self._inject_fault():
            return
        if "/vector_stores/" in self.path and "/files/" in self.path:
            self._handle_vector_store_file_get()
        elif "/vector_stores/" in self.path:
            self._handle_vector_store_get()
        elif "/files/" in self.path and "/content" in self.path:
            self._handle_file_content()
        elif "/files/" in self.path:
            self._handle_file_get()
        else:
            self.send_error(404, "Not found")

    def do_DELETE(self):
        self._log_path()
        if self._reject_unknown_path() or self._inject_fault():
            return
        if "/vector_stores/" in self.path and "/files/" in self.path:
            self._handle_vector_store_file_delete()
        elif "/vector_stores/" in self.path:
            self._handle_vector_store_delete()
        elif "/files/" in self.path:
            self._handle_file_delete()
        else:
            self.send_error(404, "Not found")

    # ── Responses API ───────────────────────────────────────────────────

    def _handle_responses(self, raw: bytes):
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            body = {}

        model = body.get("model", "unknown")
        response_id = _next_response_id()

        server: MockProviderServer = self.server  # type: ignore[assignment]
        if body.get("stream") is False:
            server.capture_summary_request(body)
            # Non-streaming requests come from background work (thread
            # summary), so they never consume a test's queued scenario.
            fault = server.take_summary_fault((body.get("metadata") or {}).get("chat_id"))
            if fault is not None:
                self._json_response(*fault)
                return
            self._json_response(200, build_summary_response(body, model, response_id))
            return
        server.capture_request(body)
        ref_error = server.unknown_reference(body)
        if ref_error is not None:
            self._json_response(*ref_error)
            return
        try:
            scenario = server._override_queue.get_nowait()
        except queue.Empty:
            user_input = extract_last_user_message(body)
            scenario = match_scenario(user_input)

        if scenario.header_delay:
            # Silent before the status line: trips the gateway's upstream
            # read timeout (OAGW `proxy_timeout_secs`).
            time.sleep(scenario.header_delay)

        # HTTP-level error — return JSON instead of SSE
        if scenario.http_error_status is not None:
            error_body = scenario.http_error_body if scenario.http_error_body is not None else {
                "error": {"message": "Mock error", "type": "mock_error"}
            }
            self._json_response(
                scenario.http_error_status, error_body, scenario.http_error_headers,
            )
            return

        chunks = build_sse_chunks(scenario, model, response_id, request_body=body)
        usage = sent_usage(scenario, body)
        if usage is not None:
            server.capture_sent_usage(usage)

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

        if scenario.slow or scenario.initial_delay or any(d for d, _ in chunks):
            # Stream events with delays so tests can disconnect mid-stream
            try:
                self.wfile.flush()
            except BrokenPipeError:
                return
            time.sleep(scenario.initial_delay)
            for delay, chunk in chunks:
                try:
                    time.sleep(delay)
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    time.sleep(scenario.slow)
                except BrokenPipeError:
                    return
        else:
            self.wfile.write(b"".join(c for _, c in chunks))

    # ── Files API ───────────────────────────────────────────────────────

    def _handle_file_upload(self, raw: bytes):
        """POST /v1/files — accept any upload, return a file object with the
        purpose and filename of the multipart form."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        file_id = f"file-mock-{uuid.uuid4().hex[:12]}"
        form = _multipart_form(self.headers.get("Content-Type", ""), raw)
        content, filename = form.get("file", (b"", None))
        purpose = form.get("purpose", (b"", None))[0].decode(errors="replace")
        if "file" not in form or not filename:
            self._json_response(400, _invalid("A file with a filename is required.", "file"))
            return
        if not purpose:
            self._json_response(400, _invalid("Missing required parameter: 'purpose'.", "purpose"))
            return
        file_obj = {
            "id": file_id,
            "object": "file",
            "bytes": len(content),
            "created_at": int(time.time()),
            "filename": filename,
            "purpose": purpose,
            "status": "processed",
        }
        with server._state_lock:
            server._files[file_id] = file_obj
        self._json_response(200, file_obj)

    def _handle_file_get(self):
        """GET /v1/files/{file_id} — return stored file object."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        file_id = self.path.rstrip("/").split("/")[-1]
        # Strip query params
        file_id = file_id.split("?")[0]
        with server._state_lock:
            file_obj = server._files.get(file_id)
        if file_obj:
            self._json_response(200, file_obj)
        else:
            self._json_response(404, {"error": {"message": f"No such file: {file_id}"}})

    def _handle_file_content(self):
        """GET /v1/files/{file_id}/content — return dummy content."""
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.end_headers()
        self.wfile.write(b"mock file content")

    def _handle_file_delete(self):
        """DELETE /v1/files/{file_id}. Like the real API, deleting a file
        also removes it from every vector store."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        file_id = self.path.rstrip("/").split("/")[-1]
        file_id = file_id.split("?")[0]
        with server._state_lock:
            deleted = server._files.pop(file_id, None) is not None
            if deleted:
                for vs_obj in server._vector_stores.values():
                    ids = vs_obj.get("file_ids", [])
                    if file_id in ids:
                        ids.remove(file_id)
        if not deleted:
            self._json_response(404, _not_found(f"No such File object: {file_id}"))
            return
        self._json_response(200, {"id": file_id, "object": "file", "deleted": True})

    # ── Vector Stores API ───────────────────────────────────────────────

    def _handle_vector_store_create(self, raw: bytes):
        """POST /v1/vector_stores — create a mock vector store."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id = f"vs_mock_{uuid.uuid4().hex[:12]}"
        vs_obj = {
            "id": vs_id,
            "object": "vector_store",
            "name": "mock-store",
            "status": "completed",
            "file_counts": {"in_progress": 0, "completed": 0, "failed": 0, "cancelled": 0, "total": 0},
            "created_at": int(time.time()),
        }
        with server._state_lock:
            server._vector_stores[vs_id] = vs_obj
        self._json_response(200, vs_obj)

    def _handle_vector_store_get(self):
        """GET /v1/vector_stores/{vs_id}."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id = self.path.rstrip("/").split("/")[-1]
        vs_id = vs_id.split("?")[0]
        with server._state_lock:
            vs_obj = server._vector_stores.get(vs_id)
        if vs_obj:
            self._json_response(200, vs_obj)
        else:
            self._json_response(404, {"error": {"message": f"No such vector_store: {vs_id}"}})

    def _handle_vector_store_delete(self):
        """DELETE /v1/vector_stores/{vs_id}."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id = self.path.rstrip("/").split("/")[-1]
        vs_id = vs_id.split("?")[0]
        with server._state_lock:
            deleted = server._vector_stores.pop(vs_id, None) is not None
        if not deleted:
            self._json_response(404, _not_found(f"No vector store found with id '{vs_id}'."))
            return
        self._json_response(200, {"id": vs_id, "object": "vector_store", "deleted": True})

    def _vector_store_file_ids(self) -> tuple[str, str | None]:
        """(vs_id, file_id) from `/…/vector_stores/{vs_id}/files[/{file_id}]`."""
        parts = self.path.split("?")[0].rstrip("/").split("/")
        i = parts.index("vector_stores")
        vs_id = parts[i + 1]
        file_id = parts[i + 3] if len(parts) > i + 3 else None
        return vs_id, file_id

    def _vector_store_file_obj(self, vs_id: str, file_id: str, status: str) -> dict:
        server: MockProviderServer = self.server  # type: ignore[assignment]
        obj = {
            "id": file_id,
            "object": "vector_store.file",
            "vector_store_id": vs_id,
            "status": status,
            "last_error": None,
            "created_at": int(time.time()),
        }
        if status in ("failed", "cancelled"):
            obj["last_error"] = server.indexing_last_error()
        return obj

    def _handle_vector_store_file_add(self, raw: bytes):
        """POST /v1/vector_stores/{vs_id}/files — attach a file to a vector store."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id, _ = self._vector_store_file_ids()
        try:
            file_id = json.loads(raw).get("file_id", "")
        except (json.JSONDecodeError, AttributeError):
            file_id = ""
        with server._state_lock:
            vs_obj = server._vector_stores.get(vs_id)
            file_known = file_id in server._files
            if vs_obj is not None and file_known:
                vs_obj.setdefault("file_ids", []).append(file_id)
        if vs_obj is None:
            self._json_response(404, _not_found(f"No vector store found with id '{vs_id}'."))
            return
        if not file_known:
            self._json_response(404, _not_found(f"No such File object: {file_id}"))
            return
        status = server.start_indexing(vs_id, file_id)
        self._json_response(200, self._vector_store_file_obj(vs_id, file_id, status))

    def _handle_vector_store_file_get(self):
        """GET /v1/vector_stores/{vs_id}/files/{file_id}."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id, file_id = self._vector_store_file_ids()
        with server._state_lock:
            vs_obj = server._vector_stores.get(vs_id)
            found = vs_obj is not None and file_id in vs_obj.get("file_ids", [])
        if found:
            status = server.next_indexing_status(vs_id, file_id)
            self._json_response(200, self._vector_store_file_obj(vs_id, file_id, status))
        else:
            self._json_response(404, {"error": {"message": f"No such vector_store file: {file_id}"}})

    def _handle_vector_store_file_delete(self):
        """DELETE /v1/vector_stores/{vs_id}/files/{file_id} — detach, keep the file."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id, file_id = self._vector_store_file_ids()
        with server._state_lock:
            vs_obj = server._vector_stores.get(vs_id)
            deleted = vs_obj is not None and file_id in vs_obj.get("file_ids", [])
            if deleted:
                vs_obj["file_ids"].remove(file_id)
        if not deleted:
            self._json_response(404, _not_found(f"No file found with id '{file_id}' in vector store '{vs_id}'."))
            return
        self._json_response(
            200, {"id": file_id, "object": "vector_store.file.deleted", "deleted": True},
        )

    # ── Helpers ─────────────────────────────────────────────────────────

    def _reject_unknown_path(self) -> bool:
        """Answer 404 to a path the configured providers do not use (see
        `path_error`) and record it; True if rejected."""
        error = path_error(self.path)
        if error is None:
            return False
        server: MockProviderServer = self.server  # type: ignore[assignment]
        server.record_path_error(self.command, self.path, error)
        self._json_response(404, {"error": {"message": f"mock: {error}", "type": "invalid_request_error"}})
        return True

    def _json_response(self, status: int, body: dict, headers: dict[str, str] | None = None):
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(payload)


class MockProviderServer(ThreadingHTTPServer):
    """Threaded mock LLM provider with per-test scenario override support."""

    name = "mock-provider"
    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), _Handler)
        self._override_queue: queue.Queue[Scenario] = queue.Queue()
        self._thread: threading.Thread | None = None
        self._files: dict[str, dict] = {}
        self._vector_stores: dict[str, dict] = {}
        self._captured_requests: list[dict] = []
        # `response.usage` of each streaming response, as sent.
        self._sent_usages: list[dict] = []
        # Non-streaming Responses requests (thread summary, background work).
        self._summary_requests: list[dict] = []
        self._request_paths: list[tuple[str, str]] = []
        # Requests rejected by `path_error`: (method, path, reason).
        self._path_errors: list[tuple[str, str, str]] = []
        self._capture_lock = threading.Lock()
        # One-shot HTTP faults: [method, path_contains, status, body, remaining].
        self._faults: list[list] = []
        # Faults of the non-streaming (thread summary) Responses requests:
        # [chat_id, status, body, remaining].
        self._summary_faults: list[list] = []
        self._fault_lock = threading.Lock()
        # Guards _files and _vector_stores: handler threads outlive per-test
        # fixtures, and the in-flight delete path is not atomic.
        self._state_lock = threading.Lock()
        # Indexing statuses of a file added to a vector store: the add answers
        # the first, each status read the next; the last one stays.
        self._indexing_statuses: list[str] = list(DEFAULT_INDEXING_STATUSES)
        self._indexing_last_error: dict | None = None
        # (vs_id, file_id) -> statuses still to report.
        self._indexing: dict[tuple[str, str], list[str]] = {}
        # While set, every status read answers `in_progress` (hold_indexing).
        self._indexing_held = False
        # Reject Responses requests that name unknown files or vector stores
        # (check_references).
        self._check_references = True

    @property
    def port(self) -> int:
        return self.server_address[1]

    def capture_request(self, body: dict) -> None:
        """Store a streaming Responses request body (thread-safe)."""
        with self._capture_lock:
            self._captured_requests.append(body)

    def capture_sent_usage(self, usage: dict) -> None:
        with self._capture_lock:
            self._sent_usages.append(usage)

    def get_sent_usages(self) -> list[dict]:
        """`response.usage` of each streaming response the mock sent (the
        turns), oldest first, since the last clear. A response without
        usage (HTTP error, `error` event, `response.failed` without usage)
        adds nothing."""
        with self._capture_lock:
            return [dict(u) for u in self._sent_usages]

    def capture_summary_request(self, body: dict) -> None:
        """Store a non-streaming Responses request body (thread-safe)."""
        with self._capture_lock:
            self._summary_requests.append(body)

    def get_last_request(self) -> dict | None:
        """Return the most recent streaming Responses request body, or None."""
        with self._capture_lock:
            return self._captured_requests[-1] if self._captured_requests else None

    def get_captured_requests(self) -> list[dict]:
        """Streaming Responses API request bodies (the turns), oldest first.

        Leaves out the non-streaming thread summary requests: the summary
        worker runs in the background and may send one during a later test
        (see `get_summary_requests`)."""
        with self._capture_lock:
            return list(self._captured_requests)

    def get_summary_requests(self) -> list[dict]:
        """Non-streaming Responses API request bodies (thread summary), oldest first."""
        with self._capture_lock:
            return list(self._summary_requests)

    def get_uploaded_files(self) -> list[dict]:
        """File objects of the uploads stored since the last `clear_state`,
        with the `purpose` and `filename` of the upload form."""
        with self._state_lock:
            return [dict(f) for f in self._files.values()]

    def log_request_path(self, method: str, path: str, *, background: bool = False) -> None:
        """Record the method and path of a request (thread-safe). A thread
        summary request (`background`) is not recorded: it may come from an
        earlier test."""
        if background:
            return
        with self._capture_lock:
            self._request_paths.append((method, path))

    def get_request_paths(self) -> list[tuple[str, str]]:
        """Return (method, path) of every request since the last clear."""
        with self._capture_lock:
            return list(self._request_paths)

    def get_post_paths(self) -> list[str]:
        """Paths of the POST requests (uploads, vector store writes, Responses
        calls) since the last clear. Leaves out the DELETEs of a background
        cleanup that an earlier test started, which may still be running."""
        return [p for m, p in self.get_request_paths() if m == "POST"]

    def record_path_error(self, method: str, path: str, reason: str) -> None:
        with self._capture_lock:
            self._path_errors.append((method, path, reason))

    def get_path_errors(self) -> list[tuple[str, str, str]]:
        """(method, path, reason) of each request to a path the configured
        providers do not use (answered 404), since the last clear."""
        with self._capture_lock:
            return list(self._path_errors)

    def clear_captured_requests(self) -> None:
        """Clear all captured request bodies, request paths and path errors."""
        with self._capture_lock:
            self._captured_requests.clear()
            self._sent_usages.clear()
            self._summary_requests.clear()
            self._request_paths.clear()
            self._path_errors.clear()

    def set_fault(
        self, method: str, path: str, status: int,
        body: dict | None = None, count: int = 1,
    ) -> None:
        """Answer the next `count` `method` requests whose path (without the
        query) matches the regex `path` in full with `status` and a JSON
        `body`, before normal handling. See FILES_PATH / VECTOR_STORES_PATH."""
        if body is None:
            body = {"error": {"message": f"Mock fault {status}", "type": "mock_fault"}}
        with self._fault_lock:
            self._faults.append([method.upper(), re.compile(path), status, body, count])

    def set_summary_fault(
        self, chat_id: str, status: int, body: dict | None = None, count: int = 1,
    ) -> None:
        """Answer the next `count` thread summary (non-streaming Responses)
        requests of chat `chat_id` (`metadata.chat_id`) with `status` and a
        JSON `body`. Turn requests and other chats' summaries (a background
        summary of an earlier test) are not affected."""
        if body is None:
            body = {"error": {"message": f"Mock fault {status}", "type": "mock_fault"}}
        with self._fault_lock:
            self._summary_faults.append([chat_id, status, body, count])

    def take_summary_fault(self, chat_id: str | None) -> tuple[int, dict] | None:
        with self._fault_lock:
            for fault in self._summary_faults:
                if fault[0] == chat_id:
                    fault[3] -= 1
                    if fault[3] <= 0:
                        self._summary_faults.remove(fault)
                    return fault[1], fault[2]
        return None

    def take_fault(self, method: str, path: str) -> tuple[int, dict] | None:
        """Consume one matching fault; return (status, body) or None."""
        with self._fault_lock:
            for fault in self._faults:
                f_method, f_path, status, body, _ = fault
                if f_method == method and f_path.fullmatch(path.split("?")[0]):
                    fault[4] -= 1
                    if fault[4] <= 0:
                        self._faults.remove(fault)
                    return status, body
        return None

    def set_indexing(self, statuses: list[str], last_error: dict | None = None) -> None:
        """Indexing statuses of the next files added to a vector store: the
        add answers `statuses[0]`, each status read the next one, and the
        last one stays. `last_error` is sent with `failed` / `cancelled`.
        Reset to DEFAULT_INDEXING_STATUSES after each test."""
        assert statuses, "at least one status"
        with self._state_lock:
            self._indexing_statuses = list(statuses)
            self._indexing_last_error = last_error

    def start_indexing(self, vs_id: str, file_id: str) -> str:
        with self._state_lock:
            pending = list(self._indexing_statuses)
            self._indexing[(vs_id, file_id)] = pending
            return pending.pop(0) if len(pending) > 1 else pending[0]

    def hold_indexing(self, held: bool = True) -> None:
        """While held, every vector store file status read answers
        `in_progress`; released, reads continue with the set statuses."""
        with self._state_lock:
            self._indexing_held = held

    def check_references(self, enabled: bool) -> None:
        """Whether a Responses request naming a file (`input_image.file_id`,
        code_interpreter `container.file_ids`) or vector store
        (`file_search.vector_store_ids`) the mock does not hold is rejected
        like the real API (400 / 404). On by default; reset after each test.
        Tests that seed provider ids in the DB turn it off."""
        with self._state_lock:
            self._check_references = enabled

    def unknown_reference(self, body: dict) -> tuple[int, dict] | None:
        """(status, error body) for the first unknown file or vector store
        the request names, or None."""
        with self._state_lock:
            if not self._check_references:
                return None
            files = set(self._files)
            stores = set(self._vector_stores)
        for item in body.get("input") or []:
            if not isinstance(item, dict) or not isinstance(item.get("content"), list):
                continue
            for part in item["content"]:
                if isinstance(part, dict) and part.get("type") == "input_image":
                    fid = part.get("file_id")
                    if fid is not None and fid not in files:
                        return 400, _invalid(f"Invalid file id: '{fid}'.", "input")
        for i, tool in enumerate(body.get("tools") or []):
            if not isinstance(tool, dict):
                continue
            if tool.get("type") == "file_search":
                for vs_id in tool.get("vector_store_ids") or []:
                    if vs_id not in stores:
                        return 404, _not_found(f"Vector store with id '{vs_id}' not found.")
            if tool.get("type") == "code_interpreter":
                container = tool.get("container")
                if isinstance(container, dict):
                    for fid in container.get("file_ids") or []:
                        if fid not in files:
                            return 400, _invalid(
                                f"Invalid file id: '{fid}'.", f"tools[{i}].container.file_ids",
                            )
        return None

    def next_indexing_status(self, vs_id: str, file_id: str) -> str:
        with self._state_lock:
            if self._indexing_held:
                return "in_progress"
            pending = self._indexing.get((vs_id, file_id))
            if not pending:
                return "completed"
            return pending.pop(0) if len(pending) > 1 else pending[0]

    def indexing_last_error(self) -> dict | None:
        with self._state_lock:
            return self._indexing_last_error

    def clear_override_scenarios(self) -> None:
        """Drop queued per-request overrides, faults and the indexing
        statuses left by previous tests."""
        with self._fault_lock:
            self._faults.clear()
            self._summary_faults.clear()
        with self._state_lock:
            self._indexing_statuses = list(DEFAULT_INDEXING_STATUSES)
            self._indexing_last_error = None
            self._indexing_held = False
            self._check_references = True
        while True:
            try:
                self._override_queue.get_nowait()
            except queue.Empty:
                return

    def vector_store_file_ids(self, vs_id: str) -> list[str] | None:
        """Provider file ids added to vector store `vs_id`, or None if the
        mock has no such store (since the last `clear_state`)."""
        with self._state_lock:
            vs_obj = self._vector_stores.get(vs_id)
            return None if vs_obj is None else list(vs_obj.get("file_ids", []))

    def clear_state(self) -> None:
        """Drop file/vector_store state left by previous tests (thread-safe)."""
        with self._state_lock:
            self._files.clear()
            self._vector_stores.clear()
            self._indexing.clear()

    def set_next_scenario(self, scenario: Scenario) -> None:
        """Override the scenario for the next request (consumed once, thread-safe)."""
        self._override_queue.put(scenario)

    def start(self) -> None:
        self._thread = threading.Thread(target=self.serve_forever, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self.shutdown()
        if self._thread is not None:
            self._thread.join(timeout=5)
            self._thread = None


class _DummyMockProvider:
    """No-op stand-in used in online mode."""

    name = "mock-provider"
    port = None

    def set_next_scenario(self, scenario: Scenario) -> None:
        pass

    def set_fault(self, method: str, path: str, status: int,
                  body: dict | None = None, count: int = 1) -> None:
        pass

    def get_last_request(self) -> dict | None:
        return None

    def get_captured_requests(self) -> list[dict]:
        return []

    def get_summary_requests(self) -> list[dict]:
        return []

    def get_sent_usages(self) -> list[dict]:
        return []

    def set_summary_fault(
        self, chat_id: str, status: int, body: dict | None = None, count: int = 1,
    ) -> None:
        pass

    def get_uploaded_files(self) -> list[dict]:
        return []

    def set_indexing(self, statuses: list[str], last_error: dict | None = None) -> None:
        pass

    def hold_indexing(self, held: bool = True) -> None:
        pass

    def check_references(self, enabled: bool) -> None:
        pass

    def get_request_paths(self) -> list[tuple[str, str]]:
        return []

    def get_post_paths(self) -> list[str]:
        return []

    def get_path_errors(self) -> list[tuple[str, str, str]]:
        return []

    def vector_store_file_ids(self, vs_id: str) -> list[str] | None:
        return None

    def clear_captured_requests(self) -> None:
        pass

    def clear_override_scenarios(self) -> None:
        pass

    def clear_state(self) -> None:
        pass

    def start(self) -> None:
        pass

    def stop(self) -> None:
        pass


DummyMockProvider = _DummyMockProvider
