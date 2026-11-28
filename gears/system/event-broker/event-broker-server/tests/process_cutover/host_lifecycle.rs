//! Observes the host's directory calls and the telemetry exported by its owned process.
use super::cutover::{Pair, raw_grpc::Wire};
use anyhow::{Context as _, Result, ensure};
use bytes::Bytes;
use prost::Message;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    net::TcpListener,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{Notify, oneshot};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request, Response, Status,
    codegen::{BoxFuture, Service, http},
    transport::Channel,
};

const OBSERVE: Duration = Duration::from_secs(15);

#[derive(Clone, PartialEq, prost::Message)]
struct Presence {
    #[prost(string, tag = "1")]
    gear: String,
    #[prost(string, tag = "2")]
    instance: String,
    #[prost(string, optional, tag = "5")]
    address: Option<String>,
    #[prost(string, optional, tag = "9")]
    openapi_spec_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Event {
    method: String,
    gear: String,
    instance: String,
    address: Option<String>,
    openapi_spec_hash: Option<String>,
    outcome: Option<tonic::Code>,
}

#[derive(Clone)]
struct Directory {
    channel: Channel,
    events: Arc<Mutex<Vec<Event>>>,
    update: Arc<Notify>,
}
impl tonic::server::NamedService for Directory {
    const NAME: &'static str = "directory.v1.DirectoryService";
}

struct Forward {
    directory: Directory,
    path: http::uri::PathAndQuery,
    attempt: Option<usize>,
}
impl tonic::server::UnaryService<Bytes> for Forward {
    type Response = Bytes;
    type Future = BoxFuture<Response<Bytes>, Status>;
    fn call(&mut self, request: Request<Bytes>) -> Self::Future {
        let directory = self.directory.clone();
        let path = self.path.clone();
        let attempt = self.attempt;
        Box::pin(async move {
            if let Some(attempt) = attempt {
                let p = Presence::decode(request.get_ref().clone())
                    .map_err(|_| Status::invalid_argument("presence record"))?;
                let mut events = directory
                    .events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let event = &mut events[attempt];
                event.gear = p.gear;
                event.instance = p.instance;
                event.address = p.address;
                event.openapi_spec_hash = p.openapi_spec_hash;
            }
            let mut upstream = tonic::client::Grpc::new(directory.channel);
            upstream
                .ready()
                .await
                .map_err(|_| Status::unavailable("directory transport"))?;
            upstream.unary(request, path, Wire).await
        })
    }
}
impl Service<http::Request<tonic::body::Body>> for Directory {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, r: http::Request<tonic::body::Body>) -> Self::Future {
        let directory = self.clone();
        let path = r
            .uri()
            .path_and_query()
            .cloned()
            .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
        let method = path.path().rsplit('/').next().unwrap_or_default();
        let attempt = matches!(
            method,
            "RegisterInstance" | "DeregisterInstance" | "Heartbeat"
        )
        .then(|| {
            let mut events = directory
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = events.len();
            events.push(Event {
                method: method.to_owned(),
                gear: String::new(),
                instance: String::new(),
                address: None,
                openapi_spec_hash: None,
                outcome: None,
            });
            index
        });
        let forward = Forward {
            directory: directory.clone(),
            path,
            attempt,
        };
        Box::pin(async move {
            let response = tonic::server::Grpc::new(Wire).unary(forward, r).await;
            if let Some(attempt) = attempt {
                let outcome = Status::from_header_map(response.headers())
                    .map_or(tonic::Code::Ok, |status| status.code());
                directory
                    .events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)[attempt]
                    .outcome = Some(outcome);
                directory.update.notify_one();
            }
            Ok(response)
        })
    }
}

fn bootstrap_instance(log: &str) -> Result<String> {
    let instances = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["fields"]["message"] == "OoP gear bootstrap starting")
        .map(|event| {
            let id = event["fields"]["instance_id"]
                .as_str()
                .context("bootstrap instance")?;
            Ok(uuid::Uuid::parse_str(id)?.to_string())
        })
        .collect::<Result<Vec<_>>>()?;
    let [instance] = instances.as_slice() else {
        anyhow::bail!("expected a single host bootstrap instance")
    };
    Ok(instance.clone())
}

#[tokio::test]
async fn the_broker_uses_the_merged_role_for_its_single_directory_registration() -> Result<()> {
    let pair = Pair::start().await?;
    let pristine = pair.broker_config()?;
    let directory = Directory {
        channel: tonic::transport::Endpoint::from_shared(pair.grpc.clone())?
            .connect()
            .await?,
        events: Arc::default(),
        update: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(directory.clone())
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _shutdown = stopped.await;
            }),
    );
    for inherit in [false, true] {
        let mut local = pristine.clone();
        let config = local["gears"]["event-broker"]["config"].clone();
        let rendered = if inherit {
            local["gears"]["event-broker"]
                .as_object_mut()
                .unwrap()
                .remove("config");
            json!({"config":config})
        } else {
            json!({"config":{"mode":"cluster_ingest","process":{"cluster":{"mode":"memory"}}}})
        };
        pair.write_broker_config(&local)?;
        directory
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let case = if inherit { "inherited" } else { "local" };
        let mut broker = pair.rendered_broker(case, &address, &rendered)?;
        let (base, page) = pair.authenticated_page(&mut broker).await?;
        assert_eq!(page.status(), 200);
        assert_eq!(
            page.json::<Value>().await?,
            json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
        );
        let openapi_bytes = tokio::time::timeout(OBSERVE, async {
            let response = pair
                .client
                .get(format!("{base}/.well-known/openapi.json"))
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            assert_eq!(
                response.headers().get(reqwest::header::CONTENT_TYPE),
                Some(&reqwest::header::HeaderValue::from_static(
                    "application/json"
                ))
            );
            response.bytes().await
        })
        .await??;
        let openapi: Value = serde_json::from_slice(&openapi_bytes)?;
        openapi
            .get("paths")
            .and_then(Value::as_object)
            .and_then(|paths| paths.get("/event-broker/v1/consumer-groups"))
            .and_then(|route| route.get("get"))
            .and_then(Value::as_object)
            .context("broker OpenAPI is missing GET /event-broker/v1/consumer-groups")?;
        let openapi_spec_hash = toolkit::directory::openapi_spec_hash(&openapi_bytes);
        tokio::time::timeout(OBSERVE, async {
            loop {
                if directory
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e.method == "Heartbeat" && e.outcome == Some(tonic::Code::Ok))
                {
                    break;
                }
                directory.update.notified().await;
            }
        })
        .await?;
        broker.terminate()?;
        assert_eq!(broker.exited().await?.code(), Some(0));
        let events = directory
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let instance = bootstrap_instance(&broker.safe_log(&pair.workload)?)?;
        let [
            registered,
            first_heartbeat,
            other_heartbeats @ ..,
            deregistered,
        ] = events.as_slice()
        else {
            anyhow::bail!(
                "{case}: expected registration, renewal and deregistration, got {events:?}"
            );
        };
        assert_eq!(
            registered,
            &Event {
                method: "RegisterInstance".into(),
                gear: "event-broker".into(),
                instance: instance.clone(),
                address: Some(base.clone()),
                openapi_spec_hash: Some(openapi_spec_hash.clone()),
                outcome: Some(tonic::Code::Ok),
            },
            "{case}: registration",
        );
        for (index, heartbeat) in std::iter::once(first_heartbeat)
            .chain(other_heartbeats)
            .enumerate()
        {
            assert_eq!(
                heartbeat,
                &Event {
                    method: "Heartbeat".into(),
                    gear: "event-broker".into(),
                    instance: instance.clone(),
                    address: None,
                    openapi_spec_hash: None,
                    outcome: Some(tonic::Code::Ok),
                },
                "{case}: heartbeat {index}",
            );
        }
        assert_eq!(
            deregistered,
            &Event {
                method: "DeregisterInstance".into(),
                gear: "event-broker".into(),
                instance: instance.clone(),
                address: None,
                openapi_spec_hash: None,
                outcome: Some(tonic::Code::Ok),
            },
            "{case}: deregistration",
        );
        println!(
            "{case}: merged role event-broker, instance {instance}, advertised {base}, OpenAPI SHA-256 {openapi_spec_hash}, accepted one registration, {} heartbeats and one deregistration",
            1 + other_heartbeats.len(),
        );
    }
    let _receiver = stop.send(());
    tokio::time::timeout(OBSERVE, task).await???;
    assert_distributed_sqlite_refusals(&pair, &pristine, &directory).await?;
    pair.finish()
}

async fn assert_distributed_sqlite_refusals(
    pair: &Pair,
    pristine: &Value,
    directory: &Directory,
) -> Result<()> {
    let sqlite_diagnostic = concat!(
        "database: the gear's database is SQLite; a SQLite database serves a single process, ",
        "so only `standalone` mode runs on one (feature 0005 section 2.4, C10)"
    );
    let (mut remote, endpoint) = pair.remote_cluster().await?;
    let mut refusals = Vec::new();
    for (mode, role) in [
        ("cluster_ingest", "event-broker-ingest"),
        ("cluster_delivery", "event-broker-delivery"),
        ("cluster_dispatcher", "event-broker-dispatcher"),
    ] {
        let mut local = pristine.clone();
        let mut config = local["gears"]["event-broker"]
            .as_object_mut()
            .context("event-broker gear config")?
            .remove("config")
            .context("event-broker application config")?;
        config["mode"] = json!(mode);
        config["process"]["cluster"] = json!({"mode":"remote"});
        local["gears"]["cluster-sdk"] =
            json!({"clients":{"consumes":{"cluster":{"grpc_endpoint":endpoint}}}});
        pair.write_broker_config(&local)?;
        directory
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("http://{}", listener.local_addr()?);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(directory.clone())
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _shutdown = stopped.await;
                }),
        );
        let mut broker = pair.rendered_broker(mode, &address, &json!({"config":config}))?;
        let exit_code = broker.exited().await?.code();
        // Drain the observer's connections before reading and reusing the recorder.
        let _receiver = stop.send(());
        tokio::time::timeout(OBSERVE, task).await???;
        let log = broker.safe_log(&pair.workload)?;
        let instance = bootstrap_instance(&log);
        let sqlite_refused = log.contains(sqlite_diagnostic);
        let cleanup_gears: Vec<_> = log
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["fields"]["message"] == "deregistered from DirectoryService")
            .map(|event| event["fields"]["gear"].as_str().map(str::to_owned))
            .collect();
        let observed = directory
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let receipt = json!({
            "mode": mode,
            "exit_code": exit_code,
            "bootstrap_instance": instance.as_ref().ok(),
            "cleanup_gears": cleanup_gears,
            "gear_database_refusal": sqlite_refused.then_some(sqlite_diagnostic),
            "directory_calls": observed.iter().map(|event| &event.method).collect::<Vec<_>>(),
        });
        println!("{receipt}");
        refusals.push((
            mode,
            role,
            exit_code,
            instance,
            sqlite_refused,
            cleanup_gears,
            observed,
        ));
    }
    remote.stop()?;
    for (mode, role, exit_code, instance, sqlite_refused, cleanup_gears, observed) in refusals {
        assert_eq!(exit_code, Some(1), "{mode}: refusal exit code");
        assert!(sqlite_refused, "{mode}: expected {sqlite_diagnostic}");
        instance.with_context(|| format!("{mode}: broker bootstrap instance"))?;
        assert_eq!(
            cleanup_gears,
            [Some(role.to_owned())],
            "{mode}: effective role from host cleanup",
        );
        assert_eq!(
            observed.as_slice(),
            [],
            "{mode}: refused broker attempted Directory lifecycle calls",
        );
    }
    Ok(())
}

// These projections read the OTLP fields used by the assertion and skip all other attributes.
#[derive(Clone, PartialEq, Message)]
struct Export {
    #[prost(message, repeated, tag = "1")]
    resources: Vec<ResourceData>,
}
#[derive(Clone, PartialEq, Message)]
struct ResourceData {
    #[prost(message, repeated, tag = "2")]
    scopes: Vec<ScopeData>,
}
#[derive(Clone, PartialEq, Message)]
struct ScopeData {
    #[prost(bytes = "bytes", repeated, tag = "2")]
    items: Vec<Bytes>,
}
#[derive(Clone, PartialEq, Message)]
struct SpanName {
    #[prost(string, tag = "5")]
    name: String,
}
#[derive(Clone, PartialEq, Message)]
struct Metric {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, optional, tag = "7")]
    sum: Option<Sum>,
}
#[derive(Clone, PartialEq, Message)]
struct Sum {
    #[prost(message, repeated, tag = "1")]
    data: Vec<DataPoint>,
}
#[derive(Clone, PartialEq, Message)]
struct DataPoint {
    #[prost(message, repeated, tag = "7")]
    attributes: Vec<Attribute>,
}
#[derive(Clone, PartialEq, Message)]
struct Attribute {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<TextValue>,
}
#[derive(Clone, PartialEq, Message)]
struct TextValue {
    #[prost(string, tag = "1")]
    text: String,
}
#[derive(Clone, Default)]
struct Exported {
    spans: Vec<String>,
    cache_points: Vec<std::collections::BTreeMap<String, String>>,
}
#[derive(Clone)]
struct Collector<const METRICS: bool>(Arc<Mutex<Exported>>);
impl tonic::server::NamedService for Collector<true> {
    const NAME: &'static str = "opentelemetry.proto.collector.metrics.v1.MetricsService";
}
impl tonic::server::NamedService for Collector<false> {
    const NAME: &'static str = "opentelemetry.proto.collector.trace.v1.TraceService";
}
impl<const METRICS: bool> tonic::server::UnaryService<Bytes> for Collector<METRICS> {
    type Response = Bytes;
    type Future = BoxFuture<Response<Bytes>, Status>;
    fn call(&mut self, r: Request<Bytes>) -> Self::Future {
        let seen = self.0.clone();
        Box::pin(async move {
            let invalid = |_| Status::invalid_argument("malformed OTLP export");
            let export = Export::decode(r.into_inner()).map_err(invalid)?;
            let mut seen = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for scope in export.resources.into_iter().flat_map(|r| r.scopes) {
                for item in scope.items {
                    if METRICS {
                        let metric = Metric::decode(item).map_err(invalid)?;
                        if metric.name == "cluster_cache_ops"
                            && let Some(sum) = metric.sum
                        {
                            for point in sum.data {
                                seen.cache_points.push(
                                    point
                                        .attributes
                                        .into_iter()
                                        .filter_map(|a| Some((a.key, a.value?.text)))
                                        .collect(),
                                );
                            }
                        }
                    } else {
                        seen.spans
                            .push(SpanName::decode(item).map_err(invalid)?.name);
                    }
                }
            }
            Ok(Response::new(Bytes::new()))
        })
    }
}
impl<const METRICS: bool> Service<http::Request<tonic::body::Body>> for Collector<METRICS> {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, r: http::Request<tonic::body::Body>) -> Self::Future {
        let collector = self.clone();
        Box::pin(async move { Ok(tonic::server::Grpc::new(Wire).unary(collector, r).await) })
    }
}

#[tokio::test]
async fn the_broker_exports_a_trace_on_bind_failure_and_cache_telemetry_on_clean_exit() -> Result<()>
{
    let collision = TcpListener::bind("127.0.0.1:0")?;
    let pair = Pair::start_at(collision.local_addr()?).await?;
    let collector = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let rendered = json!({"opentelemetry":{"resource":{"service_name":"eb-cutover"},
        "exporter":{"kind":"otlp_grpc","endpoint":format!("http://{}",collector.local_addr()?),"timeout_ms":2500},
        "tracing":{"enabled":true,"sampler":{"always_on":{}}},"metrics":{"enabled":true}}});
    let seen = Arc::new(Mutex::new(Exported::default()));
    let (stop, stopped) = oneshot::channel();
    let receiver = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(Collector::<true>(seen.clone()))
            .add_service(Collector::<false>(seen.clone()))
            .serve_with_incoming_shutdown(TcpListenerStream::new(collector), async {
                let _shutdown = stopped.await;
            }),
    );
    let mut failed = pair.rendered_broker("occupied", &pair.grpc, &rendered)?;
    assert_eq!(failed.exited().await?.code(), Some(1));
    let log = failed.safe_log(&pair.workload)?;
    ensure!(
        log.contains("Address already in use") || log.contains("address already in use"),
        "expected listener bind failure"
    );
    {
        let mut captured = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        println!(
            "post-bind-failure exports: {} spans, {} cache points",
            captured.spans.len(),
            captured.cache_points.len()
        );
        ensure!(
            !captured.spans.is_empty(),
            "post-construction error must flush traces"
        );
        assert!(captured.cache_points.is_empty());
        *captured = Exported::default();
    }
    drop(collision);
    let mut broker = pair.rendered_broker("restart-after-bind", &pair.grpc, &rendered)?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), 200);
    assert_eq!(
        page.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    let _stream_url = pair.subscription(&base).await?;
    broker.terminate()?;
    assert_eq!(broker.exited().await?.code(), Some(0));
    let log = broker.safe_log(&pair.workload)?;
    ensure!(
        log.contains("deregistered from DirectoryService"),
        "owned broker did not deregister"
    );
    let _receiver = stop.send(());
    tokio::time::timeout(OBSERVE, receiver).await???;
    let captured = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure!(
        captured
            .spans
            .iter()
            .any(|name| name == "cluster.cache.put"),
        "expected cache put trace export"
    );
    let write = std::collections::BTreeMap::from([
        ("op".into(), "put".into()),
        ("provider".into(), "eb-memory".into()),
        ("result".into(), "ok".into()),
    ]);
    ensure!(
        captured.cache_points.contains(&write),
        "cache points {:?}",
        captured.cache_points
    );
    println!(
        "graceful shutdown exported {} spans and {} eb-memory cache operation points",
        captured.spans.len(),
        captured.cache_points.len()
    );
    pair.finish()
}
