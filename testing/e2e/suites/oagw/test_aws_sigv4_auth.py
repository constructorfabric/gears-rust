"""E2E tests for the AWS SigV4 auth plugin (#5842).

The mock's `/aws/*` endpoints verify the signature independently, with
stdlib hmac/hashlib, from the request as received: they answer 403
`SignatureDoesNotMatch` when it does not verify, else echo the request and
what was verified. The mock listens on a non-default port, so the signed
`Host` carries it.
"""
import re

import httpx
import pytest

from .helpers import (
    AWS_SIGV4_AUTH_PLUGIN_ID,
    PROXY_SCHEMA,
    assert_problem,
    assert_problem_body,
    create_route,
    create_upstream,
    create_upstream_raw,
    err_type,
    traced,
    unique_alias,
)

_SIGNED = "positive-9.11-aws-sigv4-signed-request"
_CLIENT_HEADERS = "positive-9.13-aws-sigv4-client-amz-headers"
_S3_PATH = "negative-9.14-aws-sigv4-s3-rewritten-path"

_TOKEN = "FwoGZXIvYXdzEXAMPLEsessionTOKEN+/="


def _aws_auth(**config) -> dict:
    base = {
        "region": "us-east-1",
        "service": "bedrock",
        "access_key_id_ref": "cred://test-aws-access-key-id",
        "secret_access_key_ref": "cred://test-aws-secret-access-key",
    }
    base.update(config)
    return {
        "type": AWS_SIGV4_AUTH_PLUGIN_ID,
        "sharing": "private",
        "config": {k: v for k, v in base.items() if v is not None},
    }


async def _upstream(client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
                    prefix, auth, upstream_headers=None):
    """An upstream on the mock signing with `auth`, with routes GET /aws/get
    (query `q`, `e`) and POST /aws/post. Returns its alias."""
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(oagw_headers, await create_upstream(
        client, oagw_base_url, oagw_headers, mock_upstream_url,
        alias=alias, auth=auth, upstream_headers=upstream_headers,
    ))
    await create_route(
        client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/aws/get",
        match={"http": {"methods": ["GET"], "path": "/aws/get", "query_allowlist": ["q", "e"]}},
    )
    await create_route(
        client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/aws/post",
    )
    return alias


def _verified(resp: httpx.Response) -> dict:
    assert resp.status_code == 200, f"expected 200, got {resp.status_code}: {resp.text[:500]}"
    assert resp.headers.get("x-oagw-error-source") == "upstream"
    return resp.json()


@pytest.mark.scenario(_SIGNED, part="A")
@pytest.mark.asyncio
async def test_get_with_query_is_signed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A GET with repeated query names, a space, a literal plus and an empty
    value is sent in SigV4 query encoding and verifies upstream."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-get", _aws_auth(),
        )
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/get?q=a+b&q=c%2Bd&e=",
            headers=oagw_headers,
        )
        echo = _verified(resp)
        assert echo["query"] == "q=a%20b&q=c%2Bd&e="
        assert echo["verified"]["signed_headers"] == "host;x-amz-content-sha256;x-amz-date"
        assert echo["verified"]["service"] == "bedrock"
        assert re.fullmatch(r"127\.0\.0\.1:\d+", echo["headers"]["host"])
        assert not echo["headers"]["host"].endswith(":80")


@pytest.mark.scenario(_SIGNED, part="A")
@pytest.mark.asyncio
async def test_post_json_is_signed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A POST with a JSON body is signed with the body's SHA-256."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-post", _aws_auth(),
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/post",
            headers={**oagw_headers, "content-type": "application/json"},
            content=b'{"prompt":"hello","max_tokens":16}',
        )
        echo = _verified(resp)
        assert echo["verified"]["signed_headers"] == (
            "content-type;host;x-amz-content-sha256;x-amz-date"
        )
        assert echo["verified"]["payload_hash"] != "UNSIGNED-PAYLOAD"
        assert echo["body"] == '{"prompt":"hello","max_tokens":16}'
        assert echo["headers"]["authorization"].startswith(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"
        )


@pytest.mark.scenario(_SIGNED, part="A")
@pytest.mark.asyncio
async def test_session_token_is_sent_and_signed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """With `session_token_ref` the token is sent as X-Amz-Security-Token and signed."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-sts", _aws_auth(session_token_ref="cred://test-aws-session-token"),
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/post",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"k": 1},
        )
        echo = _verified(resp)
        assert echo["verified"]["session_token"] is True
        assert "x-amz-security-token" in echo["verified"]["signed_headers"].split(";")
        assert echo["headers"]["x-amz-security-token"] == _TOKEN


@pytest.mark.scenario(_CLIENT_HEADERS, part="A")
@pytest.mark.asyncio
async def test_client_amz_headers_are_replaced(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Under `passthrough: all`, the client's x-amz-date, x-amz-content-sha256
    and x-amz-security-token reach the upstream once each, with the plugin's
    values; other x-amz-* headers are signed as sent, a repeated one with
    every line."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-client", _aws_auth(session_token_ref="cred://test-aws-session-token"),
            upstream_headers={"request": {"passthrough": "all"}},
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/post",
            headers=httpx.Headers([
                *oagw_headers.items(),
                ("content-type", "application/json"),
                ("x-amz-date", "19700101T000000Z"),
                ("x-amz-content-sha256", "0" * 64),
                ("x-amz-security-token", "client-token"),
                ("x-amz-target", "BedrockRuntime.InvokeModel"),
                ("x-amz-meta-tag", "one"),
                ("x-amz-meta-tag", "two"),
            ]),
            json={"k": 1},
        )
        echo = _verified(resp)
        assert [v for k, v in echo["header_lines"] if k == "x-amz-meta-tag"] == ["one", "two"]
        assert "x-amz-meta-tag" in echo["verified"]["signed_headers"].split(";")
        lines = echo["header_lines"]
        for name in ("x-amz-date", "x-amz-content-sha256", "x-amz-security-token", "authorization"):
            assert len([v for k, v in lines if k == name]) == 1, (name, lines)
        assert echo["headers"]["x-amz-date"] != "19700101T000000Z"
        assert echo["headers"]["x-amz-content-sha256"] != "0" * 64
        assert echo["headers"]["x-amz-security-token"] == _TOKEN
        assert echo["headers"]["x-amz-target"] == "BedrockRuntime.InvokeModel"
        assert "x-amz-target" in echo["verified"]["signed_headers"].split(";")


@pytest.mark.scenario(_CLIENT_HEADERS, part="B")
@pytest.mark.asyncio
async def test_client_security_token_without_session_token_is_refused(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Without `session_token_ref` the plugin cannot remove a client's
    X-Amz-Security-Token: 400 before anything is sent; a `remove` header rule
    lets the request through."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-notoken", _aws_auth(),
            upstream_headers={"request": {"passthrough": "all"}},
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/post",
            headers={
                **traced(oagw_headers),
                "content-type": "application/json",
                "x-amz-security-token": "client-token",
            },
            json={"k": 1},
        )
        assert_problem(resp, 400, category="invalid_argument")
        body = resp.json()
        trace_id = body.pop("trace_id", None)
        assert isinstance(trace_id, str) and re.fullmatch(r"[0-9a-f]{32}", trace_id), resp.text
        assert body == {
            "type": err_type("invalid_argument"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": f"/oagw/v1/proxy/{alias}/aws/post",
            "context": {
                "field_violations": [{
                    "field": "x-amz-security-token",
                    "reason": "VALIDATION",
                    "description": (
                        "the request carries x-amz-security-token but "
                        "auth.config.session_token_ref is not set; the AWS SigV4 plugin "
                        "cannot remove the header, so drop it with headers.request.remove"
                    ),
                }],
                "resource_type": PROXY_SCHEMA,
            },
        }, resp.text

        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-strip", _aws_auth(),
            upstream_headers={
                "request": {"passthrough": "all", "remove": ["x-amz-security-token"]},
            },
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/post",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "x-amz-security-token": "client-token",
            },
            json={"k": 1},
        )
        echo = _verified(resp)
        assert "x-amz-security-token" not in echo["headers"]


@pytest.mark.scenario(_S3_PATH, part="A")
@pytest.mark.asyncio
async def test_s3_path_the_gateway_rewrote_is_refused(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """With `service: s3` a path the gateway normalized (`//`, a trailing
    `/`) would name another key: 400 on `path`, nothing sent. A path the
    gateway kept is sent as received and signed as S3 checks it: decoded
    and encoded again, so `:` or `(` sent unencoded are signed encoded."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-s3", _aws_auth(service="s3"),
        )
        for suffix in ("a//b", "folder/"):
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/get/{suffix}",
                headers=traced(oagw_headers),
            )
            body = assert_problem(resp, 400, category="invalid_argument")
            violations = body["context"]["field_violations"]
            assert [(v["field"], v["reason"]) for v in violations] == [("path", "VALIDATION")], (
                resp.text
            )
            assert "another object" in violations[0]["description"], resp.text

        for key in ("a/b", "a:b", "photo(1).jpg", "x!y*z", "k=v@w,u"):
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/get/{key}",
                headers=oagw_headers,
            )
            echo = _verified(resp)
            assert echo["path"] == f"/aws/get/{key}", key
            assert echo["verified"]["service"] == "s3"


@pytest.mark.scenario(_SIGNED, part="B")
@pytest.mark.asyncio
async def test_wrong_secret_is_rejected_by_the_upstream(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Signed with the wrong secret, the request reaches the upstream and is
    rejected there: its 403 is passed through."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "sigv4-wrong",
            _aws_auth(secret_access_key_ref="cred://test-aws-wrong-secret-access-key"),
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/aws/post",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"k": 1},
        )
        assert resp.status_code == 403, resp.text[:500]
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        assert resp.json() == {
            "error": "SignatureDoesNotMatch", "message": "signature does not match",
        }


@pytest.mark.scenario(_SIGNED, part="C")
@pytest.mark.asyncio
async def test_missing_region_is_rejected_at_write(
    oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
):
    """An upstream whose auth config has no `region` is a 400 naming the key."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=unique_alias("sigv4-noregion"), auth=_aws_auth(region=None),
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem_body(resp, {
            "type": err_type("invalid_argument"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "field_violations": [{
                    "field": "auth.config.region",
                    "reason": "REQUIRED",
                    "description": "auth.config.region: required by the AWS SigV4 auth plugin",
                }],
                "resource_type": PROXY_SCHEMA,
            },
        })
