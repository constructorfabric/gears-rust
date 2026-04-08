"""Mock LLM provider HTTP server — speaks OpenAI Responses API + Files API."""

from __future__ import annotations

import json
import queue
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer

from .responses import Scenario, extract_last_user_message, match_scenario
from .sse_builder import build_sse_stream

_response_counter = 0
_counter_lock = threading.Lock()


def _next_response_id() -> str:
    global _response_counter
    with _counter_lock:
        _response_counter += 1
        return f"resp_mock_{_response_counter}"


class _Handler(BaseHTTPRequestHandler):
    """Handle Responses API (SSE) and Files API (JSON)."""

    def log_message(self, format, *args):
        pass

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(content_length) if content_length > 0 else b"{}"

        if "responses" in self.path:
            self._handle_responses(raw)
        elif "/files" in self.path and "/content" not in self.path:
            self._handle_file_upload(raw)
        elif "/vector_stores" in self.path:
            self._handle_vector_store_create(raw)
        else:
            self.send_error(404, "Not found")

    def do_GET(self):
        if "/files/" in self.path and "/content" in self.path:
            self._handle_file_content()
        elif "/files/" in self.path:
            self._handle_file_get()
        elif "/vector_stores/" in self.path:
            self._handle_vector_store_get()
        else:
            self.send_error(404, "Not found")

    def do_DELETE(self):
        if "/files/" in self.path:
            self._handle_file_delete()
        elif "/vector_stores/" in self.path:
            self._handle_vector_store_delete()
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
        server.capture_request(body)
        try:
            scenario = server._override_queue.get_nowait()
        except queue.Empty:
            user_input = extract_last_user_message(body)
            scenario = match_scenario(user_input)

        sse_bytes = build_sse_stream(scenario, model, response_id, request_body=body)

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

        if scenario.slow:
            # Stream events with delays so tests can disconnect mid-stream
            for chunk in sse_bytes.split(b"\n\n"):
                if chunk:
                    try:
                        self.wfile.write(chunk + b"\n\n")
                        self.wfile.flush()
                        time.sleep(scenario.slow)
                    except BrokenPipeError:
                        return
        else:
            self.wfile.write(sse_bytes)

    # ── Files API ───────────────────────────────────────────────────────

    def _handle_file_upload(self, raw: bytes):
        """POST /v1/files — accept any upload, return a file object."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        file_id = f"file-mock-{uuid.uuid4().hex[:12]}"
        file_obj = {
            "id": file_id,
            "object": "file",
            "bytes": len(raw),
            "created_at": int(time.time()),
            "filename": "upload.bin",
            "purpose": "assistants",
            "status": "processed",
        }
        server._files[file_id] = file_obj
        self._json_response(200, file_obj)

    def _handle_file_get(self):
        """GET /v1/files/{file_id} — return stored file object."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        file_id = self.path.rstrip("/").split("/")[-1]
        # Strip query params
        file_id = file_id.split("?")[0]
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
        """DELETE /v1/files/{file_id}."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        file_id = self.path.rstrip("/").split("/")[-1]
        file_id = file_id.split("?")[0]
        deleted = file_id in server._files
        server._files.pop(file_id, None)
        self._json_response(200, {"id": file_id, "object": "file", "deleted": deleted})

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
        server._vector_stores[vs_id] = vs_obj
        self._json_response(200, vs_obj)

    def _handle_vector_store_get(self):
        """GET /v1/vector_stores/{vs_id}."""
        server: MockProviderServer = self.server  # type: ignore[assignment]
        vs_id = self.path.rstrip("/").split("/")[-1]
        vs_id = vs_id.split("?")[0]
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
        deleted = vs_id in server._vector_stores
        server._vector_stores.pop(vs_id, None)
        self._json_response(200, {"id": vs_id, "object": "vector_store", "deleted": deleted})

    # ── Helpers ─────────────────────────────────────────────────────────

    def _json_response(self, status: int, body: dict):
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


class MockProviderServer(HTTPServer):
    """Threaded mock LLM provider with per-test scenario override support."""

    name = "mock-provider"

    def __init__(self):
        super().__init__(("127.0.0.1", 0), _Handler)
        self._override_queue: queue.Queue[Scenario] = queue.Queue()
        self._thread: threading.Thread | None = None
        self._files: dict[str, dict] = {}
        self._vector_stores: dict[str, dict] = {}
        self._captured_requests: list[dict] = []
        self._capture_lock = threading.Lock()

    @property
    def port(self) -> int:
        return self.server_address[1]

    def capture_request(self, body: dict) -> None:
        """Store a request body for later inspection (thread-safe)."""
        with self._capture_lock:
            self._captured_requests.append(body)

    def get_last_request(self) -> dict | None:
        """Return the most recent captured request body, or None."""
        with self._capture_lock:
            return self._captured_requests[-1] if self._captured_requests else None

    def clear_captured_requests(self) -> None:
        """Clear all captured request bodies."""
        with self._capture_lock:
            self._captured_requests.clear()

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

    def get_last_request(self) -> dict | None:
        return None

    def clear_captured_requests(self) -> None:
        pass

    def start(self) -> None:
        pass

    def stop(self) -> None:
        pass


DummyMockProvider = _DummyMockProvider
