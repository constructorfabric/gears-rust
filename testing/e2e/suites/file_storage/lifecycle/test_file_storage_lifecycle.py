"""LocalFs lifecycle E2E against a private server + sidecar.

Each test owns its data (a fresh file uploaded by ``upload_file``) and covers
one cross-process seam. The sidecar writes blobs under ``fs_storage_root``,
shared with the control plane, so tests read them straight from disk.
"""

from pathlib import Path

import pytest

from ..fs_flow import API_BASE, download_bytes, download_url, upload_file

# Distinctive payload: catches any mix-up or truncation.
PAYLOAD = b"Hello from the file-storage E2E lifecycle test! \xde\xad\xbe\xef"


@pytest.mark.timeout(60)
def test_sidecar_put_finalizes_and_auto_binds(fs_client, fs_storage_root, gts_file_type):
    """Seam: sidecar PUT -> control-plane finalize callback with auto-bind.

    The sidecar's own response must forward the finalize outcome
    (``X-FS-Bound: bound`` + ``ETag``), the bytes must land at
    ``<storage_root>/<file_id>/<version_id>``, and the control plane must have
    moved the content pointer to the uploaded version.
    """
    up = upload_file(fs_client, gts_file_type, PAYLOAD)

    assert up.upload.headers.get("x-fs-bound") == "bound", dict(up.upload.headers)
    assert up.upload.headers.get("etag"), dict(up.upload.headers)

    blob = Path(fs_storage_root) / up.file_id / up.version_id
    assert blob.exists(), f"blob absent on disk at {blob}"
    assert blob.read_bytes() == PAYLOAD

    r = fs_client.get(f"{API_BASE}/files/{up.file_id}")
    assert r.status_code == 200, r.text
    assert r.json().get("content_id") == up.version_id


@pytest.mark.timeout(60)
def test_signed_download_url_serves_uploaded_bytes(fs_client, gts_file_type):
    """Seam: control-plane signed download URL -> sidecar GET returns the bytes."""
    up = upload_file(fs_client, gts_file_type, PAYLOAD)

    url = download_url(fs_client, up.file_id)
    assert "/api/file-storage-data/" in url, f"download_url must route through the sidecar: {url!r}"
    assert download_bytes(url) == PAYLOAD


@pytest.mark.timeout(60)
def test_delete_removes_blob_and_file(fs_client, fs_storage_root, gts_file_type):
    """Seam: control-plane DELETE -> blob removed from the backend + 404 afterwards."""
    up = upload_file(fs_client, gts_file_type, PAYLOAD)
    blob = Path(fs_storage_root) / up.file_id / up.version_id
    assert blob.exists(), f"precondition: blob absent on disk at {blob}"

    r = fs_client.delete(
        f"{API_BASE}/files/{up.file_id}",
        headers={"if-match": up.upload.headers["etag"]},
    )
    assert r.status_code == 204, f"{r.status_code}\n{r.text}"

    assert not blob.exists(), f"blob still on disk after DELETE: {blob}"
    assert fs_client.get(f"{API_BASE}/files/{up.file_id}").status_code == 404
