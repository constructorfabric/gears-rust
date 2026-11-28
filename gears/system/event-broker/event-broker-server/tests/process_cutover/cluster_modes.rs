//! Counts remote cluster requests while both broker composition choices serve real operations.
use super::cutover::{Pair, Process, raw_grpc::Wire};
use anyhow::{Context as _, Result, ensure};
use bytes::Bytes;
use cluster_sdk::grpc::stubs::profile::{
    self, cluster_profile_api_client::ClusterProfileApiClient,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio_stream::{StreamExt as _, wrappers::TcpListenerStream};
use tonic::{
    Request, Response, Status,
    codegen::{BoxFuture, Service, http},
    transport::Channel,
};

#[derive(Clone)]
struct Cluster<const PROFILE: bool> {
    upstream: Channel,
    calls: Arc<Mutex<BTreeMap<String, usize>>>,
}
impl tonic::server::NamedService for Cluster<true> {
    const NAME: &'static str = "cluster.profile.v1.ClusterProfileApi";
}
impl tonic::server::NamedService for Cluster<false> {
    const NAME: &'static str = "cluster.cache.v1.ClusterCacheApi";
}
struct Forward {
    upstream: Channel,
    path: http::uri::PathAndQuery,
}
impl tonic::server::UnaryService<Bytes> for Forward {
    type Response = Bytes;
    type Future = BoxFuture<Response<Bytes>, Status>;
    fn call(&mut self, req: Request<Bytes>) -> Self::Future {
        let mut client = tonic::client::Grpc::new(self.upstream.clone());
        let path = self.path.clone();
        Box::pin(async move {
            client
                .ready()
                .await
                .map_err(|_| Status::unavailable("cluster transport"))?;
            client.unary(req, path, Wire).await
        })
    }
}
impl<const PROFILE: bool> Service<http::Request<tonic::body::Body>> for Cluster<PROFILE> {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let path = req
            .uri()
            .path_and_query()
            .cloned()
            .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
        *self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(path.path().to_owned())
            .or_default() += 1;
        let forward = Forward {
            upstream: self.upstream.clone(),
            path,
        };
        Box::pin(async move { Ok(tonic::server::Grpc::new(Wire).unary(forward, req).await) })
    }
}

#[tokio::test]
async fn the_broker_keeps_memory_local_in_a_union_and_uses_only_the_explicit_remote() -> Result<()>
{
    const DESCRIBE: &str = "/cluster.profile.v1.ClusterProfileApi/DescribeProfiles";
    const PUT: &str = "/cluster.cache.v1.ClusterCacheApi/Put";
    let pair = Pair::start().await?;
    let (mut remote, endpoint) = pair.remote_cluster().await?;
    let upstream = tonic::transport::Endpoint::from_shared(endpoint.clone())?
        .connect()
        .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let target = format!("http://{}", listener.local_addr()?);
    let connections = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(Mutex::new(BTreeMap::new()));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let accepted = connections.clone();
    let incoming = TcpListenerStream::new(listener).map(move |socket| {
        if socket.is_ok() {
            accepted.fetch_add(1, Ordering::Relaxed);
        }
        socket
    });
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(Cluster::<true> {
                upstream: upstream.clone(),
                calls: calls.clone(),
            })
            .add_service(Cluster::<false> {
                upstream,
                calls: calls.clone(),
            })
            .serve_with_incoming_shutdown(incoming, async {
                let _shutdown = stopped.await;
            }),
    );
    let mut config = pair.broker_config()?;
    config["gears"]["cluster-sdk"] =
        json!({"clients":{"consumes":{"cluster":{"grpc_endpoint":target}}}});
    pair.write_broker_config(&config)?;
    let mut memory = pair.broker(
        "memory-union",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let (base, page) = pair.authenticated_page(&mut memory).await?;
    assert_eq!(page.status(), 200);
    assert_eq!(
        page.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    let _ = pair.subscription(&base).await?;
    memory.stop()?;
    assert_eq!(connections.load(Ordering::Relaxed), 0);
    assert_eq!(
        *calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        BTreeMap::new()
    );
    {
        let mut profile = ClusterProfileApiClient::connect(target.clone()).await?;
        let mut request = Request::new(profile::DescribeProfilesRequest {
            profiles: vec!["orders".into()],
        });
        request
            .metadata_mut()
            .insert("x-toolkit-internal-token", pair.workload.parse()?);
        let actual = profile.describe_profiles(request).await?.into_inner();
        assert_eq!(
            actual,
            profile::DescribeProfilesResponse {
                generation: 1,
                profiles: vec![profile::ProfileDescriptor {
                    name: "orders".into(),
                    health: profile::ProfileHealth::Serving.into(),
                    cache: Some(profile::CacheDescriptor {
                        provider: "standalone".into(),
                        consistency: profile::WireCacheConsistency::Linearizable.into(),
                        features: Some(profile::WireCacheFeatures {
                            prefix_watch: true,
                            watch: Some(true)
                        })
                    }),
                    leader_election: Some(profile::LeaderElectionDescriptor {
                        provider: "standalone".into(),
                        features: Some(profile::WireLeaderElectionFeatures { linearizable: true })
                    }),
                    lock: Some(profile::LockDescriptor {
                        provider: "standalone".into(),
                        features: Some(profile::WireLockFeatures { linearizable: true })
                    }),
                }]
            }
        );
    }
    assert_eq!(
        *calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        BTreeMap::from([(DESCRIBE.into(), 1)])
    );
    config["gears"]["event-broker"]["config"]["process"]["cluster"] = json!({"mode":"remote"});
    pair.write_broker_config(&config)?;
    let mut broker = pair.broker(
        "remote-explicit",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), 200);
    let ready = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        let probe = pair.client.get(format!("{base}/readyz")).send().await?;
        if probe.status().is_success() {
            break;
        }
        ensure!(
            tokio::time::Instant::now() < ready,
            "live remote did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let _ = pair.subscription(&base).await?;
    broker.stop()?;
    let counted = calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    ensure!(
        counted.get(DESCRIBE).copied().unwrap_or_default() > 1,
        "missing remote descriptor"
    );
    assert_eq!(counted.get(PUT), Some(&2));
    println!(
        "S4 Remote {endpoint}, explicit target {target}, memory 0 connections, remote RPCs {}",
        serde_json::to_string(&counted)?
    );
    let _receiver = stop.send(());
    tokio::time::timeout(Duration::from_secs(8), server).await???;
    let mut absent = pair.broker(
        "remote-unavailable",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let mut observation = ReadinessObservation::new(absent.pid());
    let expected: BTreeSet<_> = ["cache", "leader_election", "lock"]
        .map(|primitive| ("event-broker".to_owned(), primitive.to_owned()))
        .into_iter()
        .collect();
    let deferred = loop {
        let deferred =
            observation.read_deferred(&mut absent, &pair.workload, "initial deferrals")?;
        if !deferred.is_subset(&expected) || deferred == expected {
            break deferred;
        }
        observation.pause().await;
    };
    ensure!(
        deferred == expected,
        "{}",
        observation.describe("unexpected initial deferrals")
    );

    // The cluster deferrals can precede resolution of the application preparation gate.
    let (status, mime, body) = observation.observe_response(
        &pair, &mut absent,
        (503, "application/json", json!({
            "state": "starting", "ready": false, "unresolved_deps": ["application/preparation"]
        })),
    ).await?;
    ensure!(
        (status, mime.as_deref(), body)
            == (
                503,
                Some("application/json"),
                json!({"state":"starting","ready":false})
            ),
        "{}",
        observation.describe("unexpected response")
    );
    let deferred = observation.read_deferred(&mut absent, &pair.workload, "final deferrals")?;
    ensure!(
        deferred == expected,
        "{}",
        observation.describe("unexpected final deferrals")
    );
    println!("confirmed {}", observation.describe("final deferrals"));
    absent.stop()?;
    remote.stop()?;
    pair.finish()
}

struct ReadinessObservation {
    started: tokio::time::Instant,
    deadline: tokio::time::Instant,
    pid: u32,
    http: String,
    attempt: usize,
    response: String,
    deferred: String,
}

impl ReadinessObservation {
    fn new(pid: u32) -> Self {
        let started = tokio::time::Instant::now();
        Self {
            started,
            deadline: started + Duration::from_secs(15),
            pid,
            http: "absent".to_owned(),
            attempt: 0,
            response: "absent".to_owned(),
            deferred: "absent".to_owned(),
        }
    }

    fn describe(&self, stage: &str) -> String {
        format!(
            "remote-unavailable readiness {stage} pid={} http={} attempt={} elapsed={:?} last response {} deferred={}",
            self.pid,
            self.http,
            self.attempt,
            self.started.elapsed(),
            self.response,
            self.deferred
        )
    }

    fn safe(text: String, workload: &str) -> String {
        if text.len() <= 1024
            && [super::cutover::OWNER, super::cutover::UNKNOWN, workload]
                .iter()
                .all(|secret| !text.contains(secret))
        {
            text
        } else {
            "<redacted>".to_owned()
        }
    }

    fn check(&self, absent: &mut Process, stage: &str) -> Result<()> {
        absent
            .require_owned()
            .with_context(|| self.describe(stage))?;
        ensure!(
            tokio::time::Instant::now() < self.deadline,
            "deadline: {}",
            self.describe(stage)
        );
        Ok(())
    }

    async fn pause(&self) {
        let next = tokio::time::Instant::now() + Duration::from_millis(40);
        tokio::time::sleep_until(next.min(self.deadline)).await;
    }

    fn read_deferred(
        &mut self,
        absent: &mut Process,
        workload: &str,
        stage: &str,
    ) -> Result<BTreeSet<(String, String)>> {
        self.check(absent, stage)?;
        let deferred = absent
            .safe_log(workload)
            .with_context(|| self.describe(stage))?
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| {
                event["fields"]["message"] == "cluster resolve: validation deferred to readiness"
            })
            .filter_map(|event| {
                Some((
                    event["fields"]["profile"].as_str()?.to_owned(),
                    event["fields"]["primitive"].as_str()?.to_owned(),
                ))
            })
            .collect::<BTreeSet<_>>();
        self.deferred = Self::safe(format!("{deferred:?}"), workload);
        self.check(absent, stage)?;
        Ok(deferred)
    }

    async fn observe_response(
        &mut self,
        pair: &Pair,
        absent: &mut Process,
        retry: (u16, &str, Value),
    ) -> Result<(u16, Option<String>, Value)> {
        loop {
            self.check(absent, "bound listener")?;
            if let Some(addr) = absent
                .bound_http(&pair.workload)
                .with_context(|| self.describe("bound listener"))?
            {
                self.http = format!("http://{addr}/readyz");
                break;
            }
            self.pause().await;
        }
        loop {
            self.check(absent, "before probe")?;
            self.attempt += 1;
            let receive = async {
                let probe = pair.client.get(&self.http).send().await?;
                let status = probe.status().as_u16();
                let mime = probe
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                ensure!(
                    (status, mime.as_deref()) == (retry.0, Some(retry.1)),
                    "unexpected status={status} mime={} body=absent",
                    Self::safe(format!("{mime:?}"), &pair.workload)
                );
                let body = probe
                    .json::<Value>()
                    .await
                    .ok()
                    .context("status=503 mime=application/json body could not be decoded")?;
                self.response = format!(
                    "status={status} mime=application/json body={}",
                    Self::safe(body.to_string(), &pair.workload)
                );
                Ok((status, mime, body))
            };
            let received = tokio::time::timeout_at(self.deadline, receive).await;
            self.check(absent, "after probe")?;
            let (status, mime, body) = received
                .with_context(|| self.describe("deadline during probe"))?
                .with_context(|| self.describe("probe failed"))?;
            if (status, mime.as_deref(), &body) != (retry.0, Some(retry.1), &retry.2) {
                return Ok((status, mime, body));
            }
            self.pause().await;
        }
    }
}
