#[path = "common/cutover.rs"]
mod cutover;

#[cfg(feature = "e2e-flight-control")]
#[path = "process_cutover/private_auth.rs"]
mod private_auth;

#[cfg(feature = "e2e-flight-control")]
#[path = "process_cutover/cluster_modes.rs"]
mod cluster_modes;

#[cfg(all(unix, feature = "e2e-flight-control"))]
#[path = "process_cutover/host_lifecycle.rs"]
mod host_lifecycle;

#[cfg(all(unix, feature = "e2e-flight-control"))]
#[path = "process_cutover/ingress.rs"]
mod ingress;

use anyhow::{Context, Result, ensure};
use cutover::Pair;
use serde_json::Value;

#[cfg(feature = "e2e-flight-control")]
#[tokio::test]
async fn the_broker_publishes_its_base_schemas_to_an_independent_registry() -> Result<()> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../testing/e2e/suites/event_broker/fixtures/demo_entities.json"
    ))?;
    let documents: Vec<_> = fixture["items"]
        .as_array()
        .context("demo registration items")?
        .iter()
        .map(|item| item["content"].clone())
        .collect();
    let started = std::time::Instant::now();
    let pair = Pair::start_selected(&[]).await?;
    let core_pid = pair.core_pid()?;
    assert_core_plugin_rows(&pair).await?;
    assert_absent_base_schemas(&pair, started).await?;

    let mut config = pair.broker_config()?;
    config["gears"]["event-broker"]["config"]["workers"]["specification_refresh_interval_secs"] =
        1.into();
    pair.write_broker_config(&config)?;
    let args = ["--e2e-flight-control", "run"];
    let mut broker = pair.broker("schema-publisher", &args, Some(&pair.grpc))?;
    let broker_pid = broker.pid();
    let base_etags = pair.await_base_schemas(&mut broker).await?;
    let demo_etags = pair.admit_documents(&mut broker, &documents).await?;
    let pages = assert_demo_is_usable(&pair, &mut broker, &documents).await?;
    let inventory = assert_registry_inventory(&pair).await?;
    broker.finish().await?;

    let mut restarted = pair.broker("schema-publisher-restarted", &args, Some(&pair.grpc))?;
    assert_ne!(restarted.pid(), broker_pid);
    assert_eq!(pair.core_pid()?, core_pid);
    assert_eq!(
        assert_demo_is_usable(&pair, &mut restarted, &documents).await?,
        pages,
    );
    assert_eq!(pair.await_base_schemas(&mut restarted).await?, base_etags);
    assert_eq!(pair.registry_documents(&documents).await?, demo_etags);
    assert_base_schema_validators(&pair, base_etags).await?;
    assert_eq!(assert_registry_inventory(&pair).await?, inventory);
    assert_eq!(pair.core_pid()?, core_pid);
    restarted.finish().await?;
    pair.finish()
}

#[cfg(feature = "e2e-flight-control")]
const BROKER_BASE_IDS: [&str; 2] = [
    "gts.cf.core.events.event.v1~",
    "gts.cf.core.events.topic.v1~",
];

#[cfg(feature = "e2e-flight-control")]
async fn assert_registry_inventory(pair: &Pair) -> Result<Value> {
    use reqwest::{Method, StatusCode};
    use serde_json::json;

    let response = pair
        .registry_request(
            Method::GET,
            "/types-registry/v2/platform/entities?pattern=gts.cf.core.events.*&\
             $select=content,origin&lifecycle_status=all&limit=10",
        )?
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let inventory = pair.registry_json(response).await?;
    assert_eq!(inventory["page_info"], json!({"limit": 10}));
    let items = inventory["items"]
        .as_array()
        .context("registry page items")?;
    let versions: Vec<_> = items
        .iter()
        .map(|item| (&item["gts_id"], &item["origin"]["resource_version"]))
        .collect();
    assert_eq!(
        versions,
        [
            (&json!(BROKER_BASE_IDS[0]), &json!(1)),
            (
                &json!("gts.cf.core.events.event.v1~cf.core.demo.greeting.v1~"),
                &json!(1)
            ),
            (
                &json!("gts.cf.core.events.event.v1~cf.core.demo.spread.v1~"),
                &json!(1)
            ),
            (&json!(BROKER_BASE_IDS[1]), &json!(1)),
            (
                &json!("gts.cf.core.events.topic.v1~cf.core.demo.topic.v1"),
                &json!(1)
            ),
        ]
    );
    println!(
        "Core {} Event Broker inventory has five rows at version 1",
        pair.core_pid()?
    );
    Ok(inventory)
}

#[cfg(feature = "e2e-flight-control")]
async fn assert_base_schema_validators(
    pair: &Pair,
    etags: Vec<reqwest::header::HeaderValue>,
) -> Result<()> {
    use reqwest::{Method, StatusCode, header};

    for (id, etag) in BROKER_BASE_IDS.into_iter().zip(etags) {
        let path = format!("/types-registry/v2/platform/entities/{id}?$select=content");
        let response = pair
            .registry_request(Method::GET, &path)?
            .header(header::IF_NONE_MATCH, &etag)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(header::ETAG), Some(&etag));
        assert!(response.bytes().await?.is_empty());
        println!(
            "Core {} returned bodyless 304 for {id} at {}",
            pair.core_pid()?,
            etag.to_str()?
        );
    }
    Ok(())
}

#[cfg(feature = "e2e-flight-control")]
async fn assert_demo_is_usable(
    pair: &Pair,
    broker: &mut cutover::Process,
    documents: &[Value],
) -> Result<Vec<Value>> {
    use reqwest::{StatusCode, header};
    use serde_json::json;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    let (base, groups) = pair.authenticated_page(broker).await?;
    assert_eq!(groups.status(), StatusCode::OK);
    assert_eq!(
        pair.registry_json(groups).await?,
        json!({"items":[],"page_info":{"limit":25,"next_cursor":null,"prev_cursor":null}})
    );
    let log = broker.safe_log(&pair.workload)?;
    let events: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|event: &Value| event["target"] == "toolkit::runtime::host_runtime")
        .collect();
    let phase = events
        .iter()
        .position(|event| {
            event["fields"]["message"]
                == "Phase: GTS publication (application group waits for admission)"
        })
        .context("broker must cross the startup publication barrier")?;
    let init = events
        .iter()
        .position(|event| {
            event["fields"]["message"] == "Initializing a gear..."
                && event["fields"]["gear"] == "event-broker"
        })
        .context("broker must initialize after the publication barrier")?;
    assert_eq!(events[phase]["fields"]["publishers"], 1);
    assert!(phase < init);
    println!(
        "Broker {} startup publication: {}",
        broker.pid(),
        events[phase]
    );
    let pages = pair.await_catalogue(broker, &base, documents).await?;
    let producer = "00000000-0000-0000-0000-000000000102";
    timeout(Duration::from_secs(30), async {
        loop {
            broker.require_owned()?;
            let response = pair
                .client
                .post(format!("{base}/event-broker/v1/events"))
                .bearer_auth(cutover::OWNER)
                .header(
                    "traceparent",
                    "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
                )
                .json(&json!({
                    "id": "00000000-0000-0000-0000-000000000101",
                    "type": "gts.cf.core.events.event.v1~cf.core.demo.greeting.v1~",
                    "subject_tenant": cutover::OWNER_TENANT,
                    "source": "demo-fixture",
                    "subject": "probe",
                    "subject_type": "gts.cf.core.demo.subject.v1~",
                    "occurred_at": "2026-01-01T00:00:00Z",
                    "meta": {"version": 1, "producer": producer, "previous": 0, "sequence": 1},
                }))
                .send()
                .await?;
            let status = response.status();
            if status == StatusCode::SERVICE_UNAVAILABLE {
                sleep(Duration::from_millis(40)).await;
                continue;
            }
            let mime = response.headers().get(header::CONTENT_TYPE).cloned();
            let body = pair.registry_json(response).await?;
            assert_eq!(
                (status, mime.as_ref().and_then(|v| v.to_str().ok()), &body),
                (
                    StatusCode::NOT_FOUND,
                    Some("application/problem+json"),
                    &json!({
                        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                        "title": "Not Found",
                        "status": 404,
                        "detail": "the producer is not registered or its registration has expired",
                        "instance": "/event-broker/v1/events",
                        "trace_id": "0123456789abcdef0123456789abcdef",
                        "context": {
                            "resource_type": "gts.cf.core.events.producer.v1~",
                            "resource_name": producer,
                        },
                    }),
                )
            );
            broker.require_owned()?;
            println!(
                "Broker {} reached the producer lookup: {body}",
                broker.pid()
            );
            return Ok::<(), anyhow::Error>(());
        }
    })
    .await
    .context("demo topic did not provision before the deadline")??;
    Ok(pages)
}

#[cfg(feature = "e2e-flight-control")]
async fn assert_core_plugin_rows(pair: &Pair) -> Result<()> {
    use reqwest::{Method, StatusCode, header};
    use serde_json::json;

    let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"gts");
    for (id, vendor) in [
        (
            "gts.cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~cf.e2e.eb_test_policy.plugin.v1",
            "e2e",
        ),
        (
            "gts.cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~cf.builtin.static_authn_resolver.plugin.v1",
            "constructorfabric",
        ),
    ] {
        let path = format!("/types-registry/v2/platform/entities/{id}?$select=content");
        let response = pair.registry_request(Method::GET, &path)?.send().await?;
        let status = response.status();
        let mime = response.headers().get(header::CONTENT_TYPE).cloned();
        let body = pair.registry_json(response).await?;
        assert_eq!(
            (status, mime.as_ref().and_then(|v| v.to_str().ok()), body),
            (
                StatusCode::OK,
                Some("application/json"),
                json!({
                    "gts_id": id,
                    "gts_uuid": uuid::Uuid::new_v5(&namespace, id.as_bytes()),
                    "kind": "instance",
                    "lifecycle_status": "active",
                    "ownership": "global",
                    "content": {"id": id, "vendor": vendor, "priority": 100, "properties": {}},
                })
            ),
        );
    }
    Ok(())
}

#[cfg(feature = "e2e-flight-control")]
async fn assert_absent_base_schemas(pair: &Pair, started: std::time::Instant) -> Result<()> {
    use reqwest::{Method, StatusCode, header};
    use serde_json::json;

    let core_pid = pair.core_pid()?;
    let ids = BROKER_BASE_IDS;
    let mut missing = Vec::new();
    for id in ids {
        let path = format!("/types-registry/v2/platform/entities/{id}");
        let response = pair
            .registry_request(Method::GET, &format!("{path}?$select=content"))?
            .header(
                "traceparent",
                "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
            )
            .send()
            .await?;
        let status = response.status();
        let mime = response.headers().get(header::CONTENT_TYPE).cloned();
        let body = pair.registry_json(response).await?;
        println!(
            "Fresh Core {core_pid} alone after {} ms: {id} {status} {body}",
            started.elapsed().as_millis()
        );
        missing.push((status, mime, body));
    }
    assert_eq!(
        missing
            .iter()
            .map(|(status, _, _)| *status)
            .collect::<Vec<_>>(),
        [StatusCode::NOT_FOUND; 2]
    );
    for (id, (_, mime, body)) in ids.into_iter().zip(missing) {
        assert_eq!(
            (mime.as_ref().and_then(|v| v.to_str().ok()), body),
            (
                Some("application/problem+json"),
                json!({
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                    "title": "Not Found",
                    "status": 404,
                    "detail": format!("No entity with GTS ID: {id}"),
                    "instance": format!("/types-registry/v2/platform/entities/{id}"),
                    "trace_id": "0123456789abcdef0123456789abcdef",
                    "context": {
                        "resource_type": "gts.cf.core.types_registry.entity.v1~",
                        "resource_name": id,
                    },
                }),
            )
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_default_broker_refuses_tenant_serving_with_a_reachable_directory() -> Result<()> {
    let pair = Pair::start().await?;
    let mut broker = pair.broker("ordinary", &["-v"], Some(&pair.grpc))?;
    let status = pair.refusal_exit(&mut broker).await?;
    let log = broker.safe_log(&pair.workload)?;
    let refusal = "gear 'event-broker' serves tenant operations but no tenant authenticator is available (link authn-resolver, or set oop_http.auth_disabled for development): ";
    let refusal_line = log
        .lines()
        .find(|line| line.contains(refusal))
        .context("broker must reach tenant route authentication")?;
    assert_eq!(status.code(), Some(1));
    let decoded = serde_json::from_str::<Value>(refusal_line).ok();
    let line = decoded
        .as_ref()
        .and_then(|v| v["fields"]["error"].as_str())
        .unwrap_or(refusal_line);
    let (_, routes) = line.split_once(refusal).context("route diagnostic")?;
    let mut routes: Vec<String> = serde_json::Deserializer::from_str(routes)
        .into_iter()
        .next()
        .context("refusal lists every protected route")??;
    routes.sort();
    assert_eq!(
        routes,
        [
            "DELETE /event-broker/v1/consumer-groups/{id}",
            "DELETE /event-broker/v1/subscriptions/{id}",
            "GET /event-broker/v1/consumer-groups",
            "GET /event-broker/v1/consumer-groups/{id}",
            "GET /event-broker/v1/event-types",
            "GET /event-broker/v1/events:sse",
            "GET /event-broker/v1/events:stream",
            "GET /event-broker/v1/producers/{id}/cursors",
            "GET /event-broker/v1/subscriptions",
            "GET /event-broker/v1/subscriptions/{id}",
            "GET /event-broker/v1/topics",
            "GET /event-broker/v1/topics/segments",
            "POST /event-broker/v1/consumer-groups",
            "POST /event-broker/v1/events",
            "POST /event-broker/v1/events:batch",
            "POST /event-broker/v1/producers",
            "POST /event-broker/v1/producers/{id}",
            "POST /event-broker/v1/subscriptions",
            "POST /event-broker/v1/subscriptions/{id}"
        ]
    );
    #[cfg(not(feature = "e2e-flight-control"))]
    {
        let mut flagged = pair.broker(
            "unsupported",
            &["--e2e-flight-control", "run"],
            Some(&pair.grpc),
        )?;
        assert_eq!(flagged.exited().await?.code(), Some(2));
        ensure!(
            flagged
                .safe_log(&pair.workload)?
                .contains("unexpected argument '--e2e-flight-control'"),
            "default CLI must reject fixture option"
        );
    }
    #[cfg(feature = "e2e-flight-control")]
    for (name, endpoint) in [
        ("missing", None),
        ("invalid", Some("http://localhost:50051")),
    ] {
        let mut flagged = pair.broker(name, &["--e2e-flight-control", "run"], endpoint)?;
        assert_eq!(flagged.exited().await?.code(), Some(1));
        ensure!(
            flagged
                .safe_log(&pair.workload)?
                .contains("invalid composition configuration field: E2E_FLIGHT_CONTROL_ENDPOINT"),
            "fixture endpoint fails validation"
        );
    }
    pair.check_merged_config_offline().await?;
    pair.finish()
}

#[cfg(feature = "e2e-flight-control")]
#[tokio::test]
async fn the_opted_in_broker_lists_groups_for_the_authenticated_fixture_owner() -> Result<()> {
    use cutover::{OWNER, UNKNOWN};
    use reqwest::StatusCode;
    use serde_json::json;

    let pair = Pair::start_at(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let mut broker = pair.broker(
        "opted-in",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let (base, response) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await?,
        json!({"items":[],"page_info":{"next_cursor":null,"prev_cursor":null,"limit":25}})
    );
    for path in ["consumer-groups", "topics"] {
        let rejected = pair
            .client
            .get(format!("{base}/event-broker/v1/{path}"))
            .bearer_auth(UNKNOWN)
            .header(
                "traceparent",
                "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
            )
            .header("x-request-id", "cutover-invalid-bearer")
            .send()
            .await?;
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        let body = rejected.text().await?;
        ensure!(
            ![OWNER, UNKNOWN, &pair.workload]
                .into_iter()
                .any(|value| body.contains(value)),
            "credential in error response"
        );
        println!(
            "Unknown fixture bearer response for {path}: {}",
            serde_json::from_str::<Value>(&body)?
        );
    }
    broker.require_live()?;
    pair.assert_unknown_bearer_refused().await?;
    broker.safe_log(&pair.workload)?;
    broker.stop()?;
    pair.finish()
}

#[cfg(feature = "e2e-flight-control")]
#[tokio::test]
async fn an_exited_broker_cannot_take_credit_for_another_listeners_page() -> Result<()> {
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let pair = Pair::start_at(listener.local_addr()?).await?;
    let mut broker = pair.broker("completed-check", &["check"], None)?;
    assert_eq!(broker.exited().await?.code(), Some(0));
    let page = json!({"items":[],"page_info":{"next_cursor":null,"prev_cursor":null,"limit":25}});
    let serve = async {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let mut buffer = [0; 1024];
            let _read = socket.read(&mut buffer).await?;
            let body = page.to_string();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await?;
            socket.write_all(body.as_bytes()).await?;
            socket.shutdown().await?;
        }
    };
    let check = async {
        let seen = pair.client.get(&pair.base).send().await?;
        assert_eq!(seen.status(), reqwest::StatusCode::OK);
        assert_eq!(seen.json::<Value>().await?, page);
        let failure = pair
            .authenticated_page(&mut broker)
            .await
            .err()
            .context("an exited broker was credited with the response")?;
        assert_eq!(
            failure.to_string(),
            "owned broker exited before HTTP acceptance"
        );
        Ok::<(), anyhow::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! { result = serve => result, result = check => result }
    })
    .await
    .context("bounded alternate listener check")??;
    pair.finish()
}

#[cfg(feature = "e2e-flight-control")]
#[tokio::test]
async fn concurrent_http_denials_reach_the_fixture_with_their_trace_ids_without_an_exporter()
-> Result<()> {
    let pair = Pair::start_at(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let mut broker = pair.broker(
        "trace-observer",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let (base, response) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(pair.fixture_denial_traces()?, []);
    pair.assert_unknown_bearer_refused().await?;
    assert_eq!(pair.fixture_denial_traces()?, [None]);
    let deny = |route, id, trace| {
        let url = format!("{base}/event-broker/v1/{route}");
        let request = pair
            .client
            .get(url)
            .bearer_auth(cutover::UNKNOWN)
            .header("traceparent", format!("00-{trace}-1111111111111111-01"))
            .header("x-request-id", id);
        async move {
            let reply = request.send().await?;
            assert_eq!(reply.status(), reqwest::StatusCode::UNAUTHORIZED);
            let problem = reply.json::<Value>().await?;
            assert_eq!(problem["trace_id"], trace);
            Ok::<(), anyhow::Error>(())
        }
    };
    let groups = "0123456789abcdef0123456789abcdef";
    let topics = "1123456789abcdef0123456789abcdef";
    tokio::try_join!(
        deny("consumer-groups", "cutover-groups", groups),
        deny("topics", "cutover-topics", topics)
    )?;
    broker.require_live()?;
    let traces = pair.fixture_denial_traces()?;
    broker.stop()?;
    pair.finish()?;
    let (direct, incoming) = traces
        .split_first()
        .context("expected the independent direct RPC")?;
    assert_eq!(direct, &None);
    let mut incoming = incoming.to_vec();
    incoming.sort();
    assert_eq!(incoming, [Some(groups.to_owned()), Some(topics.to_owned())]);
    Ok(())
}
