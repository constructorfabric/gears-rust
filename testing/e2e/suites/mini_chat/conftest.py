# Updated: 2026-04-16 by Constructor Tech
"""Mini-chat E2E conftest — SSE helpers, provider fixtures, gear test env."""

from __future__ import annotations

import json
import os
import re
import sqlite3
import tempfile
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest
import httpx

from .mock_provider.server import MockProviderServer, DummyMockProvider


# ── Constants ─────────────────────────────────────────────────────────────

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:8087")
API_PREFIX = f"{BASE_URL}/cf/mini-chat/v1"

# NOTE: model_id is unique across providers in this test catalog by convention,
# not by system design. The production model policy plugin could map the same
# model_id to different providers. Tests rely on this uniqueness for simplicity.
DEFAULT_MODEL = "azure-gpt-4.1"       # Premium tier, Azure (production target)
STANDARD_MODEL = "gpt-5.2"            # Standard tier, OpenAI

PROVIDER_DEFAULT_MODEL = {
    "azure": DEFAULT_MODEL,
    "openai": STANDARD_MODEL,
}

# Static bearer tokens (config/base.yaml, static-authn-plugin `static_tokens`).
# User A is the default identity of every request; B shares A's tenant; C is
# in another tenant.
TOKEN_USER_A = "mini-chat-e2e"
TOKEN_USER_B = "mini-chat-e2e-user-b"
TOKEN_TENANT_B = "mini-chat-e2e-tenant-b"
USER_A_ID = "11111111-6a88-4768-9dfc-6bcd5187d9ed"
USER_B_ID = "44444444-6a88-4768-9dfc-6bcd5187d9ed"
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"

# Dedicated users for quota-policy tests (test_quota_policy.py). Their usage
# rows are rewritten directly in the DB, so they must not share state with
# user A. Same tenant as user A.
TOKEN_QUOTA_USER_1 = "mini-chat-e2e-quota-1"
TOKEN_QUOTA_USER_2 = "mini-chat-e2e-quota-2"
TOKEN_QUOTA_USER_3 = "mini-chat-e2e-quota-3"
QUOTA_USER_1_ID = "55555555-6a88-4768-9dfc-6bcd5187d9ed"
QUOTA_USER_2_ID = "66666666-6a88-4768-9dfc-6bcd5187d9ed"
QUOTA_USER_3_ID = "77777777-6a88-4768-9dfc-6bcd5187d9ed"

# Catalog entry with `enabled: false` (config/base.yaml).
DISABLED_MODEL = "gpt-4o-retired"

# Enabled catalog entry without a system prompt and without tools
# (config/base.yaml).
BARE_MODEL = "gpt-5-bare"

# Catalog entry with a 4096-token context window (config/base.yaml):
# thread summary and input-limit tests.
TINY_CTX_MODEL = "gpt-4.1-mini-tiny-ctx"

# TINY_CTX_MODEL with `max_input_tokens: 0` (no separate input limit,
# config/base.yaml).
NO_INPUT_LIMIT_MODEL = "gpt-4.1-mini-tiny-ctx-no-input-limit"

# System prompt of every catalog model (config/base.yaml).
CATALOG_SYSTEM_PROMPT = (
    "You are a helpful assistant. IMPORTANT RULE: When the user says exactly "
    "'PING', you MUST respond with exactly 'PONG' and nothing else."
)

# ContextConfig default `web_search_guard` (gears/mini-chat/mini-chat/src/config.rs),
# appended to the instructions when web search is enabled; not overridden in
# config/base.yaml.
WEB_SEARCH_GUARD = (
    "Use web_search only if the answer cannot be obtained from the provided "
    "context or your training data. Never use it for general knowledge "
    "questions. At most one web_search call per request."
)

# Marker header: a request carrying it is sent without any Authorization
# header (see _default_auth_header).
NO_AUTH = {"X-E2E-No-Auth": "1"}


def auth_headers(token: str) -> dict[str, str]:
    """Authorization header for one of the static tokens."""
    return {"Authorization": f"Bearer {token}"}


_TEMP_HOME = tempfile.mkdtemp(prefix="cf-gears-test-")
DB_PATH = os.path.join(_TEMP_HOME, "mini-chat", "mini_chat.db")

MODULE_DIR = Path(__file__).resolve().parent


# ── SSE helpers ───────────────────────────────────────────────────────────

@dataclass
class SSEEvent:
    """Parsed SSE event."""
    event: str
    data: dict | str = field(default_factory=dict)


def parse_sse(text: str) -> list[SSEEvent]:
    """Parse SSE text into a list of events."""
    events = []
    current_event = None
    current_data_lines: list[str] = []

    for line in text.split("\n"):
        if line.startswith("event:"):
            current_event = line[len("event:"):].strip()
            current_data_lines = []
        elif line.startswith("data:"):
            current_data_lines.append(line[len("data:"):].strip())
        elif line == "" and current_event is not None:
            raw = "\n".join(current_data_lines)
            try:
                data = json.loads(raw)
            except (json.JSONDecodeError, ValueError):
                data = raw
            events.append(SSEEvent(event=current_event, data=data))
            current_event = None
            current_data_lines = []

    if current_event is not None:
        raw = "\n".join(current_data_lines)
        try:
            data = json.loads(raw)
        except (json.JSONDecodeError, ValueError):
            data = raw
        events.append(SSEEvent(event=current_event, data=data))

    return events


def expect_stream_started(events: list[SSEEvent]) -> SSEEvent:
    """Find the 'stream_started' event or fail with diagnostics."""
    for e in events:
        if e.event == "stream_started":
            return e
    event_types = [e.event for e in events]
    raise AssertionError(f"No 'stream_started' event. Events received: {event_types}")


def expect_done(events: list[SSEEvent]) -> SSEEvent:
    """Find the 'done' event or fail with diagnostics."""
    for e in events:
        if e.event == "done":
            return e
    error_events = [e for e in events if e.event == "error"]
    event_types = [e.event for e in events]
    if error_events:
        raise AssertionError(
            f"Stream ended with error instead of done: {error_events[0].data}\n"
            f"Events received: {event_types}"
        )
    raise AssertionError(f"No 'done' event in stream. Events received: {event_types}")


def _log_request(method: str, url: str, body=None, status: int = 0, response_text: str = ""):
    import logging
    log = logging.getLogger("mini-chat-test")
    log.info(f">>> {method} {url}")
    if body:
        log.info(f">>> Body: {json.dumps(body, default=str)[:500]}")
    log.info(f"<<< {status}")
    if response_text:
        log.info(f"<<< {response_text[:500]}")


def delta_text(events: list[SSEEvent]) -> str:
    """Concatenated text of all `delta` events."""
    return "".join(
        e.data.get("content", "") for e in events
        if e.event == "delta" and isinstance(e.data, dict)
    )


# ── Canonical error (RFC 9457 Problem) assertions ────────────────────────

# `Problem.type` is `gts://gts.cf.core.errors.err.v1~cf.core.err.<category>.v1~`
# (libs/toolkit-canonical-errors/src/problem.rs, ProblemCategory::gts_fragment).
PROBLEM_FIELDS = ("type", "title", "status", "detail", "context")

# `context.resource_type` suffixes (the GTS id prefix is configurable, so
# match the tail; see `resource_error` scopes in src/api/rest/error.rs).
RESOURCE_CHAT = "cf.core.mini_chat.chat.v1~"
RESOURCE_MESSAGE = "cf.core.mini_chat.message.v1~"
RESOURCE_TURN = "cf.core.mini_chat.turn.v1~"
RESOURCE_ATTACHMENT = "cf.core.mini_chat.attachment.v1~"
RESOURCE_MODEL = "cf.core.mini_chat.model.v1~"
# `detail` of the 409 `aborted` turn conflicts (turn_conflict_detail in
# src/api/rest/error.rs).

# `$filter` / `$orderby` / `limit` / cursor errors (libs/toolkit-odata/src/errors.rs).
RESOURCE_ODATA = "cf.core.odata.query.v1~"
# Path / query extraction errors of the platform extractors
# (libs/toolkit/src/api/rest/extract/error.rs).
RESOURCE_HTTP_REQUEST = "cf.core.http.request.v1~"


def settled_attachment(chat_id: str, body: dict, timeout: float = 180.0) -> dict:
    """Return the attachment once indexing settled.

    An upload returns `uploaded` when the vector store is still indexing at
    the request deadline (real providers; the mock finishes within the
    request). Poll GET until the status leaves `uploaded`.
    """
    deadline = time.monotonic() + timeout
    while body.get("status") == "uploaded" and time.monotonic() < deadline:
        time.sleep(1.0)
        body = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{body['id']}", timeout=10,
        ).json()
    return body


def assert_problem(
    resp: httpx.Response,
    status: int,
    category: str,
    *,
    reason: str | None = None,
    field_reason: str | None = None,
    violation_type: str | None = None,
    violation_subject: str | None = None,
    resource_type: str | None = None,
) -> dict:
    """Assert `resp` is a canonical Problem of `category` (RFC 9457,
    `Content-Type: application/problem+json`) and return its body.

    - `reason`: `context.reason` (aborted, permission_denied).
    - `field_reason`: some `context.field_violations[].reason`
      (invalid_argument, out_of_range).
    - `violation_type` / `violation_subject`: one `context.violations[]` entry
      matches both given values (failed_precondition: subject/type;
      resource_exhausted: subject only).
    - `resource_type`: `context.resource_type` ends with this value (use the
      `RESOURCE_*` constants).

    `detail` is human-readable and not part of the contract, so it is not
    asserted.
    """
    assert resp.status_code == status, (
        f"expected HTTP {status}, got {resp.status_code}: {resp.text[:500]}"
    )
    media_type = resp.headers.get("content-type", "").split(";")[0].strip()
    assert media_type == "application/problem+json", (
        f"expected Content-Type application/problem+json, got {media_type!r}"
    )
    body = resp.json()
    for key in PROBLEM_FIELDS:
        assert key in body, f"Problem is missing {key!r}: {body}"
    assert "code" not in body, f"Problem must not carry a top-level 'code': {body}"
    assert body["status"] == status, body
    suffix = f"cf.core.err.{category}.v1~"
    assert body["type"].endswith(suffix), (
        f"expected Problem type ending with {suffix!r}, got {body['type']!r}"
    )
    ctx = body["context"]
    if reason is not None:
        assert ctx.get("reason") == reason, (
            f"expected context.reason={reason!r}, got {ctx.get('reason')!r}: {body}"
        )
    if resource_type is not None:
        actual = ctx.get("resource_type") or ""
        assert actual.endswith(resource_type), (
            f"expected context.resource_type ending with {resource_type!r}, "
            f"got {actual!r}: {body}"
        )
    if field_reason is not None:
        reasons = [v.get("reason") for v in ctx.get("field_violations", [])]
        assert field_reason in reasons, (
            f"expected field_violations reason {field_reason!r}, got {reasons}: {body}"
        )
    if violation_type is not None or violation_subject is not None:
        violations = ctx.get("violations", [])
        matching = [
            v for v in violations
            if (violation_type is None or v.get("type") == violation_type)
            and (violation_subject is None or v.get("subject") == violation_subject)
        ]
        assert matching, (
            f"expected a violation with type={violation_type!r} "
            f"subject={violation_subject!r}, got {violations}: {body}"
        )
    return body


# ── Direct DB access (sqlite, offline rig) ───────────────────────────────

def _to_blob(value):
    """UUID strings are stored as 16-byte blobs; bind them the same way."""
    if isinstance(value, str):
        try:
            return uuid.UUID(value).bytes
        except ValueError:
            pass
    return value


def _require_db() -> None:
    if not os.path.exists(DB_PATH):
        pytest.fail(f"mini-chat DB not found at {DB_PATH}")


def query_db(sql: str, params: tuple = ()) -> list[dict]:
    """Run a read-only query against the mini-chat DB. UUID params bind as blobs."""
    _require_db()
    conn = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True, timeout=10)
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(sql, tuple(_to_blob(p) for p in params)).fetchall()
        return [dict(r) for r in rows]
    finally:
        conn.close()


def exec_db(sql: str, params: tuple = ()) -> int:
    """Run one write statement against the mini-chat DB; return rowcount.

    Only for deterministic test seeding (quota usage, chat model).
    """
    _require_db()
    conn = sqlite3.connect(DB_PATH, timeout=10)
    try:
        cur = conn.execute(sql, tuple(_to_blob(p) for p in params))
        conn.commit()
        return cur.rowcount
    finally:
        conn.close()


def uuid_from_db(value) -> str | None:
    """Normalize a UUID column value (blob or text) to its string form."""
    if value is None:
        return None
    if isinstance(value, (bytes, bytearray)):
        return str(uuid.UUID(bytes=bytes(value)))
    return str(value)


def turn_count(chat_id: str) -> int:
    """Number of chat_turns rows of a chat (soft-deleted rows included)."""
    return query_db(
        "SELECT COUNT(*) AS n FROM chat_turns WHERE chat_id = ?", (chat_id,),
    )[0]["n"]


def provider_file_id(attachment_id: str) -> str:
    """The provider file id stored for an attachment (never exposed over REST)."""
    rows = query_db("SELECT provider_file_id FROM attachments WHERE id = ?", (attachment_id,))
    assert len(rows) == 1 and rows[0]["provider_file_id"], rows
    return rows[0]["provider_file_id"]


# ── Outbox capture ───────────────────────────────────────────────────────
#
# The toolkit-db outbox deletes a message's `toolkit_outbox_body` row about
# a second after its handler succeeded (vacuum worker,
# libs/toolkit-db/src/outbox/workers/vacuum.rs; its tuning is not part of
# the mini-chat config), so the body table cannot tell "never enqueued" from
# "already handled". The handlers keep nothing either: usage events go to
# the static model policy plugin, audit events to the static audit plugin.
#
# Instead a SQLite trigger on `toolkit_outbox_incoming` copies each message
# (queue and payload) into E2E_OUTBOX_CAPTURE when it is enqueued. The
# trigger runs inside the enqueuing transaction, so a message is captured
# exactly when it commits, and the copy is never vacuumed. Once the
# transaction that would enqueue a message has committed (e.g. the turn is
# terminal), its absence from the capture means it was not enqueued.

E2E_OUTBOX_CAPTURE = "e2e_outbox_capture"

# Outbox queue names (OutboxConfig defaults, src/config.rs; base.yaml does
# not override them).
USAGE_QUEUE = "mini-chat.usage_snapshot"
ATTACHMENT_CLEANUP_QUEUE = "mini-chat.attachment_cleanup"
CHAT_CLEANUP_QUEUE = "mini-chat.chat_cleanup"
THREAD_SUMMARY_QUEUE = "mini-chat.thread_summary"

_OUTBOX_TABLES = ("toolkit_outbox_body", "toolkit_outbox_incoming", "toolkit_outbox_partitions")


def _install_outbox_capture(timeout: float = 60.0) -> None:
    """Create the capture table and trigger once the outbox schema exists."""
    deadline = time.monotonic() + timeout
    while True:
        if os.path.exists(DB_PATH):
            names = {
                r["name"] for r in query_db("SELECT name FROM sqlite_master WHERE type = 'table'")
            }
            if all(t in names for t in _OUTBOX_TABLES):
                break
        if time.monotonic() >= deadline:
            raise RuntimeError(f"outbox tables {_OUTBOX_TABLES} not created within {timeout}s")
        time.sleep(0.2)
    conn = sqlite3.connect(DB_PATH, timeout=10)
    try:
        conn.executescript(f"""
            CREATE TABLE IF NOT EXISTS {E2E_OUTBOX_CAPTURE} (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                body_id INTEGER NOT NULL,
                queue   TEXT    NOT NULL,
                payload BLOB    NOT NULL
            );
            CREATE TRIGGER IF NOT EXISTS {E2E_OUTBOX_CAPTURE}_on_enqueue
            AFTER INSERT ON toolkit_outbox_incoming
            BEGIN
                INSERT INTO {E2E_OUTBOX_CAPTURE} (body_id, queue, payload)
                SELECT b.id, p.queue, b.payload
                FROM toolkit_outbox_body b, toolkit_outbox_partitions p
                WHERE b.id = NEW.body_id AND p.id = NEW.partition_id;
            END;
        """)
    finally:
        conn.close()


def outbox_payloads(needle: str, queue: str | None = None) -> list[dict]:
    """JSON payloads of the outbox messages enqueued so far (see "Outbox
    capture") that contain `needle`, oldest first; only `queue` if given."""
    # `payload` is a BLOB. The CAST is required: SQLite built with
    # SQLITE_LIKE_DOESNT_MATCH_BLOBS (Debian/Ubuntu system libsqlite3, used by
    # the CI Python) never matches LIKE against a BLOB.
    sql = f"SELECT payload FROM {E2E_OUTBOX_CAPTURE} WHERE CAST(payload AS TEXT) LIKE ?"
    params: tuple = (f"%{needle}%",)
    if queue is not None:
        sql += " AND queue = ?"
        params += (queue,)
    rows = query_db(sql + " ORDER BY id", params)
    return [json.loads(r["payload"]) for r in rows]


def usage_events(request_id: str) -> list[dict]:
    """Usage events enqueued for the turn with `request_id`."""
    return [
        p for p in outbox_payloads(request_id, USAGE_QUEUE)
        if p.get("request_id") == request_id
    ]


def chat_cleanup_payloads(chat_id: str) -> list[dict]:
    """Chat soft-delete cleanup messages enqueued for `chat_id`."""
    return [
        p for p in outbox_payloads(chat_id, CHAT_CLEANUP_QUEUE)
        if p.get("chat_id") == chat_id and p.get("reason") == "chat_soft_delete"
    ]


def wait_for(predicate, what: str, timeout: float = 10.0, interval: float = 0.2):
    """Poll `predicate()` until it returns a truthy value; return that value."""
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise AssertionError(f"timed out after {timeout}s waiting for {what}")
        time.sleep(interval)


def wait_cleanup_terminal(attachment_ids: list[str], timeout: float = 20.0) -> dict[str, str]:
    """Poll until the cleanup_status of every attachment is `done` or `failed`;
    return {attachment_id: cleanup_status}."""
    def statuses() -> dict[str, str]:
        out = {}
        for att_id in attachment_ids:
            rows = query_db("SELECT cleanup_status FROM attachments WHERE id = ?", (att_id,))
            assert len(rows) == 1, rows
            out[att_id] = rows[0]["cleanup_status"]
        return out

    deadline = time.monotonic() + timeout
    current = statuses()
    while not all(v in ("done", "failed") for v in current.values()):
        if time.monotonic() >= deadline:
            raise AssertionError(f"cleanup not terminal within {timeout}s: {current}")
        time.sleep(0.2)
        current = statuses()
    return current


def reserved_credits_total(user_id: str = USER_A_ID) -> int:
    """SUM(quota_usage.reserved_credits_micro) over all rows of a user."""
    rows = query_db(
        "SELECT COALESCE(SUM(reserved_credits_micro), 0) AS total "
        "FROM quota_usage WHERE user_id = ?",
        (user_id,),
    )
    return rows[0]["total"]


def assert_no_reserves(user_id: str = USER_A_ID, timeout: float = 5.0) -> None:
    """Poll until the user's quota reserves are fully released."""
    deadline = time.monotonic() + timeout
    total = reserved_credits_total(user_id)
    while total != 0 and time.monotonic() < deadline:
        time.sleep(0.1)
        total = reserved_credits_total(user_id)
    assert total == 0, f"quota reserve not released for {user_id}: {total} credits_micro"


# ── Turn / stream helpers ─────────────────────────────────────────────────

TERMINAL_STATES = ("done", "error", "cancelled")


def poll_turn(
    chat_id: str,
    request_id: str,
    states: tuple[str, ...] = TERMINAL_STATES,
    *,
    timeout: float = 15.0,
    token: str = TOKEN_USER_A,
) -> dict:
    """Poll GET /turns/{request_id} until its state is one of `states`."""
    deadline = time.monotonic() + timeout
    body = None
    while time.monotonic() < deadline:
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}",
            headers=auth_headers(token), timeout=5,
        )
        if resp.status_code == 200:
            body = resp.json()
            if body["state"] in states:
                return body
        time.sleep(0.2)
    raise AssertionError(
        f"turn {request_id} did not reach {states} within {timeout}s (last: {body})"
    )


def slow_scenario(n_deltas: int = 20, *, slow: float = 0.3, prefix: str = "w"):
    """Mock scenario with `n_deltas` deltas and `slow` seconds between events."""
    from .mock_provider.responses import MockEvent, Scenario
    events = [
        MockEvent("response.output_text.delta", {"delta": f"{prefix}{i} "})
        for i in range(n_deltas)
    ]
    text = "".join(f"{prefix}{i} " for i in range(n_deltas))
    events.append(MockEvent("response.output_text.done", {"text": text}))
    return Scenario(events=events, slow=slow)


class OpenStream:
    """An SSE stream kept open by the test.

    `read_until(pred)` parses events as they arrive. Leaving the context
    closes the connection (a client disconnect) unless `drain()` read the
    stream to its end first.
    """

    def __init__(self, url: str, body: dict | None, *, token: str = TOKEN_USER_A,
                 method: str = "POST"):
        self._client = httpx.Client(timeout=60)
        self._cm = self._client.stream(
            method, url, json=body,
            headers={"Accept": "text/event-stream", **auth_headers(token)},
        )
        self.resp = None
        self.events: list[SSEEvent] = []
        self._lines = None
        self._event = None
        self._data: list[str] = []

    def __enter__(self) -> "OpenStream":
        self.resp = self._cm.__enter__()
        assert self.resp.status_code == 200, (
            f"stream failed: {self.resp.status_code} {self.resp.read()[:500]!r}"
        )
        self._lines = self.resp.iter_lines()
        return self

    def __exit__(self, *exc):
        try:
            self._cm.__exit__(*exc)
        finally:
            self._client.close()

    def _next_event(self) -> SSEEvent | None:
        for line in self._lines:
            if line.startswith("event:"):
                self._event = line[len("event:"):].strip()
                self._data = []
            elif line.startswith("data:"):
                self._data.append(line[len("data:"):].strip())
            elif line == "" and self._event is not None:
                raw = "\n".join(self._data)
                try:
                    data = json.loads(raw)
                except (json.JSONDecodeError, ValueError):
                    data = raw
                ev = SSEEvent(event=self._event, data=data)
                self._event = None
                self.events.append(ev)
                return ev
        return None

    def read_until(self, pred) -> SSEEvent:
        """Read events until `pred(event)` is true; return that event."""
        while True:
            ev = self._next_event()
            if ev is None:
                raise AssertionError(
                    f"stream ended before the expected event: {[e.event for e in self.events]}"
                )
            if pred(ev):
                return ev

    def read_until_started(self) -> SSEEvent:
        return self.read_until(lambda e: e.event == "stream_started")

    def drain(self) -> list[SSEEvent]:
        """Read the stream to its end and return all events."""
        while self._next_event() is not None:
            pass
        return self.events


def open_stream(chat_id: str, content: str, *, request_id: str | None = None,
                token: str = TOKEN_USER_A, **extra) -> OpenStream:
    body = {"content": content, **extra}
    if request_id is not None:
        body["request_id"] = request_id
    return OpenStream(
        f"{API_PREFIX}/chats/{chat_id}/messages:stream", body, token=token,
    )


def provider_input(body: dict) -> list[tuple[str, str]]:
    """(role, text) of each message in a captured Responses API `input`."""
    items = body.get("input") or []
    if isinstance(items, str):
        return [("user", items)]
    out = []
    for item in items:
        if not isinstance(item, dict) or "role" not in item:
            continue
        content = item.get("content", "")
        if isinstance(content, list):
            text = "".join(
                part.get("text", "") for part in content if isinstance(part, dict)
            )
        else:
            text = str(content)
        out.append((item["role"], text))
    return out


def list_messages(chat_id: str, *, token: str = TOKEN_USER_A) -> list[dict]:
    resp = httpx.get(
        f"{API_PREFIX}/chats/{chat_id}/messages",
        params={"limit": 100}, headers=auth_headers(token), timeout=10,
    )
    assert resp.status_code == 200, resp.text
    return resp.json()["items"]


def provider_usage(mock_provider, done_usage: dict) -> dict:
    """{input_tokens, output_tokens} the provider reported for the last turn,
    for computing its expected credits.

    Offline: the `response.usage` the mock sent in its last streaming
    response, which must also be the `done` usage; a charge computed from
    it catches a server that reads the wrong token source for both `done`
    and settlement. Online (no mock): the `done` usage."""
    reported = {k: done_usage[k] for k in ("input_tokens", "output_tokens")}
    if getattr(mock_provider, "port", None) is None:
        return reported
    sent = mock_provider.get_sent_usages()
    assert sent, "the mock sent no usage"
    expected = {k: sent[-1][k] for k in ("input_tokens", "output_tokens")}
    assert reported == expected, f"done usage {reported} != provider usage {expected}"
    return expected


def get_quota_status(*, token: str = TOKEN_USER_A) -> dict:
    resp = httpx.get(f"{API_PREFIX}/quota/status", headers=auth_headers(token), timeout=10)
    assert resp.status_code == 200, f"GET /quota/status: {resp.status_code} {resp.text}"
    return resp.json()


def find_period(status: dict, tier: str, period: str) -> dict:
    for t in status["tiers"]:
        if t["tier"] == tier:
            for p in t["periods"]:
                if p["period"] == period:
                    return p
    raise AssertionError(f"no {tier}/{period} period in {status}")


def poll_until(call, *, until, timeout: int = 60):
    """Generic polling helper. call() returns an httpx.Response, until(resp) returns bool."""
    import time
    deadline = time.monotonic() + timeout
    resp = None
    while time.monotonic() < deadline:
        resp = call()
        assert resp.status_code == 200, f"Poll failed: {resp.status_code} {resp.text}"
        if until(resp):
            return resp
        time.sleep(1)
    raise TimeoutError(
        f"Polling timed out after {timeout}s. Last response: {resp.text[:200] if resp else 'none'}"
    )


def stream_message(
    chat_id: str, content: str, *, token: str = TOKEN_USER_A, **kwargs,
) -> tuple[int, list[SSEEvent], str]:
    """Send a streaming message and return (status_code, events, raw_body)."""
    body = {"content": content, **kwargs}
    url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
    resp = httpx.post(
        url, json=body,
        headers={"Accept": "text/event-stream", **auth_headers(token)},
        timeout=90,
    )
    raw = resp.text
    _log_request("POST", url, body, resp.status_code, raw)
    events = parse_sse(raw) if resp.status_code == 200 else []
    return resp.status_code, events, raw


# ── Config patching ───────────────────────────────────────────────────────

_REQUIRED_ONLINE = ["OPENAI_API_KEY", "AZURE_OPENAI_API_KEY"]


def _patch_mini_chat_config(config_text: str, env) -> str:
    """Patch mini-chat config based on mode (offline/online)."""
    from .config.generator import load_credentials

    # home_dir
    config_text = re.sub(r"(home_dir\s*:\s*).*", rf'\1"{_TEMP_HOME}"', config_text, count=1)

    # Log level — mini_chat logging is already in base.yaml; only inject oagw
    mini_chat_log = os.environ.get("MINI_CHAT_LOG", "debug")
    log_inject = (
        f"  oagw:\n"
        f"    console_level: {mini_chat_log}\n"
    )
    config_text = config_text.replace("  api-gateway:", log_inject + "  api-gateway:", 1)

    # Find mock provider sidecar (if any)
    mock_port = None
    for sc in env.sidecars:
        if sc.name == "mock-provider" and sc.port is not None:
            mock_port = sc.port
            break

    if mock_port is not None:
        # Offline mode — patch hosts to mock
        mock_host = "127.0.0.1"
        config_text = config_text.replace('host: "api.openai.com"', f'host: "{mock_host}"', 1)
        match = re.search(r'(azure_openai:.*?host:\s*")([^"]+)(")', config_text, re.DOTALL)
        if match:
            config_text = config_text[:match.start(2)] + mock_host + config_text[match.end(2):]

        # Inject port/use_http/upstream_alias
        for marker, alias in [("openai:", "mock-openai"), ("azure_openai:", "mock-azure")]:
            m = re.search(rf"({marker}.*?)(host:\s*\"[^\"]+\")", config_text, re.DOTALL)
            if m:
                inject = (
                    f'\n          port: {mock_port}'
                    f'\n          use_http: true'
                    f'\n          upstream_alias: "{alias}"'
                )
                config_text = config_text[:m.end(2)] + inject + config_text[m.end(2):]

        # Enable HTTP upstream
        if "allow_http_upstream" in config_text:
            config_text = config_text.replace("allow_http_upstream: false", "allow_http_upstream: true")
        else:
            config_text = config_text.replace(
                "proxy_timeout_secs:", "allow_http_upstream: true\n      proxy_timeout_secs:",
            )

        # Dummy creds
        config_text = config_text.replace("REPLACE_WITH_OPENAI_KEY", "mock-key-openai")
        config_text = config_text.replace("REPLACE_WITH_AZURE_KEY", "mock-key-azure")
    else:
        # Online mode — real creds from env
        creds = load_credentials()
        # The rig's 8 s upstream read timeout fits the mock; a real provider
        # can stay silent longer (web search, first token).
        config_text = config_text.replace("proxy_timeout_secs: 8", "proxy_timeout_secs: 60", 1)
        config_text = config_text.replace("REPLACE_WITH_OPENAI_KEY", creds.get("OPENAI_API_KEY", ""))
        config_text = config_text.replace("REPLACE_WITH_AZURE_KEY", creds.get("AZURE_OPENAI_API_KEY", ""))
        azure_host = creds.get("AZURE_OPENAI_HOST")
        if azure_host:
            match = re.search(r'(azure_openai:.*?host:\s*")([^"]+)(")', config_text, re.DOTALL)
            if match:
                config_text = config_text[:match.start(2)] + azure_host + config_text[match.end(2):]

    return config_text


# ── DB summary ────────────────────────────────────────────────────────────

def _print_db_summary() -> str | None:
    _lines: list[str] = []

    def out(msg=""):
        _lines.append(msg)

    if not os.path.exists(DB_PATH):
        return f"!! DB summary skipped: {DB_PATH} does not exist"

    try:
        conn = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
        conn.row_factory = sqlite3.Row
    except Exception as exc:
        return f"!! DB summary skipped: cannot open DB: {exc}"

    sep = "=" * 110
    out(f"\n{sep}")
    out("  POST-RUN DB SUMMARY")
    out(f"  DB: {DB_PATH}")
    out(sep)

    try:
        rows = [dict(r) for r in conn.execute(
            "SELECT * FROM quota_usage ORDER BY period_type, bucket"
        ).fetchall()]
        if rows:
            out(f"\n  QUOTA_USAGE ({len(rows)} rows)")
            out(
                f"  {'period':<8} {'bucket':<16} {'spent_cr':>12} {'reserved_cr':>12} "
                f"{'calls':>6} {'in_tok':>8} {'out_tok':>9} {'ws_calls':>9} "
                f"{'fs_calls':>9} {'rag_calls':>10} {'img_in':>7} {'img_bytes':>10}"
            )
            out(f"  {'-' * 96}")
            for q in rows:
                out(
                    f"  {q['period_type']:<8} "
                    f"{q['bucket']:<16} "
                    f"{q['spent_credits_micro']:>12} "
                    f"{q['reserved_credits_micro']:>12} "
                    f"{q['calls']:>6} "
                    f"{q['input_tokens']:>8} "
                    f"{q['output_tokens']:>9} "
                    f"{q['web_search_calls']:>9} "
                    f"{q['file_search_calls']:>9} "
                    f"{q['rag_retrieval_calls']:>10} "
                    f"{q['image_inputs']:>7} "
                    f"{q['image_upload_bytes']:>10}"
                )
            stuck = [q for q in rows if q["reserved_credits_micro"] != 0]
            total_daily = [q for q in rows if q["bucket"] == "total" and q["period_type"] == "daily"]
            if stuck:
                out(f"\n  !! STUCK RESERVES: {len(stuck)} rows with reserved_credits_micro != 0")
            else:
                out("\n  OK No stuck reserves (all reserved_credits_micro = 0)")
            if total_daily:
                td = total_daily[0]
                out(f"  OK Daily totals: {td['calls']} calls, "
                    f"{td['input_tokens']} in_tok, {td['output_tokens']} out_tok, "
                    f"{td['web_search_calls']} ws_calls, "
                    f"{td['spent_credits_micro']} credits spent")
    except Exception as exc:
        out(f"\n  !! quota_usage query failed: {exc}")

    try:
        turns = [dict(r) for r in conn.execute(
            "SELECT t.state, t.effective_model, "
            "       t.reserve_tokens, t.max_output_tokens_applied, "
            "       t.reserved_credits_micro, t.error_code, "
            "       m.input_tokens AS actual_in, m.output_tokens AS actual_out "
            "FROM chat_turns t "
            "LEFT JOIN messages m ON m.id = t.assistant_message_id "
            "WHERE t.deleted_at IS NULL ORDER BY t.started_at"
        ).fetchall()]
        if turns:
            WS_THRESHOLD = 2000
            groups: dict[tuple, dict] = {}
            for t in turns:
                model = t["effective_model"] or "?"
                state = t["state"]
                ws = "ws" if (t["actual_in"] or 0) > WS_THRESHOLD else "plain"
                err = t["error_code"] or ""
                key = (model, state, ws, err)
                if key not in groups:
                    groups[key] = {
                        "count": 0, "reserve_tok": [], "reserved_cr": [],
                        "actual_in": [], "actual_out": [],
                    }
                g = groups[key]
                g["count"] += 1
                g["reserve_tok"].append(t["reserve_tokens"] or 0)
                g["reserved_cr"].append(t["reserved_credits_micro"] or 0)
                g["actual_in"].append(t["actual_in"] or 0)
                g["actual_out"].append(t["actual_out"] or 0)

            out(f"\n  CHAT_TURNS ({len(turns)} turns, {len(groups)} groups)")
            out(
                f"  {'model':<14} {'state':<10} {'type':<6} {'cnt':>4} "
                f"{'reserve_tok':>11} {'reserved_cr':>12} "
                f"{'avg_in':>8} {'avg_out':>9} {'error':>16}"
            )
            out(f"  {'-' * 96}")
            for (model, state, ws, err), g in groups.items():
                n = g["count"]
                avg_in = sum(g["actual_in"]) // n
                avg_out = sum(g["actual_out"]) // n
                avg_res_tok = sum(g["reserve_tok"]) // n
                avg_res_cr = sum(g["reserved_cr"]) // n
                out(
                    f"  {model:<14} {state:<10} {ws:<6} {n:>4} "
                    f"{avg_res_tok:>11} {avg_res_cr:>12} "
                    f"{avg_in:>8} {avg_out:>9} {err:>16}"
                )

            states: dict[str, int] = {}
            for t in turns:
                states[t["state"]] = states.get(t["state"], 0) + 1
            out(f"\n  Total: {', '.join(f'{v} {k}' for k, v in states.items())}")
    except Exception as exc:
        out(f"\n  !! chat_turns query failed: {exc}")

    try:
        msg_stats = conn.execute(
            "SELECT role, COUNT(*) as cnt, "
            "       SUM(input_tokens) as total_in, SUM(output_tokens) as total_out "
            "FROM messages WHERE deleted_at IS NULL GROUP BY role"
        ).fetchall()
        if msg_stats:
            out("\n  MESSAGES")
            for m in msg_stats:
                m = dict(m)
                out(f"  {m['role']:<12} {m['cnt']} messages, "
                    f"in_tok={m['total_in']}, out_tok={m['total_out']}")
    except Exception as exc:
        out(f"\n  !! messages query failed: {exc}")

    conn.close()
    out(f"\n{sep}\n")
    return "\n".join(_lines)


# ── pytest hooks ──────────────────────────────────────────────────────────

def pytest_configure(config):
    """Register mini-chat markers."""
    config.addinivalue_line("markers", "openai: Tests targeting OpenAI provider")
    config.addinivalue_line("markers", "azure: Tests targeting Azure OpenAI provider")
    config.addinivalue_line(
        "markers",
        "multi_provider: Tests parameterized over the OpenAI and Azure providers "
        "(`provider` / `provider_chat` fixtures) or using a chat of each provider",
    )
    config.addinivalue_line("markers", "online_only: Tests that require real cloud (skipped in offline mode)")


def pytest_addoption(parser):
    """Register mini-chat E2E mode flag."""
    parser.addoption(
        "--mode",
        choices=["offline", "online"],
        default="offline",
        help="offline = mock LLM provider (default, no keys); online = real cloud providers",
    )


# Lower bound of a test's timeout in online mode (see pytest_collection_modifyitems).
ONLINE_MIN_TIMEOUT_SECS = 300.0


def pytest_collection_modifyitems(config, items):
    """Auto-skip online_only tests in offline mode; time the test body only.

    pytest-timeout times setup, call and teardown together by default, so
    the session fixtures (server start, credstore provisioning; each bounded
    by its own wait) would count against whichever test runs first. Each
    mini-chat test gets its timeout (its own `timeout` marker, else the ini
    value) with `func_only=True`. In online mode each timeout is raised to at
    least ONLINE_MIN_TIMEOUT_SECS: the offline budgets fit the mock, and a
    real provider takes seconds per answer.
    """
    if config.getoption("mode") == "offline":
        skip = pytest.mark.skip(reason="requires --mode online")
        for item in items:
            if "online_only" in item.keywords:
                item.add_marker(skip)
    else:
        # A provider-parameterized test runs only when that provider has a key.
        from .config.generator import provider_key_missing
        for item in items:
            for provider in ("openai", "azure"):
                if provider in item.keywords and provider_key_missing(provider):
                    item.add_marker(pytest.mark.skip(reason=f"no {provider} key in online mode"))
    default_timeout = float(config.getini("timeout") or 0)
    for item in items:
        if MODULE_DIR not in Path(str(item.path)).parents:
            continue
        marker = item.get_closest_marker("timeout")
        timeout = marker.args[0] if marker is not None and marker.args else (
            marker.kwargs.get("timeout", default_timeout) if marker is not None
            else default_timeout
        )
        if config.getoption("mode") == "online" and timeout:
            timeout = max(float(timeout), ONLINE_MIN_TIMEOUT_SECS)
        item.add_marker(pytest.mark.timeout(timeout, func_only=True), append=False)


@pytest.fixture(scope="session", autouse=True)
def _check_mini_chat_binary():
    """Skip all mini-chat tests when E2E_BINARY is not set.

    In CI, make e2e-local runs all gears against the shared server.
    Mini-chat needs its own binary (different features), built separately
    via make e2e-mini-chat. Without E2E_BINARY, skip gracefully.
    """
    if not os.environ.get("E2E_BINARY"):
        pytest.skip(
            "E2E_BINARY not set — run mini-chat tests via: make e2e-mini-chat",
            allow_module_level=True,
        )


_db_summary_text: str | None = None


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    if _db_summary_text:
        terminalreporter.write_line("")
        for line in _db_summary_text.split("\n"):
            terminalreporter.write_line(line)


# ── Authentication ──────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def _default_auth_header():
    """Send every request as user A unless it sets its own Authorization.

    The rig runs with authentication enabled. Patching ``httpx.Client.send``
    covers ``httpx.get/post/...``, ``httpx.stream`` and explicit clients, so
    existing tests keep working unchanged. A request with the ``NO_AUTH``
    marker header is sent without credentials.
    """
    original_send = httpx.Client.send

    def send(self, request, *args, **kwargs):
        if request.headers.pop("X-E2E-No-Auth", None) is None and (
            "authorization" not in request.headers
        ):
            request.headers["Authorization"] = f"Bearer {TOKEN_USER_A}"
        return original_send(self, request, *args, **kwargs)

    httpx.Client.send = send
    yield
    httpx.Client.send = original_send


# ── GearTestEnv (orchestrator integration) ──────────────────────────────

@pytest.fixture(scope="session")
def gear_test_env(request):
    """mini-chat gear test environment."""
    from lib.orchestrator import GearTestEnv

    mode = request.config.getoption("mode")
    mock = MockProviderServer() if mode == "offline" else DummyMockProvider()

    mini_chat_log = os.environ.get("MINI_CHAT_LOG", "debug")
    rust_log = os.environ.get(
        "RUST_LOG", f"info,mini_chat={mini_chat_log},oagw={mini_chat_log}",
    )

    return GearTestEnv(
        # Binary resolved from E2E_BINARY env var, or found in PATH/target.
        config_path=MODULE_DIR / "config" / "base.yaml",
        config_patch=_patch_mini_chat_config,
        port=8087,
        health_path="/cf/openapi.json",
        health_timeout=90,
        env={"RUST_LOG": rust_log},
        sidecars=[mock],
        log_suffix="mini-chat",
    )


# ── Fixtures ──────────────────────────────────────────────────────────────

@pytest.fixture(scope="session")
def server(test_env):
    """Alias for backward compat — yields base URL after server is running."""
    global _db_summary_text
    _install_outbox_capture()
    yield test_env.base_url
    # DB summary on teardown, once no turn is still running.
    _wait_no_running_turns()
    _db_summary_text = _print_db_summary()


def _wait_no_running_turns(timeout: float = 5.0) -> None:
    """Best effort: wait until no turn is `running` (the summary would show it)."""
    if not os.path.exists(DB_PATH):
        return
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        running = query_db("SELECT COUNT(*) AS n FROM chat_turns WHERE state = 'running'")
        if running[0]["n"] == 0:
            return
        time.sleep(0.1)


@pytest.fixture(scope="session", autouse=True)
def _provision_credstore_secrets(request, server):
    """Provision the LLM provider secrets through the credstore gateway.

    mini-chat routes LLM calls through OAGW, whose auth plugin resolves the
    provider ``secret_ref`` (``openai-key`` / ``azure-openai-key``) via the
    now-stateful credstore gateway. The gateway resolves a secret's metadata
    from its own DB before reading the value, so the secrets must be created
    through its API — pre-seeding the static plugin config alone is no longer
    reachable.

    The secrets are created as user A in the default tenant, which is also the
    tenant of mini-chat's S2S identity, so a ``tenant``-scoped secret resolves
    for the S2S context OAGW uses when proxying. Offline mode (default) uses mock values
    (the mock provider ignores them); online mode uses the real env keys, the
    same ones ``_patch_mini_chat_config`` would otherwise inject.

    Create-or-replace: POST (create-only), and on 409 — a rerun against an
    already-provisioned rig — PUT with the explicit ``If-Match: *`` overwrite
    (PUT no longer creates and requires a precondition). Reruns stay safe.
    """
    if request.config.getoption("mode") == "online":
        secrets = {
            "openai-key": os.environ.get("OPENAI_API_KEY", ""),
            "azure-openai-key": os.environ.get("AZURE_OPENAI_API_KEY", ""),
        }
    else:
        secrets = {
            "openai-key": "mock-key-openai",
            "azure-openai-key": "mock-key-azure",
        }
    headers = {
        "Authorization": "Bearer mini-chat-e2e",
        "content-type": "application/json",
    }
    # Iterate over a plain tuple of reference names (not `secrets.items()`) so
    # nothing derived from the secret values flows into log/error messages,
    # and never echo the response body — it could reflect the secret.
    provisioned = 0
    reachable = True
    for ref in ("openai-key", "azure-openai-key"):
        value = secrets[ref]
        if not value:
            continue  # online mode without that provider's key configured
        try:
            # The mini-chat rig serves all routes under the api-gateway
            # `prefix_path: "/cf"` (same prefix as API_PREFIX above).
            resp = httpx.post(
                f"{server}/cf/credstore/v1/secrets",
                headers=headers,
                json={"reference": ref, "value": value, "sharing": "tenant"},
                timeout=5.0,
            )
            if resp.status_code == 409:
                # Already provisioned (rerun): overwrite in place.
                resp = httpx.put(
                    f"{server}/cf/credstore/v1/secrets/{ref}",
                    headers={**headers, "If-Match": "*"},
                    json={"value": value, "sharing": "tenant"},
                    timeout=5.0,
                )
        except httpx.RequestError:
            # Any transport-level error (connect/timeout/read-timeout) — keep
            # provisioning non-fatal and fall through to the `yield`. `break`,
            # not `return`: a bare return before the `yield` in this generator
            # fixture raises "did not yield a value" and ERRORs the whole
            # session (review finding #5). Broadened from `ConnectError` so a
            # read-timeout against a still-warming server is caught too.
            reachable = False
            break
        if resp.status_code not in (200, 201, 204):
            msg = f"could not provision credstore secret {ref!r}: HTTP {resp.status_code}"
            if request.config.getoption("mode") == "offline":
                # Offline mode is deterministic — a provisioning failure is a real
                # problem, so fail fast instead of letting tests fail opaquely.
                raise RuntimeError(f"[e2e] {msg}")
            print(f"[e2e] WARN: {msg}")
        else:
            provisioned += 1

    # mini-chat registers its OAGW provider upstreams at boot, but the stateful
    # credstore has no secret then, so registration is deferred and retried in
    # the background once the secret exists (which is exactly what we just did).
    # Wait for that reconcile to converge so provider-dependent tests don't race
    # it and see a transient "Upstream not found". Skip the wait when the rig
    # was unreachable — _await_oagw_upstreams would only time out.
    if reachable:
        _await_oagw_upstreams(server, headers, provisioned)
    yield


def _await_oagw_upstreams(server, headers, expected, timeout=60.0):
    """Best-effort wait until at least ``expected`` OAGW upstreams are live.

    The mini-chat gear retries deferred provider registration on a short cadence
    after its secret is provisioned; poll ``GET /oagw/v1/upstreams`` (a JSON
    array) until the provider upstreams appear, then let routes settle. This
    only *waits* — it never fails the session; the tests' own assertions remain
    the source of truth.

    ``expected`` is the number of secrets provisioned, used as a proxy for the
    number of provider upstreams. That holds for this rig (each provider has one
    distinct ``secret_ref`` and no ``tenant_overrides``); if the config gains
    tenant overrides or shares a ``secret_ref`` across providers, revisit this
    count (upstream count would no longer equal secret count).

    Robustness: a persistently non-observable endpoint (e.g. an authz change
    returning non-200) is detected within a short probe window and we proceed
    rather than spinning the full timeout; a non-JSON/invalid body is treated
    as "not ready yet" rather than raising.
    """
    if expected == 0:
        return
    start = time.monotonic()
    deadline = start + timeout
    probe_window = min(10.0, timeout)  # time allowed to observe *any* 200
    saw_endpoint = False
    while time.monotonic() < deadline:
        count = None
        try:
            resp = httpx.get(f"{server}/cf/oagw/v1/upstreams", headers=headers, timeout=5.0)
            if resp.status_code == 200:
                saw_endpoint = True
                body = resp.json()
                if isinstance(body, list):
                    count = len(body)
        except (httpx.RequestError, ValueError):
            pass  # transport error or non-JSON body — treat as "not ready yet"

        if count is not None and count >= expected:
            # Upstreams are up; give per-provider route registration (which runs
            # just after each create_upstream) a brief moment to catch up.
            time.sleep(1.0)
            return
        # If the endpoint never became observable within the probe window, it is
        # likely gated (authz/route); stop waiting and let the tests decide.
        if not saw_endpoint and time.monotonic() - start >= probe_window:
            print("[e2e] WARN: OAGW upstreams endpoint not observable; proceeding")
            return
        time.sleep(0.5)
    print(f"[e2e] WARN: OAGW upstreams did not reach >= {expected} within {timeout:.0f}s")


@pytest.fixture
def same_utc_day(request):
    """Skip the test when it could run past UTC midnight.

    Usage and limits are kept per UTC day (and month): a turn that finishes
    after midnight is charged to the new period, so a before/after
    comparison of daily usage, or usage seeded for today, would not match.
    The margin is the test's timeout plus 10 s (60 s without a timeout)."""
    marker = request.node.get_closest_marker("timeout")
    timeout = float(marker.args[0]) if marker is not None and marker.args else 0.0
    margin = (timeout or 50.0) + 10.0
    now = datetime.now(timezone.utc)
    midnight = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
    if (midnight - now).total_seconds() < margin:
        pytest.skip(f"less than {margin:.0f} s to UTC midnight: the daily quota period rolls over")


@pytest.fixture
def offline_only(request):
    """Skip the test in online mode: it drives or inspects the mock provider
    (in online mode `mock_provider` is a no-op that records nothing)."""
    if request.config.getoption("mode") == "online":
        pytest.skip("requires the mock provider (offline mode)")


@pytest.fixture(scope="session")
def mock_provider(test_env):
    """Access to the mock provider sidecar (for set_next_scenario)."""
    return test_env.sidecars.get("mock-provider")


@pytest.fixture(autouse=True)
def reset_mock_provider_state(mock_provider):
    """Prevent session-scoped mock sidecar state from leaking between tests."""
    mock_provider.clear_captured_requests()
    mock_provider.clear_override_scenarios()
    mock_provider.clear_state()
    yield
    path_errors = mock_provider.get_path_errors()
    mock_provider.clear_captured_requests()
    mock_provider.clear_override_scenarios()
    mock_provider.clear_state()
    # The mock answers 404 to a path that neither configured provider uses
    # (mock_provider/server.py, `path_error`); none may have been requested.
    assert path_errors == []


@pytest.fixture
def chat(server) -> dict:
    """Create a fresh chat with the default model (azure-gpt-4.1)."""
    resp = httpx.post(f"{API_PREFIX}/chats", json={})
    assert resp.status_code == 201, f"Failed to create chat: {resp.status_code} {resp.text}"
    return resp.json()


@pytest.fixture
def chat_with_model(server, request):
    """Factory fixture: create a chat with a specific model."""
    def _create(model: str) -> dict:
        if request.config.getoption("mode") == "online":
            from .config.generator import model_provider, skip_unless_provider_key
            skip_unless_provider_key(model_provider(model))
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
        assert resp.status_code == 201, f"Failed to create chat: {resp.status_code} {resp.text}"
        return resp.json()
    return _create


# ── Provider-parameterized fixtures ───────────────────────────────────────

@pytest.fixture(params=[
    pytest.param("openai", marks=pytest.mark.openai),
    pytest.param("azure", marks=pytest.mark.azure),
])
def provider(request):
    """Current provider under test — parameterized, auto-marked."""
    return request.param


@pytest.fixture
def provider_chat(provider, chat_with_model) -> dict:
    """Chat using the default model for the current provider."""
    return chat_with_model(PROVIDER_DEFAULT_MODEL[provider])
