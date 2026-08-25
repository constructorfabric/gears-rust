"""AuthZ and idempotency E2E tests for the usage-collector gear.

These exercise the real PDP pipeline: static-authz returns a row scope clamped
to the caller's own tenant, and the gear matches each record's attribution
tuple against that scope. Tenants A and B are siblings under one root, so
neither is in the other's subtree.
"""

from .conftest import (
    TENANT_B,
    TOKEN_B,
    accepted_records,
    now_rfc3339,
    record_payload,
    unique_suffix,
    window_filter,
)


async def test_cross_tenant_ingest_denied(api, make_usage_type):
    """Seam: per-record attribution gate vs. the PDP-returned scope.

    Tenant A's token submits a record attributed to tenant B. Ingest is a BATCH
    path that authorizes per record (grouped by attribution tuple), so the
    denial arrives as 207 with a `rejected` entry — NOT a top-level 403.
    """
    gts_id, _ = await make_usage_type()
    async with api() as client:
        resp = await client.post("/records", json={"records": [
            record_payload(gts_id, tenant_id=TENANT_B),
        ]})

    assert resp.status_code == 207, (
        f"a foreign-tenant record must be rejected per-record, got "
        f"{resp.status_code}: {resp.text}"
    )
    results = resp.json()["results"]
    assert len(results) == 1
    assert results[0]["outcome"] == "rejected", results[0]
    assert results[0]["index"] == 0
    # `error` carries the full canonical Problem verbatim (api/rest/dto.rs
    # CreateUsageRecordResultDto::Rejected), not just a bare message — assert on
    # its `status` being the forbidden class so a record rejected for an
    # unrelated reason (unknown usage type, validation failure, storage error)
    # cannot masquerade as the attribution gate having fired.
    assert results[0]["error"]["status"] == 403, results[0]["error"]


async def test_cross_tenant_read_no_existence_leak(api, make_usage_type):
    """Seam: a foreign reader gets 404, not 403 — no existence leak.

    403 would confirm the record exists. The single-record read path fails at
    the top level (unlike batch ingest).
    """
    gts_id, _ = await make_usage_type()
    async with api() as client:
        created = accepted_records(
            await client.post("/records", json={"records": [record_payload(gts_id)]})
        )
        record_id = created[0]["id"]

    async with api(TOKEN_B) as foreign:
        resp = await foreign.get(f"/records/{record_id}")

    assert resp.status_code == 404, (
        f"a foreign tenant must not learn the record exists, got "
        f"{resp.status_code}: {resp.text}"
    )


async def test_idempotent_repost_returns_same_record(api, make_usage_type):
    """Seam: dedup on (tenant_id, gts_id, idempotency_key, created_at).

    An identical re-POST is deduplicated, not duplicated: same record id back,
    and one row visible in the listing (ADR-0004, ADR-0014 — created_at is part
    of the dedup identity, so it is pinned here rather than regenerated).
    """
    gts_id, _ = await make_usage_type()
    payload = record_payload(
        gts_id,
        idempotency_key=f"e2e-idem-fixed-{unique_suffix()}",
        created_at=now_rfc3339(),
    )

    async with api() as client:
        first = accepted_records(await client.post("/records", json={"records": [payload]}))
        second = accepted_records(await client.post("/records", json={"records": [payload]}))

        listing = await client.get("/records", params={
            "gts_id": gts_id, "$filter": window_filter(), "limit": 1000,
        })

    assert first[0]["id"] == second[0]["id"], "re-POST must dedup onto the same record"
    assert listing.status_code == 200, listing.text
    ids = [item["id"] for item in listing.json()["items"]]
    assert ids.count(first[0]["id"]) == 1, f"expected exactly one row, saw {ids}"
