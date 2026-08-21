"""E2E tests locking in `toolkit::api::rest::extract`'s error representation
across every users-info handler that was migrated off axum's bare
`Json`/`Path` (gears-rust#4596, ADR 0006).

Each handler that takes `extract::Json<T>` or `extract::Path<T>` must render
a malformed request as a full RFC 9457 `Problem` body - not axum's default
plain-text rejection. Every assertion below compares the complete response
body as a literal dict, not a spot-checked subset, mirroring this PR's own
Rust test convention (`assert_eq!(json, json!({...}))`). `trace_id` is the
one field neither side can predict (a fresh, opaque per-request value) - it
is copied from the real response into the expected dict before comparing,
so the rest of the literal still has to match exactly.

These exercise the real, running server (not a unit-level
`tower::oneshot`), so they also confirm the migration didn't regress at the
actual HTTP/routing layer.
"""
import uuid

import httpx
import pytest

from .conftest import REQUEST_TIMEOUT, TENANT_A_ID


def _users(base: str) -> str:
    return f"{base}/users-info/v1/users"


def _cities(base: str) -> str:
    return f"{base}/users-info/v1/cities"


def _user_address(base: str, user_id: str) -> str:
    return f"{base}/users-info/v1/users/{user_id}/address"


@pytest.fixture
async def existing_user(base_url, auth_headers):
    """Create a real user via the API and return its id, for tests that need
    a valid path segment to combine with a malformed body."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.post(
            _users(base_url),
            headers={**auth_headers, "Content-Type": "application/json"},
            json={
                "tenant_id": TENANT_A_ID,
                "email": f"e2e-extract-{uuid.uuid4().hex[:8]}@example.com",
                "display_name": "E2E Extract Fixture",
            },
        )
        assert resp.status_code == 201, resp.text
        return resp.json()["id"]


# ── Smoke: the migration didn't break the happy path ─────────────────────


@pytest.mark.smoke
async def test_create_and_get_user_happy_path(base_url, auth_headers):
    """`extract::Json`/`extract::Path` must behave identically to axum's
    built-ins on well-formed input - only failure handling changed."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        create = await client.post(
            _users(base_url),
            headers={**auth_headers, "Content-Type": "application/json"},
            json={
                "tenant_id": TENANT_A_ID,
                "email": f"e2e-happy-{uuid.uuid4().hex[:8]}@example.com",
                "display_name": "E2E Happy Path",
            },
        )
        assert create.status_code == 201, create.text
        user_id = create.json()["id"]
        uuid.UUID(user_id)  # a real UUID was extracted/generated, not garbage

        get = await client.get(_users(base_url) + f"/{user_id}", headers=auth_headers)
        assert get.status_code == 200, get.text
        assert get.json()["user"]["email"] == create.json()["email"]


# ── extract::Json - malformed request bodies ──────────────────────────────


@pytest.mark.smoke
async def test_malformed_json_body_returns_problem_not_plain_text(base_url, auth_headers):
    """Syntactically invalid JSON on `create_user` (`extract::Json<CreateUserReq>`)
    must render as a full `Problem`, not axum's default plain-text 400."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.post(
            _users(base_url),
            headers={**auth_headers, "Content-Type": "application/json"},
            content=b"{not-json}",
        )
        assert resp.status_code == 400
        assert resp.headers["content-type"].startswith("application/problem+json")
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Failed to parse the request body as JSON: key must be a string at line 1 column 2",
                    "reason": "json_syntax_error",
                }],
            },
        }


async def test_malformed_json_body_on_update_user(base_url, auth_headers, existing_user):
    """Same malformed-body guarantee on `update_user`
    (`extract::Path<Uuid>` + `extract::Json<UpdateUserReq>`), combined with a
    *valid* path segment - confirms the two extractors compose correctly."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.patch(
            _users(base_url) + f"/{existing_user}",
            headers={**auth_headers, "Content-Type": "application/json"},
            content=b"{not-json}",
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": f"/users-info/v1/users/{existing_user}",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Failed to parse the request body as JSON: key must be a string at line 1 column 2",
                    "reason": "json_syntax_error",
                }],
            },
        }


async def test_malformed_json_body_on_create_city(base_url, auth_headers):
    """Same guarantee on a second, independently-migrated gear module
    (`create_city`) - confirms this isn't a one-handler fluke."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.post(
            _cities(base_url),
            headers={**auth_headers, "Content-Type": "application/json"},
            content=b"{not-json}",
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/cities",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Failed to parse the request body as JSON: key must be a string at line 1 column 2",
                    "reason": "json_syntax_error",
                }],
            },
        }


async def test_malformed_json_body_on_put_user_address(base_url, auth_headers, existing_user):
    """Same guarantee on `put_user_address`, which combines
    `extract::Path<Uuid>` and `extract::Json<PutAddressReq>` on a PUT."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.put(
            _user_address(base_url, existing_user),
            headers={**auth_headers, "Content-Type": "application/json"},
            content=b"{not-json}",
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": f"/users-info/v1/users/{existing_user}/address",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Failed to parse the request body as JSON: key must be a string at line 1 column 2",
                    "reason": "json_syntax_error",
                }],
            },
        }


async def test_wrong_field_type_returns_422(base_url, auth_headers):
    """Well-formed JSON that fails schema validation (a non-UUID string in a
    `Uuid` field) is a distinct rejection kind from a syntax error - must
    resolve to 422 `invalid_json_body`, not 400 `json_syntax_error`."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.post(
            _users(base_url),
            headers={**auth_headers, "Content-Type": "application/json"},
            content=b'{"tenant_id":"not-a-uuid","email":"a@b.com","display_name":"A"}',
        )
        assert resp.status_code == 422
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 422,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Failed to deserialize the JSON body into the target type: tenant_id: UUID parsing failed: invalid character: found `n` at 0 at line 1 column 25",
                    "reason": "invalid_json_body",
                }],
            },
        }


async def test_missing_required_field_returns_422(base_url, auth_headers):
    """A well-formed JSON object missing a required field is the same
    rejection kind as a wrong-type field - 422 `invalid_json_body`."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.post(
            _users(base_url),
            headers={**auth_headers, "Content-Type": "application/json"},
            content=(
                b'{"tenant_id":"' + TENANT_A_ID.encode() + b'","email":"a@b.com"}'
            ),  # display_name missing
        )
        assert resp.status_code == 422
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 422,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Failed to deserialize the JSON body into the target type: missing field `display_name` at line 1 column 70",
                    "reason": "invalid_json_body",
                }],
            },
        }


async def test_missing_content_type_returns_415(base_url, auth_headers):
    """A body that isn't declared as JSON is a third distinct rejection kind
    - 415 `missing_json_content_type`, not 400 or 422."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.post(
            _users(base_url),
            headers={**auth_headers, "Content-Type": "text/plain"},
            content=(
                b'{"tenant_id":"' + TENANT_A_ID.encode()
                + b'","email":"a@b.com","display_name":"A"}'
            ),
        )
        assert resp.status_code == 415
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 415,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "body",
                    "description": "Expected request with `Content-Type: application/json`",
                    "reason": "missing_json_content_type",
                }],
            },
        }


# ── extract::Path - invalid path parameters ───────────────────────────────


@pytest.mark.smoke
async def test_invalid_path_uuid_returns_problem_not_plain_text(base_url, auth_headers):
    """A non-UUID path segment on `get_user` (`extract::Path<Uuid>`) must
    render as a full `Problem`, not axum's default plain-text 400."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.get(_users(base_url) + "/not-a-uuid", headers=auth_headers)
        assert resp.status_code == 400
        assert resp.headers["content-type"].startswith("application/problem+json")
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users/not-a-uuid",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "path",
                    "description": "Invalid URL: Cannot parse `id` with value `not-a-uuid`: UUID parsing failed: invalid character: found `n` at 0",
                    "reason": "invalid_path_params",
                }],
            },
        }


async def test_invalid_path_uuid_on_delete_user(base_url, auth_headers):
    """Same guarantee on `delete_user`."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.delete(_users(base_url) + "/not-a-uuid", headers=auth_headers)
        assert resp.status_code == 400
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users/not-a-uuid",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "path",
                    "description": "Invalid URL: Cannot parse `id` with value `not-a-uuid`: UUID parsing failed: invalid character: found `n` at 0",
                    "reason": "invalid_path_params",
                }],
            },
        }


async def test_invalid_path_uuid_on_get_city(base_url, auth_headers):
    """Same guarantee on a second, independently-migrated gear module
    (`get_city`)."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.get(_cities(base_url) + "/not-a-uuid", headers=auth_headers)
        assert resp.status_code == 400
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/cities/not-a-uuid",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "path",
                    "description": "Invalid URL: Cannot parse `id` with value `not-a-uuid`: UUID parsing failed: invalid character: found `n` at 0",
                    "reason": "invalid_path_params",
                }],
            },
        }


async def test_invalid_path_uuid_on_user_address(base_url, auth_headers):
    """Same guarantee on the address routes, whose path segment is a
    `{id}` shared with the parent `users` resource."""
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        resp = await client.get(_user_address(base_url, "not-a-uuid"), headers=auth_headers)
        assert resp.status_code == 400
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/users-info/v1/users/not-a-uuid/address",
            "trace_id": body["trace_id"],
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [{
                    "field": "path",
                    "description": "Invalid URL: Cannot parse `id` with value `not-a-uuid`: UUID parsing failed: invalid character: found `n` at 0",
                    "reason": "invalid_path_params",
                }],
            },
        }
