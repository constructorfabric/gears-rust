//! Shared HTTP plumbing for RAG file and vector store operations.
//!
//! Extracted from `OpenAiFileStorage` / `OpenAiVectorStore` so that
//! provider-specific implementations only need to construct URIs and
//! delegate to this client for the actual HTTP mechanics.

use std::sync::Arc;

use bytes::Bytes;
use modkit_security::SecurityContext;
use oagw_sdk::multipart::{MultipartBody, Part};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::de::DeserializeOwned;

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
        // layer maps it to FileTooLarge (413), not ProviderError (502).
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

    /// Send a DELETE request. Returns `Ok(())` on success.
    pub async fn delete(&self, ctx: SecurityContext, uri: &str) -> Result<(), FileStorageError> {
        let req = http::Request::builder()
            .method(http::Method::DELETE)
            .uri(uri)
            .body(Body::Empty)
            .map_err(|e| FileStorageError::Configuration {
                message: format!("failed to build delete request: {e}"),
            })?;

        // For delete, we don't check status — best-effort.
        self.oagw
            .proxy_request(ctx, req)
            .await
            .map_err(|e| FileStorageError::Unavailable {
                message: format!("delete request failed: {e}"),
            })?;

        Ok(())
    }

    /// Send a request through OAGW and return the response bytes,
    /// checking for non-success status.
    async fn send(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
        op_name: &str,
    ) -> Result<Bytes, FileStorageError> {
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

        if parts.status.is_server_error() {
            let detail = String::from_utf8_lossy(&bytes);
            return Err(FileStorageError::Unavailable {
                message: format!("{op_name} returned {}: {detail}", parts.status),
            });
        }
        if !parts.status.is_success() {
            let detail = String::from_utf8_lossy(&bytes);
            return Err(FileStorageError::Rejected {
                code: format!("{op_name}_failed"),
                message: format!("{op_name} returned {}: {detail}", parts.status),
            });
        }

        Ok(bytes)
    }
}

#[cfg(test)]
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
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: SecurityContext,
            _: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Upstream>, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: oagw_sdk::UpdateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _: SecurityContext,
            _: oagw_sdk::CreateRouteRequest,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Route>, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: oagw_sdk::UpdateRouteRequest,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn resolve_proxy_target(
            &self,
            _: SecurityContext,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), oagw_sdk::error::ServiceGatewayError>
        {
            unimplemented!()
        }
        async fn proxy_request(
            &self,
            _: SecurityContext,
            _: http::Request<Body>,
        ) -> Result<http::Response<Body>, oagw_sdk::error::ServiceGatewayError> {
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
}
