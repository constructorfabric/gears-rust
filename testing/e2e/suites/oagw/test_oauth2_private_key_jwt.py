"""E2E tests for the OAGW OAuth2 client credentials auth plugin with
`private_key_jwt` client authentication (RFC 7523 §2.2).

Each test generates a key pair, stores the private key (PKCS#8 PEM) in
credstore and registers the public key with the mock under a fresh `kid`.
The mock `/oauth2/token-jwt` endpoint checks the assertion (form fields,
JWS header, signature, iss/sub/aud/exp/iat/jti) and issues
`mock-e2e-token-jwt-<kid>`, so a test sees which key signed; it counts the
tokens issued per kid.
"""
import uuid

import httpx
import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec, rsa

from .helpers import (
    OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID,
    UPSTREAM_SCHEMA,
    assert_problem,
    assert_problem_body,
    create_route,
    create_upstream,
    create_upstream_raw,
    err_type,
    traced,
    unique_alias,
)

_SCENARIO = "positive-9.15-oauth2-client-credentials-private-key-jwt"


def _key_pair(alg: str) -> tuple[str, str]:
    """A fresh key for `alg`: (PKCS#8 PEM private key, SPKI PEM public key)."""
    if alg == "ES256":
        key = ec.generate_private_key(ec.SECP256R1())
    else:
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    private = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ).decode()
    public = key.public_key().public_bytes(
        serialization.Encoding.PEM,
        serialization.PublicFormat.SubjectPublicKeyInfo,
    ).decode()
    return private, public


def _key_lines(pem: str) -> list[str]:
    return [line for line in pem.splitlines() if line and not line.startswith("-----")]


def _auth(mock_upstream_url: str, key_ref: str, alg: str, kid: str | None, **extra) -> dict:
    config = {
        "token_endpoint": f"{mock_upstream_url}/oauth2/token-jwt",
        "client_id_ref": "cred://test-oauth2-client-id",
        "private_key_ref": f"cred://{key_ref}",
        "signing_alg": alg,
        # A per-test scope makes the token cache key unique.
        "scopes": f"read e2e-{uuid.uuid4().hex[:8]}",
        **extra,
    }
    if kid is not None:
        config["kid"] = kid
    return {
        "type": OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID,
        "sharing": "private",
        "config": config,
    }


class _Secrets:
    """Credstore secrets a test creates, deleted at the end."""

    def __init__(self, client: httpx.AsyncClient, base_url: str, headers: dict):
        self._client, self._url, self._headers = client, f"{base_url}/credstore/v1/secrets", headers
        self._refs: list[str] = []

    async def create(self, value: str) -> str:
        ref = f"e2e-pkjwt-{uuid.uuid4().hex[:12]}"
        resp = await self._client.post(
            self._url,
            headers={**self._headers, "content-type": "application/json"},
            json={"reference": ref, "value": value, "sharing": "tenant"},
        )
        # Report only the reference and status, never the body.
        assert resp.status_code in (200, 201, 204), (
            f"could not create secret {ref!r}: HTTP {resp.status_code}"
        )
        self._refs.append(ref)
        return ref

    async def delete_all(self) -> None:
        for ref in self._refs:
            await self._client.delete(
                f"{self._url}/{ref}", headers={**self._headers, "If-Match": "*"},
            )


async def _register_public_key(client, mock_upstream_url, kid, alg, public_pem, aud=None):
    """Register the public key; with `aud`, the mock requires that audience
    instead of its token endpoint URL."""
    key = {"kid": kid, "alg": alg, "public_key_pem": public_pem}
    if aud is not None:
        key["aud"] = aud
    resp = await client.post(f"{mock_upstream_url}/oauth2/jwt-keys", json=key)
    assert resp.status_code == 200, resp.text


async def _tokens_issued(client, mock_upstream_url, kid) -> int:
    resp = await client.get(f"{mock_upstream_url}/counters/token-jwt:{kid}")
    assert resp.status_code == 200, resp.text
    return resp.json()["count"]


async def _proxy_echo(client, oagw_base_url, headers, alias):
    return await client.post(
        f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
        headers={**headers, "content-type": "application/json"},
        json={"test": True},
    )


@pytest.mark.scenario(_SCENARIO, part="A")
@pytest.mark.asyncio
@pytest.mark.parametrize("alg", ["ES256", "RS256", "PS256"])
async def test_private_key_jwt_injects_bearer(
    alg, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """The token request carries an assertion the mock verifies with the
    public key; the upstream gets the token; a second call reuses it: one
    token request for two proxied calls."""
    alias = unique_alias("pkjwt")
    kid = f"e2e-{alg.lower()}-{uuid.uuid4().hex[:8]}"
    private_pem, public_pem = _key_pair(alg)
    async with httpx.AsyncClient(timeout=10.0) as client:
        secrets = _Secrets(client, oagw_base_url, oagw_headers)
        try:
            key_ref = await secrets.create(private_pem)
            await _register_public_key(client, mock_upstream_url, kid, alg, public_pem)
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias, auth=_auth(mock_upstream_url, key_ref, alg, kid),
            ))
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
            )

            for _ in range(2):
                resp = await _proxy_echo(client, oagw_base_url, oagw_headers, alias)
                assert resp.status_code == 200, (
                    f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
                )
                assert resp.headers.get("x-oagw-error-source") == "upstream"
                assert resp.json()["headers"].get("authorization") == (
                    f"Bearer mock-e2e-token-jwt-{kid}"
                )
            assert await _tokens_issued(client, mock_upstream_url, kid) == 1
        finally:
            await secrets.delete_all()


@pytest.mark.scenario(_SCENARIO, part="D")
@pytest.mark.asyncio
async def test_private_key_jwt_configured_audience(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """With `audience` set, the assertion's `aud` is that value, not the
    token endpoint; the mock refuses any other `aud` for this key."""
    alias = unique_alias("pkjwt-aud")
    kid = f"e2e-aud-{uuid.uuid4().hex[:8]}"
    audience = f"https://idp.example.com/{uuid.uuid4().hex[:8]}"
    private_pem, public_pem = _key_pair("ES256")
    async with httpx.AsyncClient(timeout=10.0) as client:
        secrets = _Secrets(client, oagw_base_url, oagw_headers)
        try:
            key_ref = await secrets.create(private_pem)
            await _register_public_key(
                client, mock_upstream_url, kid, "ES256", public_pem, aud=audience,
            )
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias,
                auth=_auth(mock_upstream_url, key_ref, "ES256", kid, audience=audience),
            ))
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
            )

            resp = await _proxy_echo(client, oagw_base_url, oagw_headers, alias)
            assert resp.status_code == 200, (
                f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
            )
            assert resp.json()["headers"].get("authorization") == (
                f"Bearer mock-e2e-token-jwt-{kid}"
            )
            assert await _tokens_issued(client, mock_upstream_url, kid) == 1
        finally:
            await secrets.delete_all()


@pytest.mark.scenario(_SCENARIO, part="B")
@pytest.mark.asyncio
async def test_private_key_jwt_key_of_wrong_type_is_500(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """An RSA key under `signing_alg` ES256: 500 `internal`, nothing sent
    upstream, and the response shows no key material."""
    alias = unique_alias("pkjwt-mismatch")
    private_pem, _ = _key_pair("RS256")
    async with httpx.AsyncClient(timeout=10.0) as client:
        secrets = _Secrets(client, oagw_base_url, oagw_headers)
        try:
            key_ref = await secrets.create(private_pem)
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias, auth=_auth(mock_upstream_url, key_ref, "ES256", "unused"),
            ))
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
            )

            resp = await _proxy_echo(client, oagw_base_url, traced(oagw_headers), alias)
            assert_problem(resp, 500, category="internal")
            for line in _key_lines(private_pem):
                assert line not in resp.text
            assert key_ref not in resp.text
        finally:
            await secrets.delete_all()


@pytest.mark.scenario(_SCENARIO, part="C")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("edit", "field", "reason"),
    [
        pytest.param(
            {"client_secret_ref": "cred://test-oauth2-client-secret"},
            "auth.config.client_secret_ref", "VALIDATION", id="both-refs",
        ),
        pytest.param({"signing_alg": "HS256"}, "auth.config.signing_alg", "INVALID_VALUE",
                     id="unknown-alg"),
        pytest.param({"private_key_ref": None}, "auth.config.private_key_ref", "REQUIRED",
                     id="no-key-ref"),
        pytest.param({"kid": ""}, "auth.config.kid", "INVALID_VALUE", id="empty-kid"),
        pytest.param({"audience": ""}, "auth.config.audience", "INVALID_VALUE",
                     id="empty-audience"),
        pytest.param({"token_endpoint": None}, "auth.config.token_endpoint", "REQUIRED",
                     id="no-endpoint"),
        pytest.param({"issuer_url": "https://idp.example.com"}, "auth.config.issuer_url",
                     "VALIDATION", id="both-endpoints"),
    ],
)
async def test_private_key_jwt_invalid_config_rejected_at_write(
    edit, field, reason, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A config the plugin cannot run is a 400 field violation on create,
    not a failure of every proxied request."""
    auth = _auth(mock_upstream_url, "test-oauth2-client-secret", "ES256", None)
    for key, value in edit.items():
        if value is None:
            auth["config"].pop(key)
        else:
            auth["config"][key] = value
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=unique_alias("pkjwt-bad"), auth=auth,
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        body = assert_problem(resp, 400, esrc=None, category="invalid_argument")
        violations = body["context"]["field_violations"]
        assert [(v["field"], v["reason"]) for v in violations] == [(field, reason)], body


@pytest.mark.scenario("negative-9.7-secret-access-control-cred-store", part="B")
@pytest.mark.asyncio
async def test_private_key_jwt_missing_key_rejected_at_write(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 9.7-B for the key reference: a key the store does not hold
    is refused on create, naming the key, not quoting the reference."""
    auth = _auth(mock_upstream_url, "nonexistent-signing-key", "ES256", None)
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=unique_alias("pkjwt-nokey"), auth=auth,
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem_body(resp, {
            "type": err_type("failed_precondition"),
            "title": "Failed Precondition",
            "status": 400,
            "detail": "Operation precondition not met",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "violations": [{
                    "type": "STATE",
                    "subject": "auth.config.private_key_ref",
                    "description": (
                        "credential reference is not accessible to this tenant "
                        "(not provisioned yet, or not shared)"
                    ),
                }],
                "resource_type": UPSTREAM_SCHEMA,
            },
        })
