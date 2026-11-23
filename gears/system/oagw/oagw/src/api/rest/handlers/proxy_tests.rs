// Created: 2026-04-07 by Constructor Tech
/// Requests driven through the REST router to the shared mock upstream: how
/// the handler splits the alias, the path suffix and the query, and what
/// the upstream receives.
mod addressing {
    use std::sync::Arc;

    use http::Method;
    use serde_json::json;
    use uuid::Uuid;

    use crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID;
    use crate::domain::plugin::{PluginError, TransformPlugin, TransformRequestContext};
    use crate::test_support::{
        AppHarness, MockBody, MockGuard, MockResponse, MockUpstream, RecordedRequest, TestResponse,
    };

    pub(super) fn unique_alias(prefix: &str) -> String {
        format!("{prefix}-{}", Uuid::new_v4().as_simple())
    }

    /// An upstream on `127.0.0.1:port` with `alias` (and `plugins`, if
    /// any); returns its ID.
    pub(super) async fn upstream(
        h: &AppHarness,
        alias: &str,
        port: u16,
        plugins: Option<serde_json::Value>,
    ) -> String {
        let mut body = json!({
            "server": {"endpoints": [{"host": "127.0.0.1", "port": port, "scheme": "http"}]},
            "protocol": oagw_sdk::HTTP_PROTOCOL_ID,
            "alias": alias,
        });
        if let Some(plugins) = plugins {
            body["plugins"] = plugins;
        }
        let resp = h
            .api_v1()
            .post_upstream()
            .with_body(body)
            .expect_status(201)
            .await;
        resp.json()["id"].as_str().unwrap().to_owned()
    }

    /// A route with `methods` on `path`, path suffixes appended.
    pub(super) async fn route(
        h: &AppHarness,
        upstream_id: &str,
        methods: &[&str],
        path: &str,
        query_allowlist: &[&str],
    ) -> serde_json::Value {
        h.api_v1()
            .post_route()
            .with_body(json!({
                "upstream_id": upstream_id,
                "match": {"http": {
                    "methods": methods,
                    "path": path,
                    "query_allowlist": query_allowlist,
                }},
            }))
            .expect_status(201)
            .await
            .json()
    }

    /// `method` on the proxy URL `/oagw/v1/proxy/{rest}`, sent as written.
    pub(super) async fn send(
        h: &AppHarness,
        method: Method,
        rest: &str,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        use tower::ServiceExt as _;
        let mut req = http::Request::builder()
            .method(method)
            .uri(format!("/oagw/v1/proxy/{rest}"));
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let resp = h
            .router()
            .clone()
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        TestResponse::from_response(resp).await
    }

    pub(super) fn ok_text(body: &str) -> MockResponse {
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: MockBody::Text(body.into()),
        }
    }

    fn uris(recorded: &[RecordedRequest]) -> Vec<&str> {
        recorded.iter().map(|r| r.uri.as_str()).collect()
    }

    /// The alias is the first segment; the rest is the path suffix the
    /// upstream gets after the route path.
    #[tokio::test]
    async fn alias_and_path_suffix_split_at_the_first_slash() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/v1/chat/completions", ok_text("ok"));
        let alias = unique_alias("split");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["GET"], &guard.path("/v1"), &[]).await;

        let resp = send(
            &h,
            Method::GET,
            &format!("{alias}{}", guard.path("/v1/chat/completions")),
            &[],
        )
        .await;
        resp.assert_status(200);
        assert_eq!(
            uris(&guard.recorded_requests().await),
            [guard.path("/v1/chat/completions")]
        );
    }

    /// An alias may carry a port (`host:8443`): the first `/`, not the `:`,
    /// ends it.
    #[tokio::test]
    async fn alias_with_a_port() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/path", ok_text("ok"));
        let alias = format!("{}:8443", unique_alias("port"));
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["GET"], &guard.path("/path"), &[]).await;

        send(
            &h,
            Method::GET,
            &format!("{alias}{}", guard.path("/path")),
            &[],
        )
        .await
        .assert_status(200);
        assert_eq!(
            uris(&guard.recorded_requests().await),
            [guard.path("/path")]
        );
    }

    /// The bare alias has an empty path suffix: route `/` covers it and the
    /// upstream gets `/`, with the query.
    #[tokio::test]
    async fn alias_without_a_path_suffix() {
        let h = AppHarness::builder().build().await;
        // A mock of its own: the request reaches `/`, which the shared one
        // serves for every test.
        let mock = MockUpstream::start().await;
        let alias = unique_alias("bare");
        let id = upstream(&h, &alias, mock.addr().port(), None).await;
        route(&h, &id, &["GET"], "/", &["a"]).await;

        // The mock has no handler: its 404 is relayed as the upstream's.
        let resp = send(&h, Method::GET, &alias, &[]).await;
        resp.assert_status(404);
        resp.assert_header("x-oagw-error-source", "upstream");
        send(&h, Method::GET, &format!("{alias}?a=1"), &[])
            .await
            .assert_status(404);
        assert_eq!(uris(&mock.recorded_requests().await), ["/", "/?a=1"]);
    }

    /// The upstream gets the client's query byte for byte: `%2B` vs `+`,
    /// `%20`, reserved characters left unescaped, repeated keys, empty
    /// values, keys without `=`, the case of escapes and the order. Before,
    /// the query was decoded and re-encoded, which changed all of these.
    /// A literal `;` is not among them: see
    /// `a_literal_semicolon_is_sent_encoded`.
    #[tokio::test]
    async fn query_is_forwarded_as_sent() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/search", ok_text("ok"));
        let alias = unique_alias("raw-query");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        let allow = ["q", "b", "a", "empty", "flag", "path", "lower", "upper"];
        route(&h, &id, &["GET"], &guard.path("/search"), &allow).await;

        let queries = [
            "q=a%2Bb&q=a+b&q=a%20b",
            "b=2&a=1&b=1",
            "empty=&flag&flag=",
            "path=/x/y?z:@!$'()*,",
            "lower=%c3%a9&upper=%C3%A9",
            "q=a%26b%3Dc",
            "",
        ];
        for query in queries {
            let resp = send(
                &h,
                Method::GET,
                &format!("{alias}{}?{query}", guard.path("/search")),
                &[],
            )
            .await;
            resp.assert_status(200);
        }
        let expected: Vec<String> = queries
            .iter()
            .map(|q| format!("{}?{q}", guard.path("/search")))
            .collect();
        assert_eq!(uris(&guard.recorded_requests().await), expected);
    }

    /// The allowlist sees the decoded names: `my%20key` and `my+key` are
    /// both `my key`. The upstream still gets the bytes the client sent.
    #[tokio::test]
    async fn allowlist_checks_decoded_names() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/v1/models", ok_text("ok"));
        let alias = unique_alias("allow");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        let allow = ["version", "model", "my key"];
        route(&h, &id, &["GET"], &guard.path("/v1/models"), &allow).await;
        let path = guard.path("/v1/models");

        for query in ["version=2&model=gpt-4", "my%20key=value", "my+key=value"] {
            send(&h, Method::GET, &format!("{alias}{path}?{query}"), &[])
                .await
                .assert_status(200);
        }
        assert_eq!(
            uris(&guard.recorded_requests().await),
            [
                format!("{path}?version=2&model=gpt-4"),
                format!("{path}?my%20key=value"),
                format!("{path}?my+key=value"),
            ]
        );

        // `my%2520key` decodes to `my%20key`, which is not allowlisted.
        for query in ["version=2&debug=1", "my%2520key=v", "my_key"] {
            let resp = send(&h, Method::GET, &format!("{alias}{path}?{query}"), &[]).await;
            resp.assert_status(400);
            resp.assert_body_contains(oagw_sdk::field::QUERY_NOT_ALLOWED);
        }
        assert_eq!(guard.recorded_requests().await.len(), 3);
    }

    /// Characters the old re-encoding escaped go out unescaped: `|`, `{`,
    /// `}`, `[`, `]`, `\`, `^`, a backtick, raw UTF-8 bytes and an invalid
    /// escape.
    #[tokio::test]
    async fn characters_the_old_encoding_escaped_are_forwarded_as_sent() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/chars", ok_text("ok"));
        let alias = unique_alias("chars");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        let path = guard.path("/chars");
        route(&h, &id, &["GET"], &path, &["q", "n", "bad"]).await;

        let query = "q=a|b{c}[d]\\e^f`g&n=%C3%A9&n=\u{e9}&bad=%zz";
        send(&h, Method::GET, &format!("{alias}{path}?{query}"), &[])
            .await
            .assert_status(200);
        assert_eq!(
            uris(&guard.recorded_requests().await),
            [format!("{path}?{query}")]
        );
    }

    /// The allowlist splits the query on `&` only, so `model=x;admin=1` is
    /// one parameter, `model`. An upstream that also splits on `;` would
    /// read an `admin` nobody checked, so a query with a literal `;` goes
    /// out form-encoded: the `;` as `%3B`. An escaped `;` is sent as is, and
    /// a route whose allowlist is empty admits no parameter at all.
    #[tokio::test]
    async fn a_literal_semicolon_is_sent_encoded() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/semi", ok_text("ok"));
        guard.mock("GET", "/open", ok_text("ok"));
        let alias = unique_alias("semi");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        let semi = guard.path("/semi");
        let open = guard.path("/open");
        route(&h, &id, &["GET"], &semi, &["model"]).await;
        route(&h, &id, &["GET"], &open, &[]).await;

        for query in ["model=x;admin=1", "model=x%3Badmin%3D1&model=a%20b"] {
            send(&h, Method::GET, &format!("{alias}{semi}?{query}"), &[])
                .await
                .assert_status(200);
        }
        assert_eq!(
            uris(&guard.recorded_requests().await),
            [
                format!("{semi}?model=x%3Badmin%3D1"),
                format!("{semi}?model=x%3Badmin%3D1&model=a%20b"),
            ]
        );

        let resp = send(
            &h,
            Method::GET,
            &format!("{alias}{open}?model=x;admin=1"),
            &[],
        )
        .await;
        resp.assert_status(400);
        resp.assert_body_contains(oagw_sdk::field::QUERY_NOT_ALLOWED);
        assert_eq!(guard.recorded_requests().await.len(), 2);
    }

    /// Adds `added=1` when its binding config has `add`; otherwise leaves
    /// the query alone.
    struct AddQuery;

    #[async_trait::async_trait]
    impl TransformPlugin for AddQuery {
        async fn on_request(&self, ctx: &mut TransformRequestContext) -> Result<(), PluginError> {
            if ctx.config.contains_key("add") {
                ctx.query.push(("added".into(), "1".into()));
            }
            Ok(())
        }
    }

    /// A transform that changes the query gets the pairs form-encoded; one
    /// that leaves it alone keeps the client's bytes.
    #[tokio::test]
    async fn a_transform_that_changes_the_query_reencodes_it() {
        let h = AppHarness::builder()
            .with_transform_plugin(REQUEST_ID_TRANSFORM_PLUGIN_ID, Arc::new(AddQuery))
            .build()
            .await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/t", ok_text("ok"));
        let path = guard.path("/t");
        for (name, config) in [("keep", json!({})), ("add", json!({"add": "1"}))] {
            let alias = unique_alias(name);
            let plugins = json!({"items": [{
                "plugin_ref": REQUEST_ID_TRANSFORM_PLUGIN_ID,
                "config": config,
            }]});
            let id = upstream(&h, &alias, h.mock_port(), Some(plugins)).await;
            route(&h, &id, &["GET"], &path, &["q"]).await;
            send(
                &h,
                Method::GET,
                &format!("{alias}{path}?q=a%20b&q=a%2Bb"),
                &[],
            )
            .await
            .assert_status(200);
        }
        assert_eq!(
            uris(&guard.recorded_requests().await),
            [
                format!("{path}?q=a%20b&q=a%2Bb"),
                format!("{path}?q=a+b&q=a%2Bb&added=1"),
            ]
        );
    }
}

/// HEAD and OPTIONS are routed like any other method, when a route lists
/// them.
mod methods {
    use std::time::Duration;

    use http::Method;

    use super::addressing::{ok_text, route, send, unique_alias, upstream};
    use crate::test_support::{AppHarness, MockBody, MockGuard, MockResponse};

    /// A HEAD answer: the upstream's status and headers, its
    /// `Content-Length` included, and no body.
    #[tokio::test]
    async fn head_returns_the_upstream_head_without_a_body() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock(
            "HEAD",
            "/doc",
            MockResponse {
                status: 200,
                headers: vec![
                    ("content-type".into(), "application/json".into()),
                    ("content-length".into(), "10".into()),
                    ("x-doc".into(), "v1".into()),
                ],
                body: MockBody::Text("0123456789".into()),
            },
        );
        let alias = unique_alias("head");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        let created = route(&h, &id, &["HEAD"], &guard.path("/doc"), &[]).await;
        assert_eq!(
            created["match"]["http"]["methods"],
            serde_json::json!(["HEAD"])
        );

        let resp = tokio::time::timeout(
            Duration::from_secs(10),
            send(
                &h,
                Method::HEAD,
                &format!("{alias}{}", guard.path("/doc")),
                &[],
            ),
        )
        .await
        .expect("a HEAD response must not wait for a body");
        resp.assert_status(200);
        resp.assert_header("content-length", "10");
        resp.assert_header("x-doc", "v1");
        resp.assert_header("x-oagw-error-source", "upstream");
        assert!(resp.bytes().is_empty());
        let recorded = guard.recorded_requests().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].method, "HEAD");
    }

    /// Routing ignores the method's case, so `head` matches a HEAD route.
    /// The upstream gets `HEAD`, the method the route matched, and the
    /// answer is read as a HEAD answer: sent as `head`, the upstream may
    /// answer with a body, and its `Content-Length` would be awaited.
    #[tokio::test]
    async fn a_lowercase_method_is_sent_as_the_one_it_matched() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock(
            "HEAD",
            "/lower",
            MockResponse {
                status: 200,
                headers: vec![("content-length".into(), "10".into())],
                body: MockBody::Text("0123456789".into()),
            },
        );
        let alias = unique_alias("lower");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["HEAD"], &guard.path("/lower"), &[]).await;

        let resp = tokio::time::timeout(
            Duration::from_secs(10),
            send(
                &h,
                Method::from_bytes(b"head").unwrap(),
                &format!("{alias}{}", guard.path("/lower")),
                &[],
            ),
        )
        .await
        .expect("a HEAD response must not wait for a body");
        resp.assert_status(200);
        resp.assert_header("content-length", "10");
        assert!(resp.bytes().is_empty());
        let recorded = guard.recorded_requests().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].method, "HEAD");
    }

    /// A HEAD on an event stream ends at its head: the SSE path does not
    /// wait for events.
    #[tokio::test]
    async fn head_on_an_event_stream_ends_at_the_head() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock(
            "HEAD",
            "/events",
            MockResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
                body: MockBody::Sse(vec!["one".into()]),
            },
        );
        let alias = unique_alias("head-sse");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["HEAD"], &guard.path("/events"), &[]).await;

        let resp = tokio::time::timeout(
            Duration::from_secs(10),
            send(
                &h,
                Method::HEAD,
                &format!("{alias}{}", guard.path("/events")),
                &[],
            ),
        )
        .await
        .expect("a HEAD response must not wait for events");
        resp.assert_status(200);
        resp.assert_header("content-type", "text/event-stream");
        assert!(resp.bytes().is_empty());
    }

    /// A route lists HEAD and OPTIONS explicitly: a GET route does not
    /// answer HEAD, and neither answers OPTIONS.
    #[tokio::test]
    async fn get_does_not_imply_head_or_options() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock("GET", "/r", ok_text("ok"));
        let alias = unique_alias("no-head");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["GET"], &guard.path("/r"), &[]).await;
        let rest = format!("{alias}{}", guard.path("/r"));

        for method in [Method::HEAD, Method::OPTIONS] {
            let resp = send(&h, method.clone(), &rest, &[]).await;
            resp.assert_status(404);
            resp.assert_header("x-oagw-error-source", "gateway");
        }
        send(&h, Method::GET, &rest, &[]).await.assert_status(200);
        assert_eq!(guard.recorded_requests().await.len(), 1);
    }

    /// A HEAD with WebSocket upgrade headers is refused, not tunnelled.
    #[tokio::test]
    async fn head_with_upgrade_headers_is_refused() {
        let h = AppHarness::builder().build().await;
        let guard = MockGuard::new();
        let alias = unique_alias("head-ws");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["HEAD"], &guard.path("/ws"), &[]).await;

        let resp = send(
            &h,
            Method::HEAD,
            &format!("{alias}{}", guard.path("/ws")),
            &[
                ("upgrade", "websocket"),
                ("connection", "Upgrade"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
                ("sec-websocket-version", "13"),
            ],
        )
        .await;
        resp.assert_status(400);
        assert!(guard.recorded_requests().await.is_empty());
    }

    /// OPTIONS is proxied when the route lists it, with or without
    /// `Origin`; a CORS preflight (`Origin` and
    /// `Access-Control-Request-Method`) is still answered by the gateway.
    #[tokio::test]
    async fn options_is_routed_and_a_preflight_is_not() {
        let h = AppHarness::builder().build().await;
        let mut guard = MockGuard::new();
        guard.mock(
            "OPTIONS",
            "/res",
            MockResponse {
                status: 204,
                headers: vec![("allow".into(), "GET, OPTIONS".into())],
                body: MockBody::Text(String::new()),
            },
        );
        let alias = unique_alias("options");
        let id = upstream(&h, &alias, h.mock_port(), None).await;
        route(&h, &id, &["GET", "OPTIONS"], &guard.path("/res"), &[]).await;
        let rest = format!("{alias}{}", guard.path("/res"));

        for headers in [vec![], vec![("origin", "https://app.example.com")]] {
            let resp = send(&h, Method::OPTIONS, &rest, &headers).await;
            resp.assert_status(204);
            resp.assert_header("allow", "GET, OPTIONS");
            resp.assert_header("x-oagw-error-source", "upstream");
        }
        assert_eq!(guard.recorded_requests().await.len(), 2);

        let resp = send(
            &h,
            Method::OPTIONS,
            &rest,
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "GET"),
            ],
        )
        .await;
        resp.assert_status(204);
        resp.assert_header("access-control-allow-origin", "https://app.example.com");
        resp.assert_header("access-control-allow-methods", "GET");
        assert!(resp.headers().get("allow").is_none());
        assert_eq!(guard.recorded_requests().await.len(), 2);
    }

    /// Routes with HEAD and OPTIONS round-trip through the SDK as well.
    #[tokio::test]
    async fn sdk_routes_take_head_and_options() {
        use oagw_sdk::{
            CreateRouteRequest, CreateUpstreamRequest, Endpoint, HttpMatch, HttpMethod, MatchRules,
            PathSuffixMode, Scheme, Server,
        };
        let h = AppHarness::builder().build().await;
        let ctx = h.security_context().clone();
        let upstream = h
            .facade()
            .create_upstream(
                ctx.clone(),
                CreateUpstreamRequest::builder(
                    Server {
                        endpoints: vec![Endpoint {
                            scheme: Scheme::Http,
                            host: "127.0.0.1".into(),
                            port: h.mock_port(),
                        }],
                    },
                    oagw_sdk::HTTP_PROTOCOL_ID,
                )
                .alias(unique_alias("sdk-head"))
                .build(),
            )
            .await
            .unwrap();
        let route = h
            .facade()
            .create_route(
                ctx.clone(),
                CreateRouteRequest::builder(
                    upstream.id,
                    MatchRules {
                        http: Some(HttpMatch {
                            methods: vec![HttpMethod::Options, HttpMethod::Head],
                            path: "/v1".into(),
                            query_allowlist: vec![],
                            path_suffix_mode: PathSuffixMode::Append,
                        }),
                        grpc: None,
                    },
                )
                .build(),
            )
            .await
            .unwrap();
        let stored = h.facade().get_route(ctx, route.id).await.unwrap();
        assert_eq!(
            stored.match_rules.http.unwrap().methods,
            [HttpMethod::Head, HttpMethod::Options]
        );
    }
}

mod cancelled_uploads {
    use futures_util::StreamExt as _;
    use std::sync::Arc;
    use std::time::Duration;

    use tower::ServiceExt as _;

    use super::super::client_gone;
    use crate::infra::proxy::service::tests::RequestRecorder;
    use crate::test_support::AppHarness;

    fn io_error(kind: std::io::ErrorKind) -> axum::Error {
        axum::Error::new(std::io::Error::new(kind, "test"))
    }

    #[test]
    fn a_closed_or_reset_connection_is_the_client_gone() {
        use std::io::ErrorKind;
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::UnexpectedEof,
        ] {
            assert!(client_gone(&io_error(kind)), "{kind:?}");
        }
        assert!(!client_gone(&io_error(ErrorKind::InvalidData)));
        assert!(!client_gone(&axum::Error::new("bad chunk")));
    }

    /// An HTTP/2 stream the client cancelled is the client gone; a protocol
    /// error is not. `h2` must be the version hyper uses, or the downcast in
    /// `client_gone` never matches.
    #[test]
    fn a_cancelled_http2_stream_is_the_client_gone() {
        let h2_error = |reason| axum::Error::new(h2::Error::from(reason));
        assert!(client_gone(&h2_error(h2::Reason::CANCEL)));
        assert!(!client_gone(&h2_error(h2::Reason::PROTOCOL_ERROR)));
    }

    async fn harness() -> (AppHarness, Arc<RequestRecorder>) {
        let metrics = Arc::new(RequestRecorder::default());
        let h = AppHarness::builder()
            .with_metrics(metrics.clone())
            .build()
            .await;
        (h, metrics)
    }

    /// A client gone mid-upload is counted as `Cancelled`, not as a 400.
    #[tokio::test]
    async fn a_client_gone_mid_upload_is_counted_as_cancelled() {
        let (h, metrics) = harness().await;
        let body = axum::body::Body::from_stream(futures_util::stream::iter([
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial ")),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "client went away",
            )),
        ]));
        let request = http::Request::post("/oagw/v1/proxy/any-alias/v1/test")
            .body(body)
            .unwrap();
        let resp = h.router().clone().oneshot(request).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            metrics.events(),
            ["duration total", "error unknown Cancelled"]
        );
    }

    /// A handler dropped while it reads the body (the gateway's request
    /// timeout, a shutdown) is counted as `Cancelled`.
    #[tokio::test]
    async fn a_handler_dropped_mid_upload_is_counted_as_cancelled() {
        let (h, metrics) = harness().await;
        let body = axum::body::Body::from_stream(
            futures_util::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from_static(
                b"partial ",
            ))])
            .chain(futures_util::stream::pending()),
        );
        let request = http::Request::post("/oagw/v1/proxy/any-alias/v1/test")
            .body(body)
            .unwrap();
        let timed_out = tokio::time::timeout(
            Duration::from_millis(50),
            h.router().clone().oneshot(request),
        )
        .await;
        assert!(timed_out.is_err());
        assert_eq!(
            metrics.events(),
            ["duration total", "error unknown Cancelled"]
        );
    }
}
