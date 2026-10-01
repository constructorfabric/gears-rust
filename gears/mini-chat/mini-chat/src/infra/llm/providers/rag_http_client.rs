//! Shared HTTP plumbing for RAG file and vector store operations.
//!
//! Extracted from `OpenAiFileStorage` / `OpenAiVectorStore` so that
//! provider-specific implementations only need to construct URIs and
//! delegate to this client for the actual HTTP mechanics.

use std::sync::Arc;

use bytes::Bytes;
use oagw_sdk::multipart::{MultipartBody, Part};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::de::DeserializeOwned;
use toolkit_security::SecurityContext;

use crate::domain::ports::{FileStorageError, UploadFileParams};

/// Reusable HTTP client for RAG operations proxied through OAGW.
///
/// Provides three primitives — multipart upload, JSON POST, and DELETE —
/// with response parsing and error mapping. Provider-specific impls
/// build URIs and delegate here.
pub struct RagHttpClient {
    oagw: Arc<dyn ServiceGatewayClientV1>,
}

impl RagHttpClient {
    pub fn new(oagw: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self { oagw }
    }

    /// Upload a file via multipart/form-data POST.
    ///
    /// Collects the `FileStream` into bytes, then uses `Part::bytes` to build
    /// a buffered multipart body with `Content-Length`. The handler already
    /// collected chunks for size enforcement, so this is a move (not a copy)
    /// from the handler's `Vec<Bytes>` into the multipart body.
    ///
    /// True streaming via `Part::stream` is blocked by OAGW chunked encoding
    /// issues (premature connection close before termination chunk). Once OAGW
    /// stabilizes chunked request body support, this can switch to `Part::stream`.
    ///
    /// Returns `(provider_file_id, bytes_uploaded)`.
    pub async fn multipart_upload(
        &self,
        ctx: SecurityContext,
        uri: &str,
        params: UploadFileParams,
    ) -> Result<(String, u64), FileStorageError> {
        use futures::StreamExt;

        #[derive(serde::Deserialize)]
        struct FileObject {
            id: String,
        }

        // Collect stream into bytes.
        // The stream may yield `multer::Error::FieldSizeExceeded` from the
        // handler's size constraints — propagate as Rejected so the domain
        // layer maps it to FileTooLarge (400), not ProviderError (503).
        let mut file_buf = Vec::new();
        let mut stream = params.file_stream;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                if e.downcast_ref::<multer::Error>().is_some_and(|me| {
                    matches!(
                        me,
                        multer::Error::FieldSizeExceeded { .. }
                            | multer::Error::StreamSizeExceeded { .. }
                    )
                }) {
                    FileStorageError::Rejected {
                        code: "file_too_large".to_owned(),
                        message: e.to_string(),
                    }
                } else {
                    FileStorageError::Unavailable {
                        message: format!("file stream error: {e}"),
                    }
                }
            })?;
            file_buf.extend_from_slice(&chunk);
        }
        let bytes_uploaded = file_buf.len() as u64;

        tracing::debug!(bytes_uploaded, uri, "multipart upload: building request");

        let multipart = MultipartBody::new().text("purpose", params.purpose).part(
            Part::bytes("file", file_buf)
                .filename(params.filename)
                .content_type(params.content_type),
        );

        let mut req = multipart
            .into_request(http::Method::POST, uri)
            .map_err(|e| FileStorageError::Configuration {
                message: format!("failed to build file upload request: {e}"),
            })?;
        req.headers_mut().insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        );

        let bytes = self.send(ctx, req, "file upload").await?;

        let file_obj: FileObject =
            serde_json::from_slice(&bytes).map_err(|e| FileStorageError::InvalidResponse {
                message: format!("failed to parse upload response: {e}"),
            })?;

        Ok((file_obj.id, bytes_uploaded))
    }

    /// Send a JSON POST and parse the typed response.
    pub async fn json_post<T: DeserializeOwned>(
        &self,
        ctx: SecurityContext,
        uri: &str,
        body: &serde_json::Value,
    ) -> Result<T, FileStorageError> {
        let body_bytes = serde_json::to_vec(body).map_err(|e| FileStorageError::Configuration {
            message: format!("JSON serialization: {e}"),
        })?;

        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "application/json")
            .body(Body::Bytes(Bytes::from(body_bytes)))
            .map_err(|e| FileStorageError::Configuration {
                message: format!("failed to build JSON POST request: {e}"),
            })?;

        let bytes = self.send(ctx, req, "JSON POST").await?;

        serde_json::from_slice(&bytes).map_err(|e| FileStorageError::InvalidResponse {
            message: format!("failed to parse JSON response: {e}"),
        })
    }

    /// Send a GET and parse the typed JSON response.
    pub async fn json_get<T: DeserializeOwned>(
        &self,
        ctx: SecurityContext,
        uri: &str,
    ) -> Result<T, FileStorageError> {
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .header(http::header::ACCEPT, "application/json")
            .body(Body::Empty)
            .map_err(|e| FileStorageError::Configuration {
                message: format!("failed to build GET request: {e}"),
            })?;

        let bytes = self.send(ctx, req, "GET").await?;

        serde_json::from_slice(&bytes).map_err(|e| FileStorageError::InvalidResponse {
            message: format!("failed to parse JSON response: {e}"),
        })
    }

    /// Send a JSON POST without parsing the response body.
    pub async fn json_post_no_response(
        &self,
        ctx: SecurityContext,
        uri: &str,
        body: &serde_json::Value,
    ) -> Result<(), FileStorageError> {
        let body_bytes = serde_json::to_vec(body).map_err(|e| FileStorageError::Configuration {
            message: format!("JSON serialization: {e}"),
        })?;

        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "application/json")
            .body(Body::Bytes(Bytes::from(body_bytes)))
            .map_err(|e| FileStorageError::Configuration {
                message: format!("failed to build JSON POST request: {e}"),
            })?;

        self.send(ctx, req, "JSON POST").await?;
        Ok(())
    }

    /// Send a DELETE request.
    ///
    /// 2xx and 404 (already gone) are `Ok(())`; any other status is an error,
    /// mapped the same way as [`Self::send`].
    pub async fn delete(&self, ctx: SecurityContext, uri: &str) -> Result<(), FileStorageError> {
        let req = http::Request::builder()
            .method(http::Method::DELETE)
            .uri(uri)
            .body(Body::Empty)
            .map_err(|e| FileStorageError::Configuration {
                message: format!("failed to build delete request: {e}"),
            })?;

        let (status, bytes) = self.proxy(ctx, req, "delete").await?;
        if status == http::StatusCode::NOT_FOUND {
            return Ok(());
        }
        check_status(status, &bytes, "delete")
    }

    /// Send a request through OAGW and return the response bytes,
    /// checking for non-success status.
    async fn send(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
        op_name: &str,
    ) -> Result<Bytes, FileStorageError> {
        let (status, bytes) = self.proxy(ctx, req, op_name).await?;
        check_status(status, &bytes, op_name)?;
        Ok(bytes)
    }

    /// Send a request through OAGW and return the status and body bytes.
    async fn proxy(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
        op_name: &str,
    ) -> Result<(http::StatusCode, Bytes), FileStorageError> {
        let response =
            self.oagw
                .proxy_request(ctx, req)
                .await
                .map_err(|e| FileStorageError::Unavailable {
                    message: format!("OAGW {op_name} failed: {e}"),
                })?;

        let (parts, resp_body) = response.into_parts();
        let bytes =
            resp_body
                .into_bytes()
                .await
                .map_err(|e| FileStorageError::InvalidResponse {
                    message: format!("failed to read {op_name} response body: {e}"),
                })?;

        Ok((parts.status, bytes))
    }
}

/// 5xx maps to `Unavailable`, any other non-2xx to `Rejected`.
fn check_status(
    status: http::StatusCode,
    bytes: &[u8],
    op_name: &str,
) -> Result<(), FileStorageError> {
    if status.is_server_error() {
        let detail = String::from_utf8_lossy(bytes);
        return Err(FileStorageError::Unavailable {
            message: format!("{op_name} returned {status}: {detail}"),
        });
    }
    if !status.is_success() {
        let detail = String::from_utf8_lossy(bytes);
        return Err(FileStorageError::Rejected {
            code: format!("{op_name}_failed"),
            message: format!("{op_name} returned {status}: {detail}"),
        });
    }
    Ok(())
}

/// `vector_store.file` object returned by the `OpenAI` and Azure `OpenAI`
/// vector store file endpoints.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct VectorStoreFileObject {
    /// `in_progress`, `completed`, `failed` or `cancelled`. Treated as
    /// `in_progress` when absent, so polling goes on and a status that never
    /// arrives ends as `failed` at the wait limit.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    last_error: Option<VectorStoreFileError>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct VectorStoreFileError {
    #[serde(default)]
    message: String,
}

impl VectorStoreFileObject {
    #[must_use]
    pub fn into_status(self) -> crate::domain::ports::VectorStoreFileStatus {
        use crate::domain::ports::VectorStoreFileStatus;
        match self.status.as_deref() {
            Some("completed") => VectorStoreFileStatus::Completed,
            None | Some("in_progress") => VectorStoreFileStatus::InProgress,
            Some(other) => VectorStoreFileStatus::Failed {
                message: self
                    .last_error
                    .map(|e| e.message)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| format!("vector store file status '{other}'")),
            },
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::ports::FileStorageError;

    /// Minimal OAGW mock that returns a fixed HTTP status code.
    struct StatusCodeOagw {
        status: http::StatusCode,
        body: String,
    }

    #[async_trait::async_trait]
    impl ServiceGatewayClientV1 for StatusCodeOagw {
        async fn create_upstream(
            &self,
            _: SecurityContext,
            _: oagw_sdk::CreateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: SecurityContext,
            _: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Upstream>, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: oagw_sdk::UpdateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _: SecurityContext,
            _: oagw_sdk::CreateRouteRequest,
        ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: SecurityContext,
            _: Option<uuid::Uuid>,
            _: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Route>, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: oagw_sdk::UpdateRouteRequest,
        ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn delete_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn resolve_proxy_target(
            &self,
            _: SecurityContext,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), toolkit_canonical_errors::CanonicalError>
        {
            unimplemented!()
        }
        async fn proxy_request(
            &self,
            _: SecurityContext,
            _: http::Request<Body>,
        ) -> Result<http::Response<Body>, toolkit_canonical_errors::CanonicalError> {
            Ok(http::Response::builder()
                .status(self.status)
                .body(Body::Bytes(Bytes::from(self.body.clone())))
                .unwrap())
        }
    }

    fn test_ctx() -> SecurityContext {
        crate::domain::service::test_helpers::test_security_ctx(uuid::Uuid::new_v4())
    }

    fn json_post_request() -> http::Request<Body> {
        http::Request::builder()
            .method("POST")
            .uri("http://test/v1/files")
            .body(Body::Bytes(Bytes::from(r#"{"test":true}"#)))
            .unwrap()
    }

    #[tokio::test]
    async fn test_send_503_returns_unavailable() {
        let oagw: Arc<dyn ServiceGatewayClientV1> = Arc::new(StatusCodeOagw {
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            body: "service down".to_owned(),
        });
        let client = RagHttpClient::new(oagw);
        let result = client
            .send(test_ctx(), json_post_request(), "test_op")
            .await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), FileStorageError::Unavailable { .. }),
            "503 should map to Unavailable"
        );
    }

    #[tokio::test]
    async fn test_send_400_returns_rejected() {
        let oagw: Arc<dyn ServiceGatewayClientV1> = Arc::new(StatusCodeOagw {
            status: http::StatusCode::BAD_REQUEST,
            body: "bad request".to_owned(),
        });
        let client = RagHttpClient::new(oagw);
        let result = client
            .send(test_ctx(), json_post_request(), "test_op")
            .await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), FileStorageError::Rejected { .. }),
            "400 should map to Rejected"
        );
    }

    async fn delete_with_status(status: http::StatusCode) -> Result<(), FileStorageError> {
        let oagw: Arc<dyn ServiceGatewayClientV1> = Arc::new(StatusCodeOagw {
            status,
            body: "detail".to_owned(),
        });
        RagHttpClient::new(oagw)
            .delete(test_ctx(), "http://test/v1/files/file-1")
            .await
    }

    #[tokio::test]
    async fn delete_2xx_and_404_are_ok() {
        for status in [
            http::StatusCode::OK,
            http::StatusCode::NO_CONTENT,
            http::StatusCode::NOT_FOUND,
        ] {
            assert!(
                delete_with_status(status).await.is_ok(),
                "{status} should be Ok"
            );
        }
    }

    #[tokio::test]
    async fn delete_4xx_returns_rejected() {
        for status in [
            http::StatusCode::BAD_REQUEST,
            http::StatusCode::UNAUTHORIZED,
            http::StatusCode::FORBIDDEN,
            http::StatusCode::CONFLICT,
        ] {
            let err = delete_with_status(status).await.unwrap_err();
            assert!(
                matches!(err, FileStorageError::Rejected { ref code, .. } if code == "delete_failed"),
                "{status} should map to Rejected, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn delete_5xx_returns_unavailable() {
        let err = delete_with_status(http::StatusCode::INTERNAL_SERVER_ERROR)
            .await
            .unwrap_err();
        assert!(
            matches!(err, FileStorageError::Unavailable { .. }),
            "500 should map to Unavailable, got {err:?}"
        );
    }

    /// Status mapping of a vector store file: a missing status keeps polling
    /// (the wait limit ends it), an unknown one is a failure.
    #[test]
    fn vector_store_file_status_mapping() {
        use crate::domain::ports::VectorStoreFileStatus;
        let status = |v: serde_json::Value| {
            serde_json::from_value::<VectorStoreFileObject>(v)
                .unwrap()
                .into_status()
        };
        assert!(matches!(
            status(serde_json::json!({})),
            VectorStoreFileStatus::InProgress
        ));
        assert!(matches!(
            status(serde_json::json!({"status": "in_progress"})),
            VectorStoreFileStatus::InProgress
        ));
        assert!(matches!(
            status(serde_json::json!({"status": "completed"})),
            VectorStoreFileStatus::Completed
        ));
        match status(serde_json::json!({"status": "failed", "last_error": {"message": "bad file"}}))
        {
            VectorStoreFileStatus::Failed { message } => assert_eq!(message, "bad file"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(matches!(
            status(serde_json::json!({"status": "cancelled"})),
            VectorStoreFileStatus::Failed { .. }
        ));
    }
}
