"""CredStore E2E: optimistic concurrency (generation-bound ETag / If-Match).

GET returns a strong, generation-bound ETag of the form ``"<id>.<version>"``
(``id`` = the metadata row's UUID, minted fresh for every recreated secret;
``version`` = the per-generation monotonic counter). PUT and DELETE honour
``If-Match: *`` (target must exist) and ``If-Match: "<id>.<version>"``.
A failed precondition is the canonical 409 (OPTIMISTIC_LOCK_FAILURE) — the
canonical error model has no 412. A validator minted for an earlier
generation of a deleted-and-recreated secret never matches, even when the
version counters coincide (no ABA lost update).
"""
import asyncio
import re
import uuid

import httpx
import pytest

# ``"<uuid>.<version>"`` — the strong validator's wire shape.
ETAG_RE = re.compile(r'^"([0-9a-f-]{36})\.(\d+)"$')


def parse_etag(etag: str) -> tuple[str, int]:
    """Split a composite ETag into its (generation id, version) halves."""
    m = ETAG_RE.match(etag)
    assert m, f"ETag {etag!r} is not a composite <id>.<version> validator"
    return m.group(1), int(m.group(2))


class TestIfMatchPut:
    """Guarded updates."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_version_bumps_and_guarded_put_succeeds(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """ETag tracks the version within a generation; a matching If-Match PUT commits."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oc"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v1", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            etag = resp.headers["etag"]
            gen, version = parse_etag(etag)
            assert version == 1

            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": etag},
                json={"value": "v2", "sharing": "tenant"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["value"] == "v2"
            assert resp.headers["etag"] == f'"{gen}.2"', (
                "an in-place overwrite bumps the version within the same generation"
            )

    @pytest.mark.asyncio
    async def test_stale_if_match_put_conflicts_without_side_effect(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """A stale If-Match PUT is a 409 and changes nothing."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oc"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v1", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            etag = resp.headers["etag"]
            gen, _ = parse_etag(etag)

            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": f'"{gen}.99"'},
                json={"value": "lost-update", "sharing": "tenant"},
            )
            assert resp.status_code == 409, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["value"] == "v1"
            assert resp.headers["etag"] == etag

    @pytest.mark.asyncio
    async def test_recreated_secret_rejects_previous_generation_validator(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """No ABA: delete + recreate restarts the version counter, but the old
        generation's validator must never match the new row."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oc-aba"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "gen1", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            gen1_etag = resp.headers["etag"]
            _, gen1_version = parse_etag(gen1_etag)

            # Delete and recreate: a fresh generation whose counter restarts.
            resp = await client.delete(
                f"{secrets_url}/{ref}", headers=tenant_a_headers
            )
            assert resp.status_code == 204, resp.text
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "gen2", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            gen2_etag = resp.headers["etag"]
            _, gen2_version = parse_etag(gen2_etag)
            assert gen2_version == gen1_version, "the ABA setup: versions coincide"
            assert gen2_etag != gen1_etag, "a recreated secret is a new generation"

            # The stale generation's validator must not overwrite gen2.
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": gen1_etag},
                json={"value": "stale-write", "sharing": "tenant"},
            )
            assert resp.status_code == 409, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["value"] == "gen2", "no lost update across generations"

    @pytest.mark.asyncio
    async def test_if_match_on_missing_target_conflicts(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """If-Match requires an existing target: PUT on a fresh ref is 409."""
        ref = unique_ref("e2e-oc-missing")

        async with httpx.AsyncClient(timeout=10.0) as client:
            for if_match in ("*", f'"{uuid.uuid4()}.1"'):
                resp = await client.put(
                    f"{secrets_url}/{ref}",
                    headers={**tenant_a_headers, "If-Match": if_match},
                    json={"value": "v", "sharing": "tenant"},
                )
                assert resp.status_code == 409, (if_match, resp.text)

            # No side effect: the reference was never created.
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 404

    @pytest.mark.asyncio
    async def test_malformed_if_match_is_400(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """If-Match must be * or a quoted <id>.<version> pair."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oc"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v1", "sharing": "tenant"},
            )
            # A bare quoted version (the pre-composite format) is malformed too.
            for bad in ("not-a-version", '"1"', '"not-a-uuid.1"'):
                resp = await client.put(
                    f"{secrets_url}/{ref}",
                    headers={**tenant_a_headers, "If-Match": bad},
                    json={"value": "v2", "sharing": "tenant"},
                )
                assert resp.status_code == 400, (bad, resp.text)


class TestIfMatchDelete:
    """Guarded deletes."""

    @pytest.mark.asyncio
    async def test_stale_if_match_delete_conflicts_then_matching_deletes(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """A stale If-Match DELETE is a 409; the matching one commits."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oc-del"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v1", "sharing": "tenant"},
            )
            # Bump to version 2 so the guard is meaningful.
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v2", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            etag = resp.headers["etag"]
            gen, version = parse_etag(etag)
            assert version == 2

            resp = await client.delete(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": f'"{gen}.1"'},
            )
            assert resp.status_code == 409, resp.text
            # Still readable — the stale delete had no effect.
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 200
            assert resp.headers["etag"] == etag

            resp = await client.delete(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": etag},
            )
            assert resp.status_code == 204, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 404

    @pytest.mark.asyncio
    async def test_if_match_star_delete_requires_existence(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """If-Match: * deletes an existing secret; a missing one is 404."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oc-star"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.delete(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": "*"},
            )
            assert resp.status_code == 404, resp.text

            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v", "sharing": "tenant"},
            )
            resp = await client.delete(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": "*"},
            )
            assert resp.status_code == 204, resp.text


class TestConcurrentRacingRequests:
    """Genuinely parallel requests (``asyncio.gather``), not the sequential
    ``await``s the rest of this module uses. These exercise the server-side
    optimistic-lock CAS and the crosswise dual-write fence under real overlap.
    """

    @pytest.mark.asyncio
    async def test_racing_guarded_puts_exactly_one_wins(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """N concurrent If-Match PUTs sharing one validator: exactly one commits,
        the rest are 409, and the version advances by exactly one — no lost
        update, no double-apply. This is the CAS the file name promises but the
        sequential tests never actually race."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-race"))
        n = 8

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v0", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            etag = resp.headers["etag"]
            gen, base_version = parse_etag(etag)

            async def guarded_put(i: int) -> int:
                r = await client.put(
                    f"{secrets_url}/{ref}",
                    headers={**tenant_a_headers, "If-Match": etag},
                    json={"value": f"racer-{i}", "sharing": "tenant"},
                )
                return r.status_code

            statuses = await asyncio.gather(*(guarded_put(i) for i in range(n)))

            wins = [s for s in statuses if s == 204]
            conflicts = [s for s in statuses if s == 409]
            assert len(wins) == 1, f"exactly one guarded PUT must win, got {statuses}"
            assert len(conflicts) == n - 1, f"losers must all be 409, got {statuses}"

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            new_gen, new_version = parse_etag(resp.headers["etag"])
            assert new_gen == gen, "in-place overwrite stays in the same generation"
            assert new_version == base_version + 1, (
                "version advances by exactly one commit, not once per racer"
            )
            assert resp.json()["value"].startswith("racer-")

    @pytest.mark.asyncio
    async def test_racing_unconditional_puts_never_leak_and_heal_on_rewrite(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """Concurrent unconditional PUTs are the crosswise dual-write case. The
        fence may legitimately fail closed (404) on a poisoned interleave, so a
        read afterwards is either the winner's value or 404 — never a value no
        one wrote. A settling rewrite always heals the ref back to a known-good
        value (self-heal on rewrite)."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-race"))
        n = 8
        written = {f"w-{i}" for i in range(n)}

        async with httpx.AsyncClient(timeout=10.0) as client:

            async def put(i: int) -> int:
                r = await client.put(
                    f"{secrets_url}/{ref}",
                    headers=tenant_a_headers,
                    json={"value": f"w-{i}", "sharing": "tenant"},
                )
                return r.status_code

            statuses = await asyncio.gather(*(put(i) for i in range(n)))
            assert all(s in (201, 204) for s in statuses), statuses

            # By-design outcomes only: the winner's value (200) or fail-closed
            # (404) on a crosswise interleave. Never a foreign/garbled value.
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code in (200, 404), resp.text
            if resp.status_code == 200:
                assert resp.json()["value"] in written, "final value must be one racer's"

            # A settling rewrite heals the ref regardless of the race outcome.
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "settled", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 200, resp.text
            assert resp.json()["value"] == "settled"
