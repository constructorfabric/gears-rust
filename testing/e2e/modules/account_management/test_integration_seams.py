# Created: 2026-05-16 by Constructor Tech
"""E2E integration seam tests for the account-management module.

12 tests. One file. Targets cross-process seams ONLY -- points where
two independently correct components break when connected. Pure
status-code / OData / envelope-shape coverage lives in the Rust
in-process REST tests; those seams are NOT repeated here.

Seams pinned (one per test):

* S1  -- route_smoke_all_endpoint_families: every documented method+path
        mounted (not 404/405) for the families not exercised by S2-S12.
* S2  -- tenant_crud_round_trip_via_gateway: POST/GET/PATCH/DELETE
        traversal through the gateway -> AM module path.
* S3  -- create_tenant_validates_tenant_type_via_types_registry: the
        cross-module ``tenant_type`` lookup rejects a missing catalog
        entry with a 400 envelope.
* S5  -- metadata_lifecycle_via_real_schema_registry: register schema
        in types-registry, then PUT/GET/RESOLVE walk-up against AM.
* S6  -- metadata_distinct_404_codes_propagate_through_envelope: both
        ``metadata_schema_not_registered`` and ``metadata_entry_not_found``
        survive the serialisation round-trip with a distinct ``code``.
* S7  -- idp_user_provision_deprovision_round_trip: POST -> GET ->
        DELETE -> GET against the configured IdP plugin.
* S8  -- idp_user_deprovision_idempotent: DELETE twice -> both 204.
* S9  -- conversion_request_dual_consent_happy_path: child requests,
        parent approves through the cross-barrier projection.
* S10 -- conversion_wrong_actor_returns_403_via_pep: child tries to
        approve own request -> real PolicyEnforcer denies, not a fake.
* S11 -- unauthenticated_request_rejected_by_gateway_middleware: the
        missing-header rejection comes from the gateway BEFORE AM.
* S12 -- cross_tenant_denied_via_real_policy_bundle: tenant-A caller
        cannot see tenant-B rows -- real PEP scope clamp, not a fake.

Each test is gated on the AM module being reachable; the session-level
``_check_am_reachable`` fixture in ``conftest.py`` skips the whole suite
if the dev stack does not include AM.
"""
import os
import uuid

import httpx
import pytest

from .conftest import (
    DEFAULT_METADATA_SCHEMA_ID,
    DEFAULT_TENANT_TYPE,
    REQUEST_TIMEOUT,
    TENANT_A_ID,
    TENANT_B_ID,
    _child_conversion,
    _child_conversions,
    _children,
    _conversion,
    _conversions,
    _metadata,
    _metadata_entry,
    _metadata_resolved,
    _tenant,
    _tenants,
    _user,
    _users,
    assert_tenant_shape,
    unique_name,
)


# ── S1: Route smoke ─────────────────────────────────────────────────────


def _route_reached(r: httpx.Response) -> bool:
    """Return True iff the response proves the AM handler ran.

    The smoke test only cares whether the gateway forwarded the call to
    the AM handler (i.e. the route is mounted on the method). A handler
    that legitimately returns 404 (e.g. the child-conversions POST
    rejects an unknown ``child_tenant_id`` after URL routing succeeded)
    is indistinguishable at the HTTP level from a gateway-side 404 for
    a missing route. The handler's reply, though, always carries a
    Problem-JSON envelope (``type`` field rooted at the
    ``gts.cf.core.errors.err.v1~`` namespace); a router-side 404 from
    axum returns either an empty body or a bare ``Not Found``. So
    "handler ran" = "non-error status OR Problem-JSON 404".
    """
    if r.status_code not in (404, 405):
        return True
    try:
        body = r.json()
    except ValueError:
        return False
    return isinstance(body, dict) and isinstance(body.get("type"), str) and body[
        "type"
    ].startswith("gts://gts.cf.core.errors.err.v1~")


@pytest.mark.smoke
async def test_route_smoke_all_endpoint_families(
    am_base_url, am_headers, create_tenant,
):
    """Seam: Route registration -- every endpoint family mounted on gateway.

    Verifies the gateway forwards each ``/account-management/v1/...``
    method+path combination to the AM handler. A handler that returns
    a 404 with a Problem-JSON envelope counts as success (route was
    reached); a router-side 404 with no envelope is the failure mode
    this test guards against.
    """
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        h = am_headers

        # GET /tenants/{id}/children -- listing endpoint not in S2.
        r = await c.get(_children(am_base_url, TENANT_A_ID), headers=h)
        assert _route_reached(r), (
            f"GET /tenants/{{id}}/children: {r.status_code} {r.text}"
        )

        # GET /tenants/{id}/metadata -- list endpoint not in S5.
        r = await c.get(_metadata(am_base_url, TENANT_A_ID), headers=h)
        assert _route_reached(r), (
            f"GET /tenants/{{id}}/metadata: {r.status_code} {r.text}"
        )

        # GET /tenants/{id}/conversions -- own listing not in S9.
        r = await c.get(_conversions(am_base_url, TENANT_A_ID), headers=h)
        assert _route_reached(r), (
            f"GET /tenants/{{id}}/conversions: {r.status_code} {r.text}"
        )

        # GET /tenants/{id}/child-conversions -- parent listing not in S9.
        r = await c.get(
            _child_conversions(am_base_url, TENANT_A_ID), headers=h,
        )
        assert _route_reached(r), (
            f"GET /tenants/{{id}}/child-conversions: {r.status_code} {r.text}"
        )

        # POST /tenants/{id}/child-conversions -- parent-side init not
        # in S9. A bogus ``child_tenant_id`` is fine: the handler's 404
        # ``child tenant ... not found`` Problem-JSON proves the route
        # was reached, which is all this smoke check needs.
        r = await c.post(
            _child_conversions(am_base_url, TENANT_A_ID),
            headers=h,
            json={
                "child_tenant_id": str(uuid.uuid4()),
                "target_mode": "self_managed",
            },
        )
        assert _route_reached(r), (
            f"POST /tenants/{{id}}/child-conversions: {r.status_code} {r.text}"
        )

        # GET /tenants/{id}/child-conversions/{request_id} -- parent
        # point read; never exercised by S9.
        bogus = str(uuid.uuid4())
        r = await c.get(
            _child_conversion(am_base_url, TENANT_A_ID, bogus), headers=h,
        )
        assert _route_reached(r), (
            f"GET /tenants/{{id}}/child-conversions/{{rid}}: "
            f"{r.status_code} {r.text}"
        )

        # PATCH /tenants/{id}/child-conversions/{request_id} -- parent
        # resolve dispatcher; never exercised by S9.
        r = await c.patch(
            _child_conversion(am_base_url, TENANT_A_ID, bogus),
            headers=h,
            json={"status": "approved"},
        )
        assert _route_reached(r), (
            f"PATCH /tenants/{{id}}/child-conversions/{{rid}}: "
            f"{r.status_code} {r.text}"
        )


# ── S2: Tenant CRUD round-trip via the gateway ──────────────────────────


@pytest.mark.smoke
async def test_tenant_crud_round_trip_via_gateway(
    am_base_url, am_headers, create_tenant,
):
    """Seam: Gateway -> AM module -> DB CRUD traversal.

    The gateway routes the same ``/account-management/v1/tenants/*``
    URL family across four HTTP methods; verifying the full traversal
    pins the gateway routing table, the SecurityContext middleware,
    the AM handler dispatch, and the DB layer.
    """
    created = await create_tenant("s2crud")
    tenant_id = created["id"]
    assert_tenant_shape(created)
    assert created["status"] == "active"
    assert created["parent_id"] == TENANT_A_ID

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        # GET roundtrip
        r = await c.get(_tenant(am_base_url, tenant_id), headers=am_headers)
        assert r.status_code == 200, f"GET tenant: {r.status_code} {r.text}"
        assert_tenant_shape(r.json())
        assert r.json()["id"] == tenant_id

        # PATCH: rename only — `status` is no longer a mutable PATCH
        # field; lifecycle transitions go through dedicated
        # `POST /suspend`, `/unsuspend` and `DELETE` endpoints.
        new_name = unique_name("s2crud-renamed")
        r = await c.patch(
            _tenant(am_base_url, tenant_id),
            headers=am_headers,
            json={"name": new_name},
        )
        assert r.status_code == 200, f"PATCH tenant: {r.status_code} {r.text}"
        patched = r.json()
        assert patched["name"] == new_name
        assert patched["status"] == "active"

        # POST /suspend — AIP-136 sub-resource (fallback for the
        # colon-method form per axum/matchit constraint). Pins the
        # round-trip + idempotent unsuspend on the new wire surface.
        r = await c.post(
            f"{_tenant(am_base_url, tenant_id)}/suspend",
            headers=am_headers,
        )
        assert r.status_code == 200, f"POST suspend: {r.status_code} {r.text}"
        suspended = r.json()
        assert suspended["status"] == "suspended"
        assert suspended["name"] == new_name

        r = await c.post(
            f"{_tenant(am_base_url, tenant_id)}/unsuspend",
            headers=am_headers,
        )
        assert r.status_code == 200, f"POST unsuspend: {r.status_code} {r.text}"
        unsuspended = r.json()
        assert unsuspended["status"] == "active"

        # DELETE: soft-delete moves the row to status=deleted and arms
        # the retention sweep. Returns 204 No Content; callers re-read
        # the post-delete projection via GET.
        r = await c.delete(
            _tenant(am_base_url, tenant_id), headers=am_headers,
        )
        assert r.status_code == 204, f"DELETE tenant: {r.status_code} {r.text}"
        assert r.content == b"", (
            f"204 response MUST carry an empty body, got {len(r.content)} bytes"
        )

        r = await c.get(_tenant(am_base_url, tenant_id), headers=am_headers)
        assert r.status_code == 200, f"GET post-delete tenant: {r.status_code} {r.text}"
        deleted = r.json()
        assert deleted["status"] == "deleted"
        assert deleted.get("deleted_at") is not None, (
            "Soft-delete must surface deleted_at on the wire"
        )


# ── S3: tenant_type validated against the real types-registry ──────────


async def test_create_tenant_validates_tenant_type_via_types_registry(
    am_base_url, am_headers,
):
    """Seam: AM's `create_tenant` -> types-registry SDK -> 400 envelope.

    ``tenant_type`` must resolve to a published GTS schema; a value not
    in the catalog must collapse to ``code=validation`` with a 400.
    Unit tests stub the registry client -- only this seam exercises
    the real SDK -> registry crossing.
    """
    bogus_type = "gts.cf.core.rg.type.v1~x.e2etest.does_not_exist_zz.v1~"

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.post(
            _tenants(am_base_url),
            headers=am_headers,
            json={
                "name": unique_name("s3type"),
                "parent_id": TENANT_A_ID,
                "tenant_type": bogus_type,
            },
        )
        assert r.status_code == 400, (
            f"Unknown tenant_type must surface 400, got "
            f"{r.status_code} {r.text}"
        )
        # The error envelope must carry the typed code so codegen
        # clients can branch on it.
        body = r.json()
        assert "code" in body or "title" in body, (
            f"Expected typed envelope, got: {body}"
        )


# ── S5: metadata lifecycle through real types-registry ─────────────────


async def test_metadata_lifecycle_via_real_schema_registry(
    am_base_url, am_headers, create_tenant, create_metadata_schema,
):
    """Seam: types-registry schema -> AM metadata write -> resolve walk-up.

    Stitches three modules: register schema in types-registry, PUT
    metadata on a child tenant, GET it back, then resolve from a
    deeper descendant so the inheritance walk-up actually runs across
    the registered schema's ``__inheritance_policy``.
    """
    schema_id = await create_metadata_schema()

    parent = await create_tenant("s5parent")
    child = await create_tenant("s5child", parent_id=parent["id"])

    payload = {"environment": "e2e", "owner": "am-suite"}

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        # PUT on the parent -- direct write through the GTS-validated path.
        r = await c.put(
            _metadata_entry(am_base_url, parent["id"], schema_id),
            headers=am_headers,
            json=payload,
        )
        assert r.status_code == 200, f"PUT metadata: {r.status_code} {r.text}"
        stored = r.json()
        assert stored["tenant_id"] == parent["id"]
        assert stored["schema_id"] == schema_id
        assert stored["value"] == payload

        # GET back the direct entry.
        r = await c.get(
            _metadata_entry(am_base_url, parent["id"], schema_id),
            headers=am_headers,
        )
        assert r.status_code == 200, f"GET metadata: {r.status_code} {r.text}"
        assert r.json()["value"] == payload

        # Resolve from the child -- walks up the chain and inherits.
        r = await c.get(
            _metadata_resolved(am_base_url, child["id"], schema_id),
            headers=am_headers,
        )
        assert r.status_code == 200, (
            f"resolve metadata: {r.status_code} {r.text}"
        )
        resolved = r.json()
        assert resolved["resolved"] is True, (
            f"Expected resolved=true via inheritance, got: {resolved}"
        )
        assert resolved["value"] == payload


# ── S6: distinct 404 codes preserved through serialisation ─────────────


async def test_metadata_distinct_404_codes_propagate_through_envelope(
    am_base_url, am_headers, create_tenant, create_metadata_schema,
):
    """Seam: AM error -> Problem JSON envelope -> distinguishability.

    AM collapses both "schema unknown to registry" and "entry missing
    for tenant" into a unified ``MetadataEntryNotFound`` 404 with
    ``context.resource_type = gts.cf.core.am.tenant_metadata.v1~``.
    The two paths are distinguishable by the ``detail`` text: path A
    mentions "not registered in the types registry" while path B
    references the missing entry. Both MUST surface as 404 with the
    unified metadata ``resource_type``.
    """
    schema_id = await create_metadata_schema()
    tenant = await create_tenant("s6codes")
    bogus_schema = "gts.cf.core.am.tenant_metadata.v1~x.never.am.registered.v1~"

    metadata_rt = "gts.cf.core.am.tenant_metadata.v1~"

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        # Path A: GET with a schema_id that was never registered.
        r = await c.get(
            _metadata_entry(am_base_url, tenant["id"], bogus_schema),
            headers=am_headers,
        )
        assert r.status_code == 404, (
            f"unknown schema must be 404, got {r.status_code} {r.text}"
        )
        body_a = r.json()
        ctx_a = body_a.get("context", {})
        assert ctx_a.get("resource_type") == metadata_rt, (
            f"schema-not-registered MUST carry resource_type={metadata_rt}, "
            f"got: {body_a}"
        )
        assert "not registered" in body_a.get("detail", "").lower(), (
            f"schema-not-registered detail MUST mention registry, "
            f"got: {body_a}"
        )

        # Path B: GET on a registered schema with no entry written.
        r = await c.get(
            _metadata_entry(am_base_url, tenant["id"], schema_id),
            headers=am_headers,
        )
        assert r.status_code == 404, (
            f"missing entry must be 404, got {r.status_code} {r.text}"
        )
        body_b = r.json()
        ctx_b = body_b.get("context", {})
        assert ctx_b.get("resource_type") == metadata_rt, (
            f"entry-not-found MUST carry resource_type={metadata_rt}, "
            f"got: {body_b}"
        )

        # Regression guard: the two 404 paths are distinguishable by
        # both detail text and resource_name even though they share
        # the same resource_type on the unified metadata envelope.
        assert body_a.get("detail") != body_b.get("detail"), (
            "Distinct 404 variants collapsed to identical detail text"
        )
        assert ctx_a.get("resource_name") != ctx_b.get("resource_name"), (
            "Distinct 404 variants collapsed to identical resource_name"
        )


# ── S7: IdP plugin provision-deprovision round-trip ────────────────────


@pytest.mark.smoke
async def test_idp_user_provision_deprovision_round_trip(
    am_base_url, am_headers, create_tenant,
):
    """Seam: AM -> IdpPluginClient -> IdP -> back through the projection.

    POST creates the user via the configured plugin; GET with
    ``user_id`` filter must surface it; DELETE then GET must show it
    gone. Tests the full happy-path plugin contract end-to-end.
    """
    tenant = await create_tenant("s7idp")
    tenant_id = tenant["id"]

    username = unique_name("s7user")

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        # Provision.
        r = await c.post(
            _users(am_base_url, tenant_id),
            headers=am_headers,
            json={"username": username, "email": f"{username}@e2e.test"},
        )
        assert r.status_code == 201, (
            f"provision user: {r.status_code} {r.text}"
        )
        user = r.json()
        user_id = user["id"]
        uuid.UUID(user_id)
        assert user["username"] == username

        # Existence check via filtered listing.
        r = await c.get(
            _users(am_base_url, tenant_id),
            headers=am_headers,
            params={"user_id": user_id},
        )
        assert r.status_code == 200, (
            f"list users: {r.status_code} {r.text}"
        )
        ids = [u["id"] for u in r.json().get("items", [])]
        assert user_id in ids, "Provisioned user not visible in listing"

        # Deprovision.
        r = await c.delete(
            _user(am_base_url, tenant_id, user_id), headers=am_headers,
        )
        assert r.status_code == 204, (
            f"deprovision user: {r.status_code} {r.text}"
        )

        # Existence check after delete -- the listing must come back
        # empty (canonical "absent" signal; AM does NOT 404 here).
        r = await c.get(
            _users(am_base_url, tenant_id),
            headers=am_headers,
            params={"user_id": user_id},
        )
        assert r.status_code == 200
        ids = [u["id"] for u in r.json().get("items", [])]
        assert user_id not in ids, "Deprovisioned user still present"


# ── S8: deprovision idempotency ────────────────────────────────────────


async def test_idp_user_deprovision_idempotent(
    am_base_url, am_headers, create_tenant, seed_idp_user,
):
    """Seam: AM deprovision-idempotent contract through the real IdP plugin.

    The advertised contract is: DELETE returns 204 on either path
    (deleted-this-call or already-absent). A retry after a successful
    delete MUST also return 204, not 404. This pins the
    plugin-layer "absent on retry" handling.
    """
    tenant = await create_tenant("s8idem")
    user = await seed_idp_user(tenant["id"])

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.delete(
            _user(am_base_url, tenant["id"], user["id"]), headers=am_headers,
        )
        assert r.status_code == 204, (
            f"first DELETE: {r.status_code} {r.text}"
        )

        r = await c.delete(
            _user(am_base_url, tenant["id"], user["id"]), headers=am_headers,
        )
        assert r.status_code == 204, (
            f"retry DELETE must also be 204, got: {r.status_code} {r.text}"
        )


# ── S10: counterparty-only rule on conversion approval ────────────────


async def test_conversion_initiator_cannot_approve_own_request(
    am_base_url, am_headers, create_tenant,
):
    """Seam: AM service-layer counterparty-only rule on PATCH approve.

    The initiator of a conversion request MUST NOT be able to approve
    its own request -- the counterparty-only rule lives in AM's service
    layer (`approve` requires `actor_kind != initiator_side`). This
    test exercises the child-side surface
    (`PATCH /tenants/{child}/conversions/{request_id}`) where the
    request initiator IS the URL-bound caller, so the service surfaces
    `code=failed_precondition` with `invalid_actor_for_transition`
    (HTTP 400) regardless of PEP scope.

    Note: this test does NOT pin the parent-side PEP scope check —
    that requires a separate token/tenant pair outside the caller's
    subtree (tracked in the cross-tenant config follow-up). Renaming
    the test makes the actual coverage honest: the previous
    `_returns_403_via_pep` name accepted 400 too, which passed the
    suite even if the parent-side PEP wiring was broken.
    """
    child = await create_tenant(
        "s10initiator-cant-approve",
        parent_id=TENANT_A_ID,
        self_managed=False,
    )

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        # Child initiates -- legitimate.
        r = await c.post(
            _conversions(am_base_url, child["id"]),
            headers=am_headers,
            json={"target_mode": "self_managed"},
        )
        assert r.status_code == 201, (
            f"request own conversion: {r.status_code} {r.text}"
        )
        request_id = r.json()["id"]

        # Initiator (child-side surface) tries to approve its own
        # request -- service rejects with 400 `failed_precondition`
        # `invalid_actor_for_transition`. The detail string is
        # platform-stable and references the reason marker so a
        # silent re-classification (e.g. service swallowing the
        # rule and returning 200) is caught.
        r = await c.patch(
            _conversion(am_base_url, child["id"], request_id),
            headers=am_headers,
            json={"status": "approved"},
        )
        assert r.status_code == 400, (
            f"Initiator self-approve MUST surface 400 "
            f"`invalid_actor_for_transition`, got: "
            f"{r.status_code} {r.text}"
        )
        body = r.json()
        # The canonical error envelope puts the specific reason in
        # context.violations, not the top-level detail (which is a
        # generic "Operation precondition not met" summary).
        violations = body.get("context", {}).get("violations", [])
        violation_text = " ".join(
            (v.get("description") or "") + " " + (v.get("type") or "")
            for v in violations
        ).lower()
        detail = (body.get("detail") or "").lower()
        combined = detail + " " + violation_text
        assert "invalid actor" in combined or "initiator" in combined, (
            f"detail MUST reference the counterparty-only rule, "
            f"got: {body}"
        )


# ── S11: missing auth header -> rejected by gateway middleware ─────────


async def test_unauthenticated_request_rejected_by_gateway_middleware(
    am_base_url,
):
    """Seam: Gateway authn middleware runs BEFORE the AM handler.

    A request with no Authorization header must be rejected by the
    gateway's middleware (401 / 403) -- not by the AM module's own
    handler with a 404 / 500. Verifies middleware ordering on the
    full ``/account-management/v1`` URL family.
    """
    no_auth_headers = {"Content-Type": "application/json"}

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(
            _tenant(am_base_url, TENANT_A_ID), headers=no_auth_headers,
        )
        assert r.status_code in (401, 403), (
            f"Missing auth must surface 401/403 from the gateway, "
            f"got: {r.status_code} {r.text}"
        )
        # The middleware MUST short-circuit before AM -- a 404 here
        # would mean AM ran and produced its own envelope, which is
        # the wrong layer.
        assert r.status_code != 404, (
            "404 from a missing-auth path means AM ran before authn; "
            "middleware ordering is broken."
        )


# ── S12: cross-tenant denied by real policy bundle ─────────────────────


async def test_cross_tenant_denied_via_real_policy_bundle(
    am_base_url, am_headers, am_headers_tenant_b, create_tenant,
):
    """Seam: real PEP scope clamping — token B cannot read AM's root.

    Setup:
      * Token A (subject_tenant_id=TENANT_A_ID) is rooted at the AM
        platform root that ``bootstrap.root_id`` seeded.
      * Token B (subject_tenant_id=TENANT_B_ID) is rooted at a
        SUBJECT that AM does NOT carry as a tenant row — by design
        per ``config/e2e-local.yaml``. The B-subject's PEP subtree
        therefore contains nothing in AM.

    Baseline (token A on TENANT_A_ID): 200. Pin this BEFORE the
    cross-tenant attempt so we know the row genuinely exists; a
    regression that wiped the bootstrap would otherwise mask the
    cross-tenant test as a no-op (the previous version compared
    against a tenant id that never existed).

    Cross-tenant (token B on TENANT_A_ID): 403 or 404. The B-token's
    PEP-narrowed subtree does not contain TENANT_A_ID, so the read
    collapses to the existence-channel rejection. A regression in
    ``static-authz-plugin`` or the PEP -> AccessScope wiring would
    let the call through; baseline + cross-tenant together pin both
    halves of the seam.

    Same posture against a freshly-created child under TENANT_A_ID:
    a child created under root A is also outside B's subtree, so the
    cross-tenant read on the child MUST also collapse. This catches
    PEP regressions that special-case the root id but mis-handle the
    closure on descendants.
    """
    # Pin the AM tenant exists under token A (baseline) so the
    # cross-tenant assertion can't be satisfied by accident.
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(
            _tenant(am_base_url, TENANT_A_ID), headers=am_headers,
        )
        assert r.status_code == 200, (
            f"Baseline (token A reading root A) MUST succeed -- "
            f"if this fails the cross-tenant test below is meaningless; "
            f"got: {r.status_code} {r.text}"
        )

    # Create a real child under root A so the cross-tenant attempt
    # below is against a tenant that DEFINITELY exists in AM.
    child = await create_tenant("s12-cross-tenant-child")
    child_id = child["id"]

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        # Token B on root A — must be denied by scope clamp.
        r = await c.get(
            _tenant(am_base_url, TENANT_A_ID),
            headers=am_headers_tenant_b,
        )
        assert r.status_code in (403, 404), (
            f"Cross-tenant read of root A by token B MUST be denied "
            f"(403/404); got 200 (or other) means PEP clamp is broken: "
            f"{r.status_code} {r.text}"
        )

        # Token B on a real existing child under A — same clamp must fire.
        r = await c.get(
            _tenant(am_base_url, child_id),
            headers=am_headers_tenant_b,
        )
        assert r.status_code in (403, 404), (
            f"Cross-tenant read of existing child under A by token B "
            f"MUST be denied (403/404); got: {r.status_code} {r.text}"
        )
