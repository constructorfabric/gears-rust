use super::*;
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use tower::ServiceExt;

/// Test service that asserts the User-Agent header matches the expected value.
#[derive(Clone)]
struct CheckUaService {
    expected_ua: HeaderValue,
}

impl Service<Request<Full<Bytes>>> for CheckUaService {
    type Response = Response<Full<Bytes>>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Full<Bytes>>) -> Self::Future {
        let ua = req.headers().get(http::header::USER_AGENT);
        assert_eq!(ua, Some(&self.expected_ua));
        std::future::ready(Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::new()))
            .unwrap()))
    }
}

#[tokio::test]
async fn test_user_agent_added() {
    let check_service = CheckUaService {
        expected_ua: HeaderValue::from_static("test-agent/1.0"),
    };

    let layer = UserAgentLayer::try_new("test-agent/1.0").unwrap();
    let mut service = layer.layer(check_service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    service.ready().await.unwrap().call(req).await.unwrap();
}

#[tokio::test]
async fn test_user_agent_not_overwritten() {
    let check_service = CheckUaService {
        expected_ua: HeaderValue::from_static("custom-agent/2.0"),
    };

    let layer = UserAgentLayer::try_new("test-agent/1.0").unwrap();
    let mut service = layer.layer(check_service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .header(http::header::USER_AGENT, "custom-agent/2.0")
        .body(Full::new(Bytes::new()))
        .unwrap();

    service.ready().await.unwrap().call(req).await.unwrap();
}

#[test]
fn test_user_agent_layer_invalid_value() {
    // Control characters are invalid in header values
    let result = UserAgentLayer::try_new("invalid\x00agent");
    assert!(result.is_err());
}
