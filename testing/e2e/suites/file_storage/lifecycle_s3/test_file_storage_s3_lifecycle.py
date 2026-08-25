"""S3-backend full lifecycle E2E (P2 1.7 Stage 6 — optional).

Exercises the real `S3Backend` (ADR-0005) end-to-end through a live server +
sidecar pair, proving Stage 5's per-request `claims.backend_id` dispatch
actually routes bytes to a real (test-double) S3 backend — not just the
in-process `s3s-fs` unit/integration coverage in
`gears/file-storage/file-storage/src/infra/backend/s3_tests.rs`, which calls
`S3Backend` directly and never exercises the sidecar's HTTP layer or
token-driven dispatch.

Skips at collection time (`pytest.skip(..., allow_module_level=True)`, see
`conftest.py`'s `_require_e2e_s3_endpoint`) unless `FS_E2E_S3_ENDPOINT` is
set — this suite requires a real S3-compatible HTTP endpoint (no in-process
double is stood up here, unlike the Rust unit tests). See `conftest.py`'s
module docstring for the exact local setup recipe ("Running the S3 e2e
locally") and env var names (`FS_E2E_S3_ENDPOINT`, `FS_E2E_S3_BUCKET`,
`FS_E2E_S3_ACCESS_KEY`, `FS_E2E_S3_SECRET_KEY`, `FS_E2E_S3_REGION`).

Two scenarios, mirroring `lifecycle/test_file_storage_lifecycle.py`'s
single-part flow (adapted for S3) plus a new multipart flow:

1. `test_s3_single_part_full_lifecycle` — create -> presign -> PUT
   (client -> sidecar -> S3) -> finalize (sidecar s2s callback) -> bind ->
   download-url -> GET bytes -> assert round-trip.
2. `test_s3_multipart_full_lifecycle` -- initiate -> PUT >= 2 parts
   (client -> sidecar -> S3, sidecar reports each part back to the control
   plane) -> complete -> bind -> download -> assert the assembled bytes are
   byte-for-byte correct.

Both flows create the file via `POST /files` (as normal), but since this
suite's server config makes the S3 backend the registry's *default* (see
`conftest.py`'s `_patch_file_storage_s3_config` / `default_backend_id`), the
signed upload URL minted for every new version already names the S3 backend
— no special per-request backend selection is needed (see `conftest.py`'s
module docstring, "Backend selection finding", for why).
"""

import uuid

import httpx
import pytest

REQUEST_TIMEOUT = 30.0
API_BASE = "/api/file-storage/v1"

# A known, distinctive single-part payload.
SINGLE_PART_PAYLOAD = b"Hello from the file-storage S3 E2E lifecycle test! \xde\xad\xbe\xef"

# Multipart payload: DEFAULT_MIN_PART_SIZE (5 MiB, `domain/multipart.rs`) is
# the server's floor for a part size regardless of backend, so a >5 MiB
# declared size is the minimum needed to force >= 2 parts through the
# server-authoritative parts plan. Each part uses a distinct byte pattern so
# a mis-ordered assembly on the S3 side would be caught by the final
# byte-for-byte comparison.
_PART_SIZE = 5 * 1024 * 1024
_LAST_PART_SIZE = 1 * 1024 * 1024
MULTIPART_PAYLOAD = (b"A" * _PART_SIZE) + (b"B" * _LAST_PART_SIZE)


def _create_file(client: httpx.Client, gts_file_type: str) -> dict:
    owner_id = str(uuid.uuid4())
    create_body = {
        "owner_kind": "user",
        "owner_id": owner_id,
        "name": f"s3-lifecycle-test-{uuid.uuid4()}.bin",
        "gts_file_type": gts_file_type,
        "mime_type": "application/octet-stream",
    }
    r = client.post(f"{API_BASE}/files", json=create_body)
    assert r.status_code == 201, f"POST /files failed: {r.status_code}\n{r.text}"
    return r.json()


@pytest.mark.timeout(60)
def test_s3_single_part_full_lifecycle(
    lifecycle_s3_base_url: str,
    lifecycle_s3_auth_headers: dict,
    gts_file_type: str,
):
    """Single-part S3 lifecycle: create -> upload -> bind -> download.

    Seam coverage:
    * `default_backend_id` config wiring routes new uploads to the S3 backend
      (P2 1.7 Stage 6 / this plan's config.rs change).
    * Signed-URL issuance (control plane -> sidecar) naming the S3 backend id.
    * Byte transport PUT: client -> sidecar -> real S3 PutObject (Stage 1).
    * Sidecar finalize callback (POST .../finalize on the control plane).
    * Control-plane download-url path (GET -> signed sidecar URL -> S3 GetObject).
    * The downloaded bytes match the uploaded bytes exactly.
    """
    client = httpx.Client(
        base_url=lifecycle_s3_base_url,
        headers=lifecycle_s3_auth_headers,
        timeout=REQUEST_TIMEOUT,
        follow_redirects=False,
    )

    # ── 1. Create a file and get the signed upload URL ────────────────────
    ticket = _create_file(client, gts_file_type)
    assert "upload_url" in ticket, f"missing upload_url in: {ticket!r}"
    assert "/api/file-storage-data/" in ticket["upload_url"], (
        f"upload_url must route through the sidecar data-plane: {ticket['upload_url']!r}"
    )
    file_id: str = ticket["file_id"]
    version_id: str = ticket["version_id"]
    upload_url: str = ticket["upload_url"]

    # ── 2. Upload bytes to the sidecar via the signed URL ─────────────────
    # The sidecar dispatches to the S3Backend named by the token's
    # `claims.backend_id` (Stage 5), writes the object to real S3, then POSTs
    # the finalize callback to the control plane.
    upload_resp = httpx.put(upload_url, content=SINGLE_PART_PAYLOAD, timeout=REQUEST_TIMEOUT)
    assert upload_resp.status_code == 200, (
        f"PUT {upload_url!r} failed: {upload_resp.status_code}\n{upload_resp.text}"
    )

    # ── 3. Bind the version (first bind — no If-Match required) ──────────
    bind_resp = client.post(
        f"{API_BASE}/files/{file_id}/bind",
        json={"version_id": version_id},
    )
    assert bind_resp.status_code == 200, (
        f"POST /files/{file_id}/bind failed: {bind_resp.status_code}\n{bind_resp.text}"
    )
    bound_file = bind_resp.json()
    assert bound_file.get("content_id") == version_id

    # ── 4. Get a signed download URL from the control plane ───────────────
    dl_ticket_resp = client.get(f"{API_BASE}/files/{file_id}/download-url")
    assert dl_ticket_resp.status_code == 200, (
        f"GET /files/{file_id}/download-url failed: "
        f"{dl_ticket_resp.status_code}\n{dl_ticket_resp.text}"
    )
    download_url = dl_ticket_resp.json()["download_url"]
    assert "/api/file-storage-data/" in download_url

    # ── 5. Download bytes via the signed URL and assert the S3 round-trip ─
    dl_resp = httpx.get(download_url, timeout=REQUEST_TIMEOUT)
    assert dl_resp.status_code == 200, (
        f"GET {download_url!r} failed: {dl_resp.status_code}\n{dl_resp.text}"
    )
    assert dl_resp.content == SINGLE_PART_PAYLOAD, (
        "Downloaded content mismatch — bytes did not round-trip via S3!\n"
        f"  expected: {SINGLE_PART_PAYLOAD!r}\n"
        f"  got:      {dl_resp.content!r}"
    )


@pytest.mark.timeout(90)
def test_s3_multipart_full_lifecycle(
    lifecycle_s3_base_url: str,
    lifecycle_s3_auth_headers: dict,
    gts_file_type: str,
):
    """Multipart S3 lifecycle: create -> initiate -> PUT parts -> complete ->
    bind -> download.

    Seam coverage:
    * `POST /files/{id}/multipart` (server-authoritative parts plan) picks
      the S3 backend (again via `default_backend_id`, since there is no
      per-request backend selector on this endpoint either — see
      `conftest.py`'s docstring).
    * Each part PUT: client -> sidecar -> real S3 UploadPart (Stage 2),
      followed by the sidecar's automatic `.../parts/{n}/report` s2s callback.
    * `POST /files/{id}/multipart/{upload_id}/complete` -> S3
      CompleteMultipartUpload, assembling the parts server-side.
    * The final downloaded object is byte-for-byte identical to the
      concatenation of the uploaded parts (not just correct length —
      distinct per-part byte patterns catch a mis-ordered assembly).
    """
    client = httpx.Client(
        base_url=lifecycle_s3_base_url,
        headers=lifecycle_s3_auth_headers,
        timeout=REQUEST_TIMEOUT,
        follow_redirects=False,
    )

    # ── 1. Create the file (establishes file_id; its own v1 upload ticket
    #        is intentionally unused — the multipart flow mints its own
    #        version) ──────────────────────────────────────────────────────
    ticket = _create_file(client, gts_file_type)
    file_id: str = ticket["file_id"]

    # ── 2. Initiate a multipart upload and get the server-authoritative
    #        parts plan ────────────────────────────────────────────────────
    initiate_resp = client.post(
        f"{API_BASE}/files/{file_id}/multipart",
        json={
            "declared_mime": "application/octet-stream",
            "declared_size": len(MULTIPART_PAYLOAD),
        },
    )
    assert initiate_resp.status_code == 200, (
        f"POST /files/{file_id}/multipart failed: "
        f"{initiate_resp.status_code}\n{initiate_resp.text}"
    )
    plan = initiate_resp.json()
    upload_id = plan["upload_id"]
    version_id = plan["version_id"]
    parts = plan["parts"]
    assert len(parts) >= 2, (
        f"expected >= 2 parts to exercise real multipart assembly, got {len(parts)}: {plan!r}"
    )

    # ── 3. PUT each part's exact byte range to its signed sidecar URL.
    #        The sidecar writes each part straight to S3 (Stage 2's
    #        UploadPart) and reports it back to the control plane itself
    #        (s2s callback) — no client action needed for that step. ───────
    for part in parts:
        offset, size, part_number = part["offset"], part["size"], part["part_number"]
        body = MULTIPART_PAYLOAD[offset : offset + size]
        assert len(body) == size, (
            f"part {part_number}: expected {size} bytes at offset {offset}, "
            f"payload only has {len(MULTIPART_PAYLOAD) - offset} remaining"
        )
        part_resp = httpx.put(part["upload_url"], content=body, timeout=REQUEST_TIMEOUT)
        assert part_resp.status_code == 200, (
            f"PUT part {part_number} ({part['upload_url']!r}) failed: "
            f"{part_resp.status_code}\n{part_resp.text}"
        )

    # ── 4. Complete the multipart upload (S3 CompleteMultipartUpload) ─────
    complete_resp = client.post(
        f"{API_BASE}/files/{file_id}/multipart/{upload_id}/complete"
    )
    # 200 (not the pre-item-3.3 204): the response now carries version_id,
    # size, and the ADR-0006 composite hash/manifest.
    assert complete_resp.status_code == 200, (
        f"POST .../multipart/{upload_id}/complete failed: "
        f"{complete_resp.status_code}\n{complete_resp.text}"
    )

    # ── 5. Bind the newly-completed version ────────────────────────────────
    bind_resp = client.post(
        f"{API_BASE}/files/{file_id}/bind",
        json={"version_id": version_id},
    )
    assert bind_resp.status_code == 200, (
        f"POST /files/{file_id}/bind failed: {bind_resp.status_code}\n{bind_resp.text}"
    )
    assert bind_resp.json().get("content_id") == version_id

    # ── 6. Download and assert the fully-assembled object is byte-exact ───
    dl_ticket_resp = client.get(f"{API_BASE}/files/{file_id}/download-url")
    assert dl_ticket_resp.status_code == 200, (
        f"GET /files/{file_id}/download-url failed: "
        f"{dl_ticket_resp.status_code}\n{dl_ticket_resp.text}"
    )
    download_url = dl_ticket_resp.json()["download_url"]

    dl_resp = httpx.get(download_url, timeout=REQUEST_TIMEOUT)
    assert dl_resp.status_code == 200, (
        f"GET {download_url!r} failed: {dl_resp.status_code}\n{dl_resp.text}"
    )
    assert dl_resp.content == MULTIPART_PAYLOAD, (
        "Downloaded multipart-assembled content mismatch — the parts did not "
        "round-trip via S3 correctly (wrong bytes, wrong order, or truncated)."
    )
