"""LocalFs single-part full lifecycle E2E.

Exercises the real LocalFsBackend end-to-end with a live server + sidecar:

    create → upload bytes (sidecar finalizes) → verify on-disk temp file
           → bind → download-url → GET bytes → assert bytes
           → delete → verify on-disk file gone → assert control-plane 404

The sidecar writes blobs to a session-scoped ``tempfile.mkdtemp`` root shared
with the control plane (same ``storage_root``).  The test reads those files
directly from disk between the HTTP steps to prove the bytes really landed.

Architecture note — the real download path:
    After the sidecar PUTs bytes and calls back the control-plane finalize
    endpoint, the version is ``available``.  The client then:
    1. ``POST /files/{id}/bind`` — swaps the content pointer.
    2. ``GET /files/{id}/download-url`` — issues a signed sidecar download URL.
    3. ``GET`` the returned URL directly against the sidecar.
    This is the exact path a production deployment follows.
"""

import uuid
from pathlib import Path

import httpx
import pytest

REQUEST_TIMEOUT = 10.0
API_BASE = "/api/file-storage/v1"

# A known byte payload — distinctive enough to catch any mix-up or truncation.
PAYLOAD = b"Hello from the file-storage E2E lifecycle test! \xde\xad\xbe\xef"


@pytest.mark.timeout(60)
def test_localfs_single_part_full_lifecycle(
    lifecycle_base_url: str,
    lifecycle_sidecar,
    lifecycle_auth_headers: dict,
    fs_storage_root: str,
    gts_file_type: str,
):
    """Single-part LocalFs lifecycle: create → upload → disk verify → bind → download → delete.

    Seam coverage:
    * Route wiring (POST /files, POST /bind, GET /download-url, DELETE /files/{id})
    * Signed-URL issuance (control plane → sidecar)
    * Byte transport (PUT to sidecar)
    * Sidecar finalize callback (POST /versions/{id}/finalize on control plane)
    * ON-DISK VERIFICATION: bytes at <storage_root>/<file_id>/<version_id>
    * Control-plane download-url path (GET → signed sidecar URL)
    * Control-plane delete → best-effort blob removal from disk
    * Control-plane 404 after delete
    """
    client = httpx.Client(
        base_url=lifecycle_base_url,
        headers=lifecycle_auth_headers,
        timeout=REQUEST_TIMEOUT,
        follow_redirects=False,
    )

    # ── 1. Create a file and get the signed upload URL ────────────────────
    owner_id = str(uuid.uuid4())
    create_body = {
        "owner_kind": "user",
        "owner_id": owner_id,
        "name": f"lifecycle-test-{uuid.uuid4()}.bin",
        "gts_file_type": gts_file_type,
        "mime_type": "application/octet-stream",
    }
    r = client.post(f"{API_BASE}/files", json=create_body)
    assert r.status_code == 201, (
        f"POST /files failed: {r.status_code}\n{r.text}"
    )
    ticket = r.json()
    assert "file_id" in ticket, f"missing file_id in: {ticket!r}"
    assert "version_id" in ticket, f"missing version_id in: {ticket!r}"
    assert "upload_url" in ticket, f"missing upload_url in: {ticket!r}"
    assert "/api/file-storage-data/" in ticket["upload_url"], (
        f"upload_url must route through the sidecar data-plane: {ticket['upload_url']!r}"
    )

    file_id: str = ticket["file_id"]
    version_id: str = ticket["version_id"]
    upload_url: str = ticket["upload_url"]

    # ── 2. Upload bytes to the sidecar via the signed URL ─────────────────
    # The sidecar verifies token, writes bytes, then POSTs the finalize
    # callback to the control plane — so after this step the version is
    # ``available`` and bind can proceed.
    upload_resp = httpx.put(upload_url, content=PAYLOAD, timeout=REQUEST_TIMEOUT)
    assert upload_resp.status_code == 200, (
        f"PUT {upload_url!r} failed: {upload_resp.status_code}\n{upload_resp.text}"
    )

    # ── 3. Verify on-disk: the blob must be at <storage_root>/<file_id>/<version_id>
    on_disk_path = Path(fs_storage_root) / file_id / version_id
    assert on_disk_path.exists(), (
        f"Expected blob on disk at {on_disk_path} but file is absent.\n"
        f"storage_root={fs_storage_root!r}"
    )
    disk_bytes = on_disk_path.read_bytes()
    assert disk_bytes == PAYLOAD, (
        f"On-disk content mismatch!\n"
        f"  expected: {PAYLOAD!r}\n"
        f"  got:      {disk_bytes!r}\n"
        f"  path:     {on_disk_path}"
    )

    # ── 4. Bind the version (first bind — no If-Match required) ──────────
    # The sidecar finalize callback has marked the version ``available``.
    bind_resp = client.post(
        f"{API_BASE}/files/{file_id}/bind",
        json={"version_id": version_id},
    )
    assert bind_resp.status_code == 200, (
        f"POST /files/{file_id}/bind failed: {bind_resp.status_code}\n{bind_resp.text}"
    )
    bound_file = bind_resp.json()
    assert bound_file.get("content_id") == version_id, (
        f"Expected content_id={version_id!r}, got: {bound_file.get('content_id')!r}"
    )

    # ── 5. Get a signed download URL from the control plane ───────────────
    dl_ticket_resp = client.get(f"{API_BASE}/files/{file_id}/download-url")
    assert dl_ticket_resp.status_code == 200, (
        f"GET /files/{file_id}/download-url failed: "
        f"{dl_ticket_resp.status_code}\n{dl_ticket_resp.text}"
    )
    dl_ticket = dl_ticket_resp.json()
    assert "download_url" in dl_ticket, f"missing download_url in: {dl_ticket!r}"
    download_url: str = dl_ticket["download_url"]
    assert "/api/file-storage-data/" in download_url, (
        f"download_url must route through the sidecar: {download_url!r}"
    )

    # ── 6. Download bytes via the signed URL ──────────────────────────────
    dl_resp = httpx.get(download_url, timeout=REQUEST_TIMEOUT)
    assert dl_resp.status_code == 200, (
        f"GET {download_url!r} failed: {dl_resp.status_code}\n{dl_resp.text}"
    )
    assert dl_resp.content == PAYLOAD, (
        f"Downloaded content mismatch!\n"
        f"  expected: {PAYLOAD!r}\n"
        f"  got:      {dl_resp.content!r}"
    )

    # ── 7. Delete the file (If-Match: current content ETag) ───────────────
    current_etag = bound_file.get("etag") or "*"
    del_resp = client.delete(
        f"{API_BASE}/files/{file_id}",
        headers={"if-match": current_etag},
    )
    assert del_resp.status_code == 204, (
        f"DELETE /files/{file_id} failed: {del_resp.status_code}\n{del_resp.text}"
    )

    # ── 8. Verify on-disk: the blob must be gone after delete ─────────────
    assert not on_disk_path.exists(), (
        f"Blob should be gone after DELETE but still exists at {on_disk_path}"
    )

    # ── 9. Control-plane 404 after delete ─────────────────────────────────
    get_resp = client.get(f"{API_BASE}/files/{file_id}")
    assert get_resp.status_code == 404, (
        f"Expected 404 after delete, got {get_resp.status_code}\n{get_resp.text}"
    )
    # Should be RFC 9457 problem+json.
    assert "application/problem+json" in get_resp.headers.get("content-type", ""), (
        f"Expected problem+json content-type, got: {get_resp.headers.get('content-type')!r}"
    )
    problem = get_resp.json()
    assert problem.get("status") == 404


# ── Fixture available from parent conftest (re-exposed here for clarity) ──

@pytest.fixture
def gts_file_type():
    """A syntactically valid GTS file type accepted at upload time."""
    import os
    return os.getenv(
        "E2E_FS_GTS_TYPE",
        "gts.cf.fstorage.file.type.v1~x.e2e.test.v1~",
    )
