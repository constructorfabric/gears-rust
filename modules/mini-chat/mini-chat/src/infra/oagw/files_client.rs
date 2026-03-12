use std::sync::Arc;

use bytes::Bytes;
use modkit_security::SecurityContext;
use oagw_sdk::{Body, MultipartBody, Part, ServiceGatewayClientV1};
use tracing::debug;

use crate::domain::error::DomainError;

use super::validate_path_segment;

/// Reject filenames that could cause injection or are malformed.
///
/// Consolidates all filename validation rules in a single place (called from
/// the handler boundary). Checks applied:
/// - non-empty
/// - no null bytes
/// - no `..` path traversal
/// - no `/` or `\` path separators
/// - max 255 Unicode characters (uses `chars().count()` for correctness)
pub fn validate_filename(value: &str) -> Result<(), DomainError> {
    if value.is_empty()
        || value.contains('\0')
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.chars().count() > 255
    {
        return Err(DomainError::Validation {
            message: "Filename is empty, too long, or contains invalid characters".into(),
        });
    }
    Ok(())
}

/// Client for OAGW Files API (upload/delete provider files).
pub struct OagwFilesClient {
    gw: Arc<dyn ServiceGatewayClientV1>,
    alias: String,
}

impl OagwFilesClient {
    pub fn new(gw: Arc<dyn ServiceGatewayClientV1>, alias: String) -> Self {
        Self { gw, alias }
    }

    /// Upload a file to the provider via OAGW.
    /// Returns the provider file ID.
    pub async fn upload_file(
        &self,
        ctx: SecurityContext,
        filename: &str,
        content_type: &str,
        data: Bytes,
    ) -> Result<String, DomainError> {
        validate_filename(filename)?;
        let multipart = MultipartBody::new().text("purpose", "assistants").part(
            Part::bytes("file", data)
                .filename(filename)
                .content_type(content_type),
        );

        let uri = format!("/{}/v1/files", self.alias);
        let req = multipart
            .into_request("POST", &uri)
            .map_err(|e| DomainError::Internal {
                message: format!("failed to build upload request: {e}"),
            })?;

        let resp = self
            .gw
            .proxy_request(ctx, req)
            .await
            .map_err(|e| DomainError::Internal {
                message: format!("OAGW upload failed: {e}"),
            })?;

        let (parts, body) = resp.into_parts();
        if !parts.status.is_success() {
            return Err(DomainError::Internal {
                message: format!("provider upload failed with status {}", parts.status),
            });
        }

        let body_bytes = body.into_bytes().await.map_err(|e| DomainError::Internal {
            message: format!("failed to read upload response: {e}"),
        })?;

        let json: serde_json::Value =
            serde_json::from_slice(&body_bytes).map_err(|e| DomainError::Internal {
                message: format!("failed to parse upload response: {e}"),
            })?;

        let file_id = json["id"]
            .as_str()
            .ok_or_else(|| DomainError::Internal {
                message: "missing 'id' in upload response".to_owned(),
            })?
            .to_owned();

        debug!(file_id = %file_id, "File uploaded to provider");
        Ok(file_id)
    }

    /// Delete a file from the provider via OAGW.
    pub async fn delete_file(
        &self,
        ctx: SecurityContext,
        provider_file_id: &str,
    ) -> Result<(), DomainError> {
        validate_path_segment(provider_file_id, "provider_file_id")?;
        let uri = format!("/{}/v1/files/{}", self.alias, provider_file_id);
        let req = http::Request::builder()
            .method("DELETE")
            .uri(&uri)
            .body(Body::Empty)
            .map_err(|e| DomainError::Internal {
                message: format!("failed to build delete request: {e}"),
            })?;

        let resp = self
            .gw
            .proxy_request(ctx, req)
            .await
            .map_err(|e| DomainError::Internal {
                message: format!("OAGW delete failed: {e}"),
            })?;

        let status = resp.into_parts().0.status;
        if status.is_success() || status == http::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(DomainError::Internal {
                message: format!("OAGW delete failed for file {provider_file_id}: HTTP {status}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_filename;

    #[test]
    fn accepts_normal_filenames() {
        assert!(validate_filename("report.pdf").is_ok());
        assert!(validate_filename("my file (1).docx").is_ok());
        assert!(validate_filename("\u{65e5}\u{672c}\u{8a9e}.txt").is_ok());
        assert!(validate_filename("a").is_ok());
    }

    #[test]
    fn rejects_empty_filename() {
        assert!(validate_filename("").is_err());
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(validate_filename("file\0name.txt").is_err());
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_filename("../../../etc/passwd").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("foo/../bar").is_err());
    }

    #[test]
    fn rejects_path_separators() {
        assert!(validate_filename("path/file.txt").is_err());
        assert!(validate_filename("path\\file.txt").is_err());
    }

    #[test]
    fn rejects_filename_over_255_chars() {
        let long = "a".repeat(256);
        assert!(validate_filename(&long).is_err());
    }

    #[test]
    fn accepts_filename_at_255_chars() {
        let exact = "a".repeat(255);
        assert!(validate_filename(&exact).is_ok());
    }

    #[test]
    fn limit_counts_unicode_chars_not_bytes() {
        // 255 multi-byte chars (each 3 bytes in UTF-8) should be accepted.
        let unicode_255: String = "\u{3042}".repeat(255);
        assert!(validate_filename(&unicode_255).is_ok());
        let unicode_256: String = "\u{3042}".repeat(256);
        assert!(validate_filename(&unicode_256).is_err());
    }
}
