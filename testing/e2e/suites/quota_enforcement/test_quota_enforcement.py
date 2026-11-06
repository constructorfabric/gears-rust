"""Quota Enforcement E2E: the seams only a live server shows.

One seam per test: a debit decided over the real SQL store, bootstrap
reaching Ready with the published plugin and cluster, tenant isolation
through authn -> static-authz -> SecureConn, a bulk item error as the gateway
serves it, and cursor pagination over the real store.
"""
import httpx
import pytest

from .conftest import GEAR, debit_body, qe_component, quota_body, subject_filter


def field_names(problem) -> list[str]:
    """Every ``field`` named anywhere in a problem body."""
    if isinstance(problem, dict):
        found = [problem["field"]] if isinstance(problem.get("field"), str) else []
        for value in problem.values():
            found += field_names(value)
        return found
    if isinstance(problem, list):
        return [name for item in problem for name in field_names(item)]
    return []


class TestDebit:
    """A debit decides against a Quota held in the real store."""

    @pytest.mark.smoke
    async def test_debit_allows_then_denies_over_cap(self, qe_url, tenant_a_headers, subject):
        """60 of 100 is allowed and planned on the Quota; 50 more is denied by it."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{qe_url}/quotas", headers=tenant_a_headers, json=quota_body(subject)
            )
            assert resp.status_code == 201, resp.text
            assert resp.headers.get("location"), resp.headers
            quota_id = resp.json()["id"]

            resp = await client.post(
                f"{qe_url}/operations/debit",
                headers=tenant_a_headers,
                json=debit_body(subject, 60),
            )
            assert resp.status_code == 200, resp.text
            decision = resp.json()
            assert decision["result"] == {"outcome": "allowed"}, decision
            assert decision["debit_plan"] == [{"quota_id": quota_id, "amount": 60}], decision

            resp = await client.post(
                f"{qe_url}/operations/debit",
                headers=tenant_a_headers,
                json=debit_body(subject, 50),
            )
            assert resp.status_code == 200, resp.text
            result = resp.json()["result"]
            assert result["outcome"] == "denied", result
            assert result["violated_quota_ids"] == [quota_id], result


class TestReadiness:
    """Bootstrap completes against the live dependencies."""

    def test_health_reports_quota_enforcement_ready(self, base_url):
        """Storage, cluster, PDP probe and catalog all resolved: healthy, no code."""
        component = qe_component(base_url)
        assert component is not None, f"no {GEAR} component in /health"
        assert component["status"] == "healthy", component
        assert "code" not in component, component


class TestTenantIsolation:
    """The PDP scope reaches storage."""

    async def test_other_tenant_cannot_read_a_quota(
        self, qe_url, tenant_a_headers, tenant_b_headers, subject
    ):
        """Tenant B gets 404 for tenant A's Quota, never its content."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{qe_url}/quotas", headers=tenant_a_headers, json=quota_body(subject)
            )
            assert resp.status_code == 201, resp.text
            quota_id = resp.json()["id"]

            resp = await client.get(f"{qe_url}/quotas/{quota_id}", headers=tenant_b_headers)
            assert resp.status_code == 404, resp.text
            assert resp.headers["content-type"].startswith("application/problem+json"), resp.headers
            assert subject not in resp.text, resp.text


class TestBulk:
    """A bulk envelope is all-or-nothing and names its failing item."""

    async def test_bulk_create_item_error_names_the_item_and_applies_nothing(
        self, qe_url, tenant_a_headers, subject
    ):
        """A bad second item is a problem naming ``items[1].cap``; the first is not created."""
        envelope = {
            "tenant_id": quota_body(subject)["tenant_id"],
            "idempotency_key": f"{subject}-pack",
            "items": [
                {"idempotency_key": "seat-0", "quota": quota_body(subject)},
                {"idempotency_key": "seat-1", "quota": quota_body(subject, cap=-1)},
            ],
        }
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{qe_url}/quotas/bulk-create", headers=tenant_a_headers, json=envelope
            )
            assert resp.status_code == 400, resp.text
            assert resp.headers["content-type"].startswith("application/problem+json"), resp.headers
            assert "items[1].cap" in field_names(resp.json()), resp.text

            resp = await client.get(
                f"{qe_url}/quotas", headers=tenant_a_headers, params=subject_filter(subject)
            )
            assert resp.status_code == 200, resp.text
            assert resp.json()["items"] == [], resp.text


class TestPagination:
    """List cursors round-trip through the real store."""

    async def test_list_cursor_pages_through_every_quota(self, qe_url, tenant_a_headers, subject):
        """Three Quotas, limit 1: three pages, each Quota exactly once, then no cursor."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            created = set()
            for _ in range(3):
                resp = await client.post(
                    f"{qe_url}/quotas", headers=tenant_a_headers, json=quota_body(subject)
                )
                assert resp.status_code == 201, resp.text
                created.add(resp.json()["id"])

            seen: list[str] = []
            pages = 0
            params = {**subject_filter(subject), "limit": 1}
            while True:
                resp = await client.get(f"{qe_url}/quotas", headers=tenant_a_headers, params=params)
                assert resp.status_code == 200, resp.text
                page = resp.json()
                pages += 1
                seen += [item["id"] for item in page["items"]]
                if page["next_cursor"] is None:
                    break
                assert pages < 4, f"cursor did not terminate: {page}"
                params = {**subject_filter(subject), "limit": 1, "cursor": page["next_cursor"]}

            assert pages == 3, seen
            assert len(seen) == len(set(seen)), f"duplicate ids: {seen}"
            assert set(seen) == created, seen
