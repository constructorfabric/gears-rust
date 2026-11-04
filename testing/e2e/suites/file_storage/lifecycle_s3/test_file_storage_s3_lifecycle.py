"""S3-backend lifecycle E2E (optional, see ``conftest.py``).

Only the S3-specific seams live here; the generic upload / bind / download /
delete behaviour is covered by the local-fs suite (``../lifecycle``). The
control plane's default backend is S3, so every upload URL minted by
``POST /files`` carries the S3 ``claims.backend_id``.
"""

import httpx
import pytest

from ..fs_flow import (
    API_BASE,
    REQUEST_TIMEOUT,
    create_file,
    download_bytes,
    download_url,
    upload_file,
)

SINGLE_PART_PAYLOAD = b"Hello from the file-storage S3 E2E lifecycle test! \xde\xad\xbe\xef"

# 5 MiB is the server's minimum part size regardless of backend, so a 6 MiB
# declared size forces a >= 2 part plan. Distinct per-part bytes expose a
# mis-ordered assembly.
_PART_SIZE = 5 * 1024 * 1024
_LAST_PART_SIZE = 1 * 1024 * 1024
MULTIPART_PAYLOAD = (b"A" * _PART_SIZE) + (b"B" * _LAST_PART_SIZE)


@pytest.mark.timeout(60)
def test_s3_single_part_object_roundtrip(fs_client, gts_file_type):
    """Seam: sidecar dispatch by ``claims.backend_id`` -> S3 PutObject / GetObject.

    The object written through the sidecar is read back byte-for-byte from S3
    via a signed download URL.
    """
    up = upload_file(fs_client, gts_file_type, SINGLE_PART_PAYLOAD)

    assert download_bytes(download_url(fs_client, up.file_id)) == SINGLE_PART_PAYLOAD


@pytest.mark.timeout(90)
def test_s3_multipart_upload_and_complete(fs_client, gts_file_type):
    """Seam: multipart through ``POST /files`` -> S3 UploadPart -> ``complete``.

    Parts are PUT to the sidecar (which reports each back to the control
    plane); ``complete`` assembles them in S3 and auto-binds. The downloaded
    object must equal the concatenation of the parts.
    """
    ticket = create_file(
        fs_client, gts_file_type, multipart={"declared_size": len(MULTIPART_PAYLOAD)}
    )
    file_id = ticket["file_id"]
    plan = ticket.get("multipart")
    assert plan is not None, f"expected a multipart parts plan in: {ticket!r}"
    assert "upload_url" not in ticket, f"multipart ticket must not carry upload_url: {ticket!r}"
    parts = plan["parts"]
    assert len(parts) >= 2, f"expected >= 2 parts, got {len(parts)}: {plan!r}"

    for part in parts:
        body = MULTIPART_PAYLOAD[part["offset"] : part["offset"] + part["size"]]
        assert len(body) == part["size"], f"part {part['part_number']}: payload too short"
        resp = httpx.put(part["upload_url"], content=body, timeout=REQUEST_TIMEOUT)
        assert resp.status_code == 200, f"PUT part {part['part_number']}: {resp.status_code}\n{resp.text}"

    complete = fs_client.post(f"{API_BASE}/files/{file_id}/multipart/{plan['upload_id']}/complete")
    assert complete.status_code == 200, f"{complete.status_code}\n{complete.text}"
    body = complete.json()
    assert body.get("bind_state") == "bound", body
    assert body.get("etag"), body
    bound = fs_client.get(f"{API_BASE}/files/{file_id}")
    assert bound.status_code == 200, bound.text
    assert bound.json().get("content_id") == plan["version_id"]

    assert download_bytes(download_url(fs_client, file_id)) == MULTIPART_PAYLOAD
