"""E2E tests for the upstream circuit breaker (ADR-0005, F0006).

The breaker is per replica; the suite runs one server, so it counts exactly
this suite's requests. Each test has its own upstream, and its own key on
the mock's `/flaky/{key}/{n}` (503 for the first n calls, 200 after), which
also tells whether a request reached the upstream: the call number in a
later answer counts only the calls that did.
"""
import asyncio
import uuid

import httpx
import pytest

from .helpers import (
    PROXY_SCHEMA,
    assert_problem,
    assert_problem_body,
    create_route,
    create_upstream,
    create_upstream_raw,
    delete_upstream,
    err_type,
    list_all,
    traced,
    unique_alias,
    update_upstream,
    update_upstream_raw,
)

_OPENS = "negative-18.9-circuit-opens-after-consecutive-failures"
_PROBES = "positive-18.10-half-open-probe-closes-circuit"
_RESETS = "positive-18.11-config-change-or-delete-resets-circuit"
_WRITE_VALIDATION = "negative-2.15-write-validation-rejected-400"

# Opens on the second failure in a row, probes after a second.
_CB = {"failure_threshold": 2, "timeout_seconds": 1, "success_threshold": 1}
# Stays open for the whole test.
_CB_LONG = {**_CB, "timeout_seconds": 60}
# Past `timeout_seconds: 1`, with room for a busy runner.
_HALF_OPEN_WAIT = 1.3


async def _breaker_upstream(client, base, headers, mock_url, cleanup, prefix, cb=_CB, alias=None):
    alias = alias or unique_alias(prefix)
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias, circuit_breaker=cb,
    ))
    for path in ("/flaky", "/status"):
        await create_route(client, base, headers, upstream["id"], ["GET"], path)
    return alias, upstream


async def _get(client, base, headers, alias, path):
    return await client.get(f"{base}/oagw/v1/proxy/{alias}{path}", headers=headers)


def _flaky(failures: int) -> str:
    """A fresh `/flaky` path that fails `failures` times, then recovers."""
    return f"/flaky/{uuid.uuid4().hex}/{failures}"


def _assert_upstream(resp: httpx.Response, status: int) -> None:
    """The upstream's own answer, passed through."""
    assert resp.status_code == status, resp.text[:500]
    assert resp.headers.get("x-oagw-error-source") == "upstream", resp.headers


def _assert_open(resp: httpx.Response, retry_after: int = 1) -> None:
    """The gateway's fail-fast 503 while the circuit is open; `Retry-After`
    counts down from `timeout_seconds` in whole seconds."""
    assert_problem(resp, 503, category="service_unavailable")
    assert 1 <= int(resp.headers["retry-after"]) <= retry_after, resp.headers


async def _open(client, base, headers, alias, path="/status/503", retry_after=1):
    """Two failures in a row: the next request is refused."""
    for _ in range(2):
        _assert_upstream(await _get(client, base, headers, alias, path), 503)
    _assert_open(await _get(client, base, headers, alias, "/status/200"), retry_after)


@pytest.mark.scenario(_OPENS)
@pytest.mark.asyncio
async def test_circuit_opens_after_consecutive_failures(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.9: the failure after a success starts the count again; two
    in a row open the circuit, and the next request is refused with 503 and
    `Retry-After` without reaching the upstream."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _breaker_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-open",
        )
        flaky = _flaky(100)
        get = lambda path: _get(client, oagw_base_url, oagw_headers, alias, path)  # noqa: E731

        _assert_upstream(await get(flaky), 503)
        _assert_upstream(await get("/status/200"), 200)
        _assert_upstream(await get(flaky), 503)
        _assert_upstream(await get("/status/502"), 502)
        _assert_open(await get(flaky))
        _assert_open(await get("/status/200"))

        # The probe is the third call the upstream sees for this key: the
        # refused requests never reached it.
        await asyncio.sleep(_HALF_OPEN_WAIT)
        probe = await get(flaky)
        _assert_upstream(probe, 503)
        assert probe.json()["call"] == 3, probe.text


@pytest.mark.scenario(_OPENS)
@pytest.mark.asyncio
async def test_disabled_breaker_lets_every_request_through(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.9: with `enabled: false` the config is kept and nothing is refused."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _breaker_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-off",
            cb={**_CB, "enabled": False},
        )
        assert upstream["circuit_breaker"]["enabled"] is False
        for _ in range(4):
            _assert_upstream(await _get(client, oagw_base_url, oagw_headers, alias, "/status/503"), 503)
        _assert_upstream(await _get(client, oagw_base_url, oagw_headers, alias, "/status/200"), 200)


@pytest.mark.scenario(_PROBES, part="A")
@pytest.mark.asyncio
async def test_half_open_probe_closes_the_circuit(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.10-A: after the timeout the upstream has recovered; the
    probe's 200 closes the circuit, and one failure no longer opens it."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _breaker_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-probe",
        )
        get = lambda path: _get(client, oagw_base_url, oagw_headers, alias, path)  # noqa: E731
        flaky = _flaky(2)
        await _open(client, oagw_base_url, oagw_headers, alias, flaky)

        await asyncio.sleep(_HALF_OPEN_WAIT)
        probe = await get(flaky)
        _assert_upstream(probe, 200)
        assert probe.json()["call"] == 3, probe.text

        _assert_upstream(await get("/status/503"), 503)
        _assert_upstream(await get("/status/200"), 200)


@pytest.mark.scenario(_PROBES, part="B")
@pytest.mark.asyncio
async def test_failed_probe_opens_the_circuit_again(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.10-B: a probe the upstream still fails opens the circuit for another timeout."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _breaker_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-reopen",
        )
        get = lambda path: _get(client, oagw_base_url, oagw_headers, alias, path)  # noqa: E731
        await _open(client, oagw_base_url, oagw_headers, alias)

        await asyncio.sleep(_HALF_OPEN_WAIT)
        _assert_upstream(await get("/status/503"), 503)
        _assert_open(await get("/status/200"))


async def _opened_upstream(client, base, headers, mock_url, cleanup, prefix):
    """An upstream whose circuit is open for a minute."""
    alias, upstream = await _breaker_upstream(
        client, base, headers, mock_url, cleanup, prefix, cb=_CB_LONG,
    )
    await _open(client, base, headers, alias, retry_after=60)
    return alias, upstream


@pytest.mark.scenario(_RESETS, part="A")
@pytest.mark.asyncio
async def test_unchanged_write_keeps_the_circuit_open(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.11-A: a write that keeps the breaker and the endpoints keeps the circuit open."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _opened_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-same",
        )
        await update_upstream(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, circuit_breaker=_CB_LONG, tags=["changed"],
        )
        _assert_open(await _get(client, oagw_base_url, oagw_headers, alias, "/status/200"), 60)


@pytest.mark.scenario(_RESETS, part="B")
@pytest.mark.asyncio
async def test_config_change_resets_the_breaker(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.11-B: a changed breaker closes the circuit at once, and GET returns it."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _opened_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-change",
        )
        changed = {**_CB_LONG, "failure_threshold": 3}
        updated = await update_upstream(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, circuit_breaker=changed,
        )
        assert updated["circuit_breaker"] == {"sharing": "private", "enabled": True, **changed}
        _assert_upstream(await _get(client, oagw_base_url, oagw_headers, alias, "/status/200"), 200)

        stored = await client.get(
            f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}", headers=oagw_headers,
        )
        assert stored.status_code == 200, stored.text[:500]
        assert stored.json()["circuit_breaker"] == updated["circuit_breaker"]


@pytest.mark.scenario(_RESETS, part="B")
@pytest.mark.asyncio
async def test_removed_and_re_added_breaker_starts_closed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.11-B: the breaker removed and the same config added back starts closed."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _opened_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-readd",
        )
        removed = await update_upstream(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url, alias=alias,
        )
        assert removed.get("circuit_breaker") is None, removed
        readded = await update_upstream(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, circuit_breaker=_CB_LONG,
        )
        assert readded["circuit_breaker"] == upstream["circuit_breaker"]
        _assert_upstream(await _get(client, oagw_base_url, oagw_headers, alias, "/status/200"), 200)


@pytest.mark.scenario(_RESETS, part="C")
@pytest.mark.asyncio
async def test_delete_and_recreate_starts_closed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.11-C: an upstream re-created under the same alias starts with a closed circuit."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _opened_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-delete",
        )

        deleted = await delete_upstream(client, oagw_base_url, oagw_headers, upstream["id"])
        assert deleted.status_code == 204, deleted.text[:500]
        await _breaker_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-delete",
            cb=_CB_LONG, alias=alias,
        )
        _assert_upstream(await _get(client, oagw_base_url, oagw_headers, alias, "/status/200"), 200)


@pytest.mark.asyncio
async def test_circuit_breaker_defaults_round_trip(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """ADR-0005 defaults: omitted fields are stored and returned with their
    defaults; an upstream without the field has none."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        with_cb = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("cb-defaults"), circuit_breaker={"failure_threshold": 4},
        ))
        without = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("cb-none"),
        ))
        expected = {
            "sharing": "private",
            "enabled": True,
            "failure_threshold": 4,
            "timeout_seconds": 30,
            "success_threshold": 3,
        }
        for upstream, cb in ((with_cb, expected), (without, None)):
            assert upstream.get("circuit_breaker") == cb, upstream
            stored = await client.get(
                f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}", headers=oagw_headers,
            )
            assert stored.status_code == 200, stored.text[:500]
            assert stored.json().get("circuit_breaker") == cb, stored.text


def _zero(field: str) -> dict:
    return {**_CB, field: 0}


def _format_error(detail: str, instance: str) -> dict:
    """The whole body of a rule without a field."""
    return {
        "type": err_type("invalid_argument"),
        "title": "Invalid Argument",
        "status": 400,
        "detail": detail,
        "instance": instance,
        "context": {"format": detail, "resource_type": PROXY_SCHEMA},
    }


_FIELDS = ["failure_threshold", "timeout_seconds", "success_threshold"]


@pytest.mark.scenario(_WRITE_VALIDATION, part="A")
@pytest.mark.asyncio
@pytest.mark.parametrize("enabled", [True, False], ids=["enabled", "disabled"])
@pytest.mark.parametrize("field", _FIELDS)
async def test_zero_value_rejected_on_create(
    field, enabled, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15-A: a breaker value of 0 is refused, enabled or not, and nothing is stored."""
    _ = mock_upstream
    alias = unique_alias("cb-zero")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=alias, circuit_breaker={**_zero(field), "enabled": enabled},
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem_body(resp, _format_error(
            f"circuit_breaker.{field} must be at least 1", "/oagw/v1/upstreams",
        ))
        upstreams = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert all(u["alias"] != alias for u in upstreams)


@pytest.mark.scenario(_WRITE_VALIDATION, part="A")
@pytest.mark.asyncio
@pytest.mark.parametrize("field", _FIELDS)
async def test_zero_value_rejected_on_update(
    field, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15-A: the same refusal on PUT; the stored upstream is unchanged."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _breaker_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "cb-zero-put",
        )
        url = f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}"
        before = await client.get(url, headers=oagw_headers)
        assert before.status_code == 200, before.text[:500]

        resp = await update_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), upstream["id"], mock_upstream_url,
            alias=alias, circuit_breaker=_zero(field),
        )
        assert_problem_body(resp, _format_error(
            f"circuit_breaker.{field} must be at least 1", f"/oagw/v1/upstreams/{upstream['id']}",
        ))

        after = await client.get(url, headers=oagw_headers)
        assert after.status_code == 200, after.text[:500]
        assert after.json() == before.json()


_MAX = {"failure_threshold": 1000, "timeout_seconds": 86400, "success_threshold": 100}


@pytest.mark.scenario(_WRITE_VALIDATION, part="A")
@pytest.mark.asyncio
@pytest.mark.parametrize("field", _FIELDS)
async def test_value_above_maximum_rejected(
    field, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15-A: a breaker value above its maximum is refused and
    nothing is stored; the maximum itself is accepted."""
    _ = mock_upstream
    alias = unique_alias("cb-max")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=alias, circuit_breaker={**_CB, field: _MAX[field] + 1},
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem_body(resp, _format_error(
            f"circuit_breaker.{field} must be at most {_MAX[field]}", "/oagw/v1/upstreams",
        ))
        upstreams = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert all(u["alias"] != alias for u in upstreams)

        at_max = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, circuit_breaker={**_CB, field: _MAX[field]},
        ))
        assert at_max["circuit_breaker"][field] == _MAX[field], at_max


@pytest.mark.scenario(_OPENS)
@pytest.mark.asyncio
async def test_unknown_field_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.9: a misspelled breaker field is refused like any body
    that does not deserialize (422), not stored with the default."""
    _ = mock_upstream
    alias = unique_alias("cb-typo")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, circuit_breaker={"failure_treshold": 1},
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert resp.status_code == 422, resp.text[:500]
        upstreams = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert all(u["alias"] != alias for u in upstreams)
