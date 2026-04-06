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
        _: Option<uuid::Uuid>,
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
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), oagw_sdk::error::ServiceGatewayError> {
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
