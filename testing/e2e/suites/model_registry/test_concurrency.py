"""Model Registry E2E: ETag and If-Match (MR-C*, ADR-0004)."""
import pytest

from .helpers import assert_problem, etag_of


@pytest.fixture
async def resource(request, providers_url, models_url, root_headers, create_provider, create_model):
    """(url, update body) of a fresh provider or model at version 1."""
    provider = await create_provider(root_headers)
    if request.param == "provider":
        return f"{providers_url}/{provider['id']}", {"name": "Changed"}
    model = await create_model(root_headers, provider["id"])
    return f"{models_url}/{model['id']}", {"display_name": "Changed"}


both = pytest.mark.parametrize("resource", ["provider", "model"], indirect=True)


class TestIfMatchOnUpdate:
    @both
    async def test_missing_if_match_is_400(self, client, root_headers, resource):
        """MR-C01: PATCH without If-Match is IF_MATCH_REQUIRED and changes nothing."""
        url, body = resource
        resp = await client.patch(url, headers=root_headers, json=body)
        assert_problem(
            resp, 400, "invalid_argument", field="If-Match", field_reason="IF_MATCH_REQUIRED"
        )

        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"1"'

    @both
    @pytest.mark.parametrize("validator", ['"99"', 'W/"1"', '"01"'])
    async def test_stale_validator_is_409(self, client, root_headers, resource, validator):
        """MR-C02, MR-C03: any validator that is not the current ETag is 409, never 412."""
        url, body = resource
        resp = await client.patch(url, headers={**root_headers, "If-Match": validator}, json=body)
        assert_problem(resp, 409, "aborted", reason="OPTIMISTIC_LOCK_FAILURE")

        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"1"'

    @both
    async def test_two_validators_are_invalid_format(self, client, root_headers, resource):
        """MR-C03: more than one validator is INVALID_FORMAT on If-Match."""
        url, body = resource
        resp = await client.patch(
            url, headers={**root_headers, "If-Match": '"1", "2"'}, json=body
        )
        assert_problem(resp, 400, "invalid_argument", field="If-Match", field_reason="INVALID_FORMAT")

    @both
    async def test_current_etag_then_wildcard(self, client, root_headers, resource):
        """MR-C04: the current ETag and `*` both update and increment the ETag."""
        url, body = resource
        resp = await client.get(url, headers=root_headers)
        etag = etag_of(resp)

        resp = await client.patch(url, headers={**root_headers, "If-Match": etag}, json=body)
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"2"'

        # The old validator no longer matches.
        resp = await client.patch(url, headers={**root_headers, "If-Match": etag}, json=body)
        assert_problem(resp, 409, "aborted", reason="OPTIMISTIC_LOCK_FAILURE")

        resp = await client.patch(url, headers={**root_headers, "If-Match": "*"}, json=body)
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"3"'


class TestIfMatchOnDelete:
    @both
    async def test_stale_delete_keeps_the_row(self, client, root_headers, resource):
        """MR-C05: DELETE with a stale If-Match is 409 and the row remains."""
        url, _ = resource
        resp = await client.delete(url, headers={**root_headers, "If-Match": '"99"'})
        assert_problem(resp, 409, "aborted", reason="OPTIMISTIC_LOCK_FAILURE")

        resp = await client.get(url, headers=root_headers)
        assert resp.status_code == 200, resp.text

    @both
    async def test_delete_with_current_etag(self, client, root_headers, resource):
        """MR-C05: DELETE with the current ETag succeeds."""
        url, _ = resource
        resp = await client.delete(url, headers={**root_headers, "If-Match": '"1"'})
        assert resp.status_code == 204, resp.text

    @both
    async def test_delete_without_if_match(self, client, root_headers, resource):
        """MR-C05: If-Match is optional on DELETE."""
        url, _ = resource
        resp = await client.delete(url, headers=root_headers)
        assert resp.status_code == 204, resp.text
