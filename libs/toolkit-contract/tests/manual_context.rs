//! `ManualCallContext` on its own: the request helper and its misuse guards.

#![cfg(feature = "raw-http")]
#![allow(clippy::unwrap_used)]

use axum::Router;
use axum::extract::Request;
use axum::routing::any;
use toolkit_contract::ir::binding::{
    HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr, StreamFraming,
};
use toolkit_contract::runtime::client::build_default_http_client;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::manual::{ManualCallContext, ManualTransport};
use toolkit_contract::runtime::raw::empty_body;
use toolkit_contract::runtime::transport_error::TransportError;

fn method_binding(
    http_method: HttpMethod,
    field_bindings: Vec<HttpFieldBinding>,
) -> HttpMethodBindingIr {
    HttpMethodBindingIr {
        method_name: "call".to_owned(),
        http_method,
        path_template: "/call".to_owned(),
        field_bindings,
        retryable: false,
        streaming: false,
        stream_framing: StreamFraming::default(),
        optional: false,
    }
}

/// Answers every request with its method in `x-method`.
async fn start_server() -> String {
    let app = Router::new().route(
        "/api/call",
        any(|req: Request| async move {
            axum::http::Response::builder()
                .header("x-method", req.method().as_str())
                .body(axum::body::Body::empty())
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn request_uses_the_verb_of_the_binding() {
    let config = ClientConfig::new(start_server().await);
    let http = build_default_http_client("test", &config).unwrap();
    let transport = ManualTransport::new("test", &config);

    for verb in [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Head,
        HttpMethod::Options,
    ] {
        let method = method_binding(verb, Vec::new());
        let binding = HttpBindingIr {
            base_path: "/api".to_owned(),
            methods: vec![method.clone()],
        };
        let call =
            ManualCallContext::new(&http, &config, &binding, &method, None, None, &transport);
        let url = call.url(&serde_json::json!({}), None).unwrap();
        let resp = call.request(&url).unwrap().send().await.unwrap();
        assert_eq!(
            resp.headers()["x-method"].to_str().unwrap(),
            verb.as_str().unwrap()
        );
    }
}

#[tokio::test]
async fn helpers_refuse_the_wrong_kind_of_method() {
    let config = ClientConfig::new("http://provider.invalid");
    let http = build_default_http_client("test", &config).unwrap();
    let transport = ManualTransport::new("test", &config);

    let passthrough = method_binding(
        HttpMethod::Passthrough,
        vec![HttpFieldBinding::Passthrough {
            field: "req".to_owned(),
            param: "path".to_owned(),
            methods: vec![HttpMethod::Get],
        }],
    );
    let get = method_binding(HttpMethod::Get, Vec::new());
    let binding = HttpBindingIr {
        base_path: "/api".to_owned(),
        methods: vec![passthrough.clone(), get.clone()],
    };

    let call = ManualCallContext::new(
        &http,
        &config,
        &binding,
        &passthrough,
        None,
        None,
        &transport,
    );
    assert!(matches!(
        call.request("http://provider.invalid/api/call"),
        Err(TransportError::InvalidRequest(_))
    ));

    let call = ManualCallContext::new(&http, &config, &binding, &get, None, None, &transport);
    let req = http::Request::get("/x").body(empty_body()).unwrap();
    assert!(matches!(
        call.send_raw(req).await,
        Err(TransportError::InvalidRequest(_))
    ));
}
