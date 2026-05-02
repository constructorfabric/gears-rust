# File Parser - Quickstart

Converts various document formats into a unified structured representation. Extracts text, formatting, and metadata from files and returns them as document blocks with inline elements.

**Supported formats:**
- Documents: PDF, DOCX, plain text, Markdown, HTML
- Images: PNG, JPG, JPEG, WebP, GIF (OCR-capable)
- Legacy formats: DOC, RTF, ODT, XLS, XLSX, PPT, PPTX (basic support)

**Input methods:**
- Upload files directly
- Parse from local file paths (restricted to `allowed_local_base_dir`; paths with `..` are always rejected)

Full API documentation: <http://127.0.0.1:8087/docs>

## Configuration

`allowed_local_base_dir` is **required**. The module will refuse to start if it is missing or the path cannot be resolved.

```yaml
modules:
  file-parser:
    config:
      max_file_size_mb: 100
      allowed_local_base_dir: /data/documents
```

Only files under this directory (after symlink resolution) are accessible via the `parse-local` endpoints. Paths containing `..` are always rejected regardless of where they point.

## Examples

### List Supported File Types

```bash
curl -s http://127.0.0.1:8087/file-parser/v1/info | python3 -m json.tool
```

**Output:**
```json
{
    "supported_extensions": {
        "plain_text": ["txt", "log", "md"],
        "html": ["html", "htm"],
        "pdf": ["pdf"],
        "docx": ["docx"],
        "image": ["png", "jpg", "jpeg", "webp", "gif"],
        "generic_stub": ["doc", "rtf", "odt", "xls", "xlsx", "ppt", "pptx"]
    }
}
```

### Upload and Parse a File

```bash
echo "Hello, CyberFabric!" > /tmp/test.txt
curl -s -X POST "http://127.0.0.1:8087/file-parser/v1/upload?filename=test.txt" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @/tmp/test.txt | python3 -m json.tool
```

**Output:**
```json
{
    "document": {
        "id": "019bc231-fcfd-7df3-a49c-82174973ec44",
        "title": "test.txt",
        "meta": {
            "source": {"type": "uploaded", "original_name": "test.txt"},
            "content_type": "text/plain"
        },
        "blocks": [
            {
                "type": "paragraph",
                "inlines": [{"type": "text", "text": "Hello, CyberFabric!", "style": {}}]
            }
        ]
    }
}
```

### Parse a Local File

Assumes `allowed_local_base_dir` is set to `/data/documents` and the file exists there.

```bash
curl -s -X POST "http://127.0.0.1:8087/file-parser/v1/parse-local?render_markdown=true" \
  -H "Content-Type: application/json" \
  -d '{"file_path": "/data/documents/report.txt"}' | python3 -m json.tool
```

**Output:**
```json
{
    "document": {
        "id": "019bc232-abcd-7890-b123-456789abcdef",
        "title": "report.txt",
        "meta": {
            "source": {"type": "local_path", "path": "/data/documents/report.txt"},
            "content_type": "text/plain"
        },
        "blocks": [
            {
                "type": "paragraph",
                "inlines": [{"type": "text", "text": "Report content here.", "style": {}}]
            }
        ]
    },
    "markdown": "Report content here.\n"
}
```

### Local File Parsing Errors

**Path with `..` component** — always rejected before any filesystem access:

```bash
curl -s -X POST "http://127.0.0.1:8087/file-parser/v1/parse-local" \
  -H "Content-Type: application/json" \
  -d '{"file_path": "/data/documents/../etc/passwd"}'
```

Response: **403 Forbidden**
```json
{
    "status": 403,
    "title": "Path Traversal Blocked",
    "detail": "Access denied: path '/data/documents/../etc/passwd' contains '..' traversal component"
}
```

**Path outside `allowed_local_base_dir`** — rejected after canonicalization:

```bash
curl -s -X POST "http://127.0.0.1:8087/file-parser/v1/parse-local" \
  -H "Content-Type: application/json" \
  -d '{"file_path": "/etc/hostname"}'
```

Response: **403 Forbidden**
```json
{
    "status": 403,
    "title": "Path Traversal Blocked",
    "detail": "Access denied: '/etc/hostname' is outside the allowed base directory"
}
```

For additional endpoints, see <http://127.0.0.1:8087/docs>.
