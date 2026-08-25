"""E2E coverage for AM's service-account (machine-identity) surface.

Seam under test: AM REST -> ``IdpPluginClient`` -> ``static-idp-plugin``
-> back through the canonical envelope. The plugin overrides the four
service-account methods, so this suite exercises a real lifecycle
rather than the ``501 Unimplemented`` the SDK defaults produce for an
adapter that ships only the tenant / user halves.

What only an end-to-end request can show, and what is therefore pinned
here rather than in the Rust suites:

* the ``Location`` and ``Cache-Control: no-store`` headers on the two
  credential-bearing responses, as they leave the gateway;
* that the secret appears in exactly those two responses and in no
  listing;
* that a rotation returns a *different* secret for the same identity;
* cross-tenant addressing, using two genuinely distinct caller tokens —
  the in-process Rust tests share one fake registry and can only
  approximate this.
"""

import uuid

import httpx

from .conftest import (
    REQUEST_TIMEOUT,
    _service_account,
    _service_account_rotate,
    _service_accounts,
    unique_name,
)

# Canonical-envelope ``context.resource_type`` for AM's tenant resource.
# The cross-tenant guard answers with THIS type, not the service-account
# one -- see ``test_accounts_are_tenant_scoped_across_callers``.
TENANT_RESOURCE_TYPE = "gts.cf.core.am.tenant.v1~"


async def _provision(client, base, headers, tenant_id, name, scopes=None):
    """Provision one account and return the parsed credential body."""
    payload = {"name": name}
    if scopes is not None:
        payload["scopes"] = scopes
    resp = await client.post(
        _service_accounts(base, tenant_id), headers=headers, json=payload,
    )
    assert resp.status_code == 201, (
        f"provision service account: {resp.status_code} {resp.text}"
    )
    return resp


async def test_service_account_lifecycle_round_trip(
    am_base_url, am_headers, create_tenant,
):
    """Provision -> list -> rotate -> revoke, end to end.

    Also pins the two header contracts that exist precisely because the
    body carries a credential: ``Location`` addressing the new account,
    and ``Cache-Control: no-store`` so no intermediary retains it.
    """
    tenant = await create_tenant("splife")
    tenant_id = tenant["id"]
    name = unique_name("sp")

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        resp = await _provision(
            c, am_base_url, am_headers, tenant_id, name, ["platform.read"],
        )
        creds = resp.json()
        client_id = creds["client_id"]
        secret = creds["client_secret"]

        assert resp.headers.get("cache-control") == "no-store", (
            "a credential-bearing response must not be cacheable"
        )
        assert resp.headers.get("location", "").endswith(
            f"/service-accounts/{client_id}"
        ), f"Location must address the new account: {resp.headers!r}"
        assert secret, "client_secret must be present exactly once, here"
        assert creds["token_url"], "token_url must be populated"
        uuid.UUID(creds["subject_id"])

        # Listing: carries the verbatim submitted name, and no secret.
        r = await c.get(
            _service_accounts(am_base_url, tenant_id), headers=am_headers,
        )
        assert r.status_code == 200, f"list: {r.status_code} {r.text}"
        items = r.json()["service_accounts"]
        entry = next(p for p in items if p["client_id"] == client_id)
        assert entry["name"] == name, (
            "the submitted name must be echoed verbatim — it is the only "
            "contractual bridge from a name to an opaque client id"
        )
        assert entry["enabled"] is True
        assert entry["scopes"], "provider-reported scopes must be surfaced"
        assert secret not in r.text, "a listing must never carry a secret"

        # Rotate: same identity, different secret, same header contract.
        r = await c.post(
            _service_account_rotate(am_base_url, tenant_id, client_id),
            headers=am_headers,
        )
        assert r.status_code == 200, f"rotate: {r.status_code} {r.text}"
        rotated = r.json()
        assert r.headers.get("cache-control") == "no-store"
        assert rotated["client_id"] == client_id, "client id must be stable"
        assert rotated["subject_id"] == creds["subject_id"], (
            "subject id must survive rotation — RBAC bindings reference it"
        )
        assert rotated["client_secret"] != secret, (
            "a rotation returning the same secret would be a silent no-op"
        )

        # Revoke, then confirm it is gone from the listing.
        r = await c.delete(
            _service_account(am_base_url, tenant_id, client_id),
            headers=am_headers,
        )
        assert r.status_code == 204, f"revoke: {r.status_code} {r.text}"

        r = await c.get(
            _service_accounts(am_base_url, tenant_id), headers=am_headers,
        )
        assert r.status_code == 200
        remaining = [
            p["client_id"] for p in r.json()["service_accounts"]
        ]
        assert client_id not in remaining, "revoked account still listed"


async def test_revoke_is_idempotent(am_base_url, am_headers, create_tenant):
    """A retried DELETE must stay a 204 — that is the whole contract."""
    tenant = await create_tenant("spidem")
    tenant_id = tenant["id"]

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        resp = await _provision(
            c, am_base_url, am_headers, tenant_id, unique_name("sp"),
        )
        client_id = resp.json()["client_id"]
        url = _service_account(am_base_url, tenant_id, client_id)

        for attempt in (1, 2):
            r = await c.delete(url, headers=am_headers)
            assert r.status_code == 204, (
                f"revoke attempt {attempt}: {r.status_code} {r.text}"
            )

        # An account that never existed answers the same way, so revoke
        # cannot be used to probe for existence.
        r = await c.delete(
            _service_account(am_base_url, tenant_id, "svc-never-existed"),
            headers=am_headers,
        )
        assert r.status_code == 204, f"revoke unknown: {r.status_code}"


async def test_duplicate_name_is_rejected_without_revealing_the_existing(
    am_base_url, am_headers, create_tenant,
):
    """``(tenant_id, name)`` is unique over live accounts.

    The refusal must not resume, reveal, or modify the account already
    holding the name — otherwise the caller could reach a credential it
    was not issued.
    """
    tenant = await create_tenant("spdup")
    tenant_id = tenant["id"]
    name = unique_name("sp")

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        first = (
            await _provision(c, am_base_url, am_headers, tenant_id, name)
        ).json()

        r = await c.post(
            _service_accounts(am_base_url, tenant_id),
            headers=am_headers,
            json={"name": name},
        )
        assert r.status_code == 400, (
            f"duplicate name must be rejected: {r.status_code} {r.text}"
        )
        assert first["client_id"] not in r.text, (
            "the refusal must not reveal the existing account"
        )
        assert first["client_secret"] not in r.text, (
            "no credential may appear in an error body"
        )
        # The provider's own detail text must not reach the caller — AM
        # substitutes its own fixed message for the whole category.
        assert "already exists in tenant" not in r.text, (
            "provider failure text leaked into the envelope"
        )

        # After a revoke the name is free again, which is what makes
        # revoke-then-provision a valid recovery path.
        r = await c.delete(
            _service_account(am_base_url, tenant_id, first["client_id"]),
            headers=am_headers,
        )
        assert r.status_code == 204
        await _provision(c, am_base_url, am_headers, tenant_id, name)


async def test_rotate_absent_account_returns_404(
    am_base_url, am_headers, create_tenant,
):
    """Unlike revoke, rotation does not fold absence into success: it
    minted no credential the caller could use."""
    tenant = await create_tenant("sp404")
    tenant_id = tenant["id"]

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.post(
            _service_account_rotate(
                am_base_url, tenant_id, "svc-does-not-exist",
            ),
            headers=am_headers,
        )
        assert r.status_code == 404, (
            f"rotate absent account: {r.status_code} {r.text}"
        )


async def test_item_path_exposes_no_get(
    am_base_url, am_headers, create_tenant,
):
    """The URL ``create`` advertises in ``Location`` answers DELETE and
    prefixes rotate-secret, but registers no GET — the contract has no
    by-id read, and the collection listing covers reads. Pinned so an
    accidental future GET (which would need a by-id read on every
    adapter) is caught here."""
    tenant = await create_tenant("spnoget")
    tenant_id = tenant["id"]

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        resp = await _provision(
            c, am_base_url, am_headers, tenant_id, unique_name("sp"),
        )
        client_id = resp.json()["client_id"]

        r = await c.get(
            _service_account(am_base_url, tenant_id, client_id),
            headers=am_headers,
        )
        assert r.status_code == 405, (
            f"item path must not answer GET: {r.status_code} {r.text}"
        )


async def test_accounts_are_tenant_scoped_across_callers(
    am_base_url, am_headers, am_headers_tenant_b, create_tenant,
):
    """Two distinct caller tokens, two distinct tenant trees.

    Tenant B holding tenant A's exact ``client_id`` must not be able to
    rotate it, and must not see it in its own listing. This is the check
    the in-process Rust tests can only approximate, since they share one
    fake registry behind a single compiled scope.
    """
    tenant_a = await create_tenant("spscope")
    tenant_a_id = tenant_a["id"]

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        owned_by_a = (
            await _provision(
                c, am_base_url, am_headers, tenant_a_id, unique_name("sp"),
            )
        ).json()

        # Tenant B addressing A's account under A's tenant id.
        #
        # 404, exactly -- NOT "403 or 404". DESIGN.md "Authorization"
        # fixes the guard order as gate, then tenant read: under this
        # stack's static-authz-plugin the gate PERMITS (it denies only
        # when no tenant resolves at all) and hands back an
        # `In(OWNER_TENANT_ID, [tenant_b])` clamp, so the refusal comes
        # one step later, from the scoped tenant read in
        # `resolve_active_tenant`. Tenant A is outside B's compiled
        # scope, so it does not resolve and AM answers NotFound -- the
        # tenant is invisible rather than merely forbidden, which is
        # what keeps tenant topology from leaking through a 403.
        #
        # A 403 here would mean the gate started denying outright, which
        # is a different (and topology-leaking) contract; the disjunctive
        # assertion this replaces could not tell the two apart.
        r = await c.post(
            _service_account_rotate(
                am_base_url, tenant_a_id, owned_by_a["client_id"],
            ),
            headers=am_headers_tenant_b,
        )
        assert r.status_code == 404, (
            "tenant B rotating tenant A's account must be 404 (invisible), "
            f"not merely forbidden: {r.status_code} {r.text}"
        )
        # The 404 must be the TENANT guard firing, not an account-level
        # miss: a service-account resource_type here would mean B got far
        # enough to address inside A's tenant.
        ctx = r.json().get("context", {})
        assert ctx.get("resource_type") == TENANT_RESOURCE_TYPE, (
            "cross-tenant refusal must surface as a tenant not-found, "
            f"got resource_type={ctx.get('resource_type')!r}: {r.text}"
        )
        assert owned_by_a["client_secret"] not in r.text

        # And B must not be able to enumerate A's accounts.
        r = await c.get(
            _service_accounts(am_base_url, tenant_a_id),
            headers=am_headers_tenant_b,
        )
        assert r.status_code == 404, (
            f"cross-tenant listing must be 404 (invisible): {r.status_code} {r.text}"
        )
        ctx = r.json().get("context", {})
        assert ctx.get("resource_type") == TENANT_RESOURCE_TYPE, (
            "cross-tenant listing refusal must surface as a tenant not-found, "
            f"got resource_type={ctx.get('resource_type')!r}: {r.text}"
        )
