"""Shared assertions for the Model Registry E2E suite."""
from __future__ import annotations

import re
from typing import Optional
from urllib.parse import quote

PROVIDER_RESOURCE = "gts.cf.genai.model_registry.provider.v1~"
MODEL_RESOURCE = "gts.cf.genai.model_registry.model.v1~"

_TITLES = {
    "invalid_argument": "Invalid Argument",
    "not_found": "Not Found",
    "already_exists": "Already Exists",
    "permission_denied": "Permission Denied",
    "aborted": "Aborted",
    "service_unavailable": "Service Unavailable",
}

UUID_V7_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
)


def problem_type(category: str) -> str:
    return f"gts://gts.cf.core.errors.err.v1~cf.core.err.{category}.v1~"


def assert_problem(
    resp,
    status: int,
    category: str,
    *,
    reason: Optional[str] = None,
    field: Optional[str] = None,
    field_reason: Optional[str] = None,
    resource_type: Optional[str] = None,
) -> dict:
    """Check a canonical RFC-9457 response and return its body.

    ``reason`` is the category reason in ``context``; ``field`` and
    ``field_reason`` select one entry of ``context.field_violations``.
    """
    assert resp.status_code == status, resp.text
    content_type = resp.headers.get("content-type", "")
    assert content_type.startswith("application/problem+json"), content_type
    body = resp.json()
    assert body["type"] == problem_type(category), body
    assert body["title"] == _TITLES[category], body
    assert body["status"] == status, body
    context = body.get("context") or {}
    if reason is not None:
        assert context.get("reason") == reason, body
    if resource_type is not None:
        assert context.get("resource_type") == resource_type, body
    if field is not None:
        violations = context.get("field_violations") or []
        matching = [v for v in violations if v.get("field") == field]
        assert matching, f"no violation on {field!r}: {body}"
        if field_reason is not None:
            assert any(v.get("reason") == field_reason for v in matching), body
    return body


def etag_of(resp) -> str:
    etag = resp.headers.get("etag")
    assert etag is not None, f"no ETag on {resp.request.method} {resp.request.url}"
    return etag


def canonical_segment(canonical_id: str) -> str:
    """Percent-encode a canonical id into one path segment."""
    return quote(canonical_id, safe="")


async def collect(client, url: str, headers: dict, params: Optional[dict] = None) -> list:
    """Walk every page of a list and return all items."""
    params = dict(params or {})
    params.setdefault("limit", 100)
    items: list = []
    for _ in range(50):
        resp = await client.get(url, headers=headers, params=params)
        assert resp.status_code == 200, resp.text
        page = resp.json()
        items.extend(page["items"])
        cursor = page["page_info"].get("next_cursor")
        if not cursor:
            return items
        # The cursor fixes the order and is bound to the filter it was issued for.
        params = {k: v for k, v in params.items() if k != "$orderby"}
        params["cursor"] = cursor
    raise AssertionError("pagination did not terminate")
