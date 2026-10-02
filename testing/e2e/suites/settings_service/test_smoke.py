"""settings-service E2E smoke: the gear is up, its read surfaces answer, and
its refusals are the canonical problem documents.

What each test pins, and why it is enough for a smoke:

- The gear started inside the shared server — its migrations ran, its init
  reached types-registry and credstore — because its routes answer at all.
- The three read surfaces (browse, declarations, search) answer an
  authenticated administrator with a page rather than an error.
- The error layer is wired: a malformed key and a too-short query are refused
  with `400` problem documents before the store is touched, an undeclared
  setting is `404`, and an anonymous caller is turned away.

The server keeps its SQLite files under ``server.home_dir`` between runs, so
on a developer's machine the fleet may hold declarations from earlier work
while CI starts empty. The tests assert the shape of an answer, never that the
store is empty; the one test that needs a declaration skips when there is
none, visibly.
"""
import httpx
import pytest

from .conftest import UNDECLARED_KEY, encoded

PROBLEM_TYPE_PREFIX = "gts://gts.cf.core.errors.err.v1~cf.core.err."


def _is_problem(body: dict, status: int) -> bool:
    """A canonical problem document: `type` as a `gts://` URI, `title`, `status`."""
    return (
        isinstance(body, dict)
        and str(body.get("type", "")).startswith(PROBLEM_TYPE_PREFIX)
        and isinstance(body.get("title"), str)
        and body.get("status") == status
    )


class TestGearIsUp:
    """The routes answer, and answer with a page."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_declarations_browse_and_search_answer_a_page(
        self, settings_url, tenant_a_headers
    ):
        async with httpx.AsyncClient(timeout=10.0) as client:
            for path in ("/declarations", "/settings", "/search?q=proxy"):
                resp = await client.get(f"{settings_url}{path}", headers=tenant_a_headers)
                assert resp.status_code == 200, f"{path}: {resp.status_code} {resp.text}"
                body = resp.json()
                assert isinstance(body.get("items"), list), f"{path}: a page of items: {body}"
                assert "limit" in body.get("page_info", {}), (
                    f"{path}: the shared page envelope: {body}"
                )

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_a_declared_setting_reads_with_its_value_source_and_tag(
        self, settings_url, tenant_a_headers
    ):
        # Whatever the fleet has declared — a contributed module's settings on
        # a developer's machine, nothing on a fresh CI runner — the first one
        # resolves for the root tenant with the fields the contract promises.
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(f"{settings_url}/declarations", headers=tenant_a_headers)
            assert resp.status_code == 200, resp.text
            declared = [d for d in resp.json()["items"] if d.get("status") == "active"]
            if not declared:
                pytest.skip("the e2e fleet holds no declarations yet")
            key = declared[0]["key"]
            resp = await client.get(
                f"{settings_url}/settings/{encoded(key)}", headers=tenant_a_headers
            )
            assert resp.status_code == 200, resp.text
            body = resp.json()
            assert body.get("key") == key, body
            assert "value" in body and "source" in body, body
            assert isinstance(body.get("etag"), str) and body["etag"], body
            assert resp.headers.get("etag"), "the state tag travels as a header too"

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_an_anonymous_caller_is_turned_away(self, settings_url):
        # Every route is declared `.authenticated()`; the platform answers
        # before the gear sees the request.
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(f"{settings_url}/declarations")
            assert resp.status_code == 401, resp.text


class TestRefusalsAreProblemDocuments:
    """The canonical error layer, end to end."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_a_malformed_key_is_refused_before_the_store(
        self, settings_url, tenant_a_headers
    ):
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{settings_url}/settings/{encoded('not a key')}",
                headers=tenant_a_headers,
            )
            assert resp.status_code == 400, resp.text
            body = resp.json()
            assert _is_problem(body, 400), body
            violations = body.get("context", {}).get("field_violations", [])
            assert violations, f"a validation problem names the field: {body}"

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_a_search_query_shorter_than_two_characters_is_refused(
        self, settings_url, tenant_a_headers
    ):
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(f"{settings_url}/search?q=x", headers=tenant_a_headers)
            assert resp.status_code == 400, resp.text
            assert _is_problem(resp.json(), 400), resp.text

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_an_undeclared_setting_is_absent(self, settings_url, tenant_a_headers):
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{settings_url}/settings/{encoded(UNDECLARED_KEY)}",
                headers=tenant_a_headers,
            )
            assert resp.status_code == 404, resp.text
            body = resp.json()
            assert _is_problem(body, 404), body
            # The gear names the kind, never the caller's identifier: the key
            # appears only in `instance`, the request path the platform adds.
            assert UNDECLARED_KEY not in str(body.get("detail", "")), body
            assert UNDECLARED_KEY not in str(body.get("title", "")), body
            assert body.get("context", {}).get("resource_name") == "declaration", body

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_a_write_to_an_undeclared_setting_is_absent_too(
        self, settings_url, tenant_a_headers
    ):
        # The write gates run in order — declaration first — so with nothing
        # declared a write is `404`, not a step-up challenge; the step-up gate
        # itself is exercised by the crate's surface tests, which can declare.
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{settings_url}/settings/{encoded(UNDECLARED_KEY)}/value",
                headers={**tenant_a_headers, "If-Match": "absent"},
                json={"value": True},
            )
            assert resp.status_code == 404, resp.text
            assert _is_problem(resp.json(), 404), resp.text
