//! The remote SDK contributor retains its last good descriptors through transport failures.

use std::collections::VecDeque;
use std::fmt;
use std::future::poll_fn;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use cluster_sdk::cache::CacheFeatures;
use cluster_sdk::dto::{
    CacheDescriptor, DescribeProfilesResponse, LeaderElectionDescriptor, LockDescriptor,
    ProfileDescriptor, ProfileHealth, WireCacheConsistency, WireLeaderElectionFeatures,
    WireLockFeatures,
};
use cluster_sdk::grpc::stubs::profile as wire;
use cluster_sdk::grpc::stubs::profile::cluster_profile_api_server::{
    ClusterProfileApi, ClusterProfileApiServer,
};
use cluster_sdk::requirements::{DESCRIPTOR_REFRESH_INTERVAL, requirements};
use cluster_sdk::{ClusterClient, RemoteClusterClient, cluster_readiness};
use event_broker::domain::EventBrokerCluster;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{advance, pause, resume, timeout};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use toolkit::{ClientHub, Healthcheck, HealthcheckStatus, RestHealthcheckRegistry};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

const PROFILE: &str = "event-broker";
const PHASE_LIMIT: Duration = Duration::from_secs(8);
const HOST_DEADLINE: Duration = Duration::from_secs(3);
// The SDK's two-second bind deadline applies only to initial resolution.
const PENDING_WINDOW: Duration = Duration::from_millis(2_100);

fn descriptor(health: ProfileHealth) -> ProfileDescriptor {
    ProfileDescriptor {
        name: PROFILE.to_owned(),
        cache: CacheDescriptor {
            consistency: WireCacheConsistency::Linearizable,
            features: CacheFeatures::new(true).into(),
            provider: "fixture-cache".to_owned(),
        },
        lock: LockDescriptor {
            features: WireLockFeatures { linearizable: true },
            provider: "fixture-lock".to_owned(),
        },
        leader_election: LeaderElectionDescriptor {
            features: WireLeaderElectionFeatures { linearizable: true },
            provider: "fixture-election".to_owned(),
        },
        health,
    }
}

enum Reply {
    Immediate(DescribeProfilesResponse),
    Held {
        response: DescribeProfilesResponse,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
}

struct ReplyGate {
    entered: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}

impl ReplyGate {
    fn hold(response: DescribeProfilesResponse) -> (Self, Reply) {
        let (entered, entry) = oneshot::channel();
        let (open, release) = oneshot::channel();
        (
            Self {
                entered: entry,
                release: open,
            },
            Reply::Held {
                response,
                entered,
                release,
            },
        )
    }
}

type Requests = Arc<Mutex<Vec<(Vec<String>, u64)>>>;

#[derive(Clone)]
struct ProfileService {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Requests,
}

#[tonic::async_trait]
impl ClusterProfileApi for ProfileService {
    async fn describe_profiles(
        &self,
        request: Request<wire::DescribeProfilesRequest>,
    ) -> Result<Response<wire::DescribeProfilesResponse>, Status> {
        let reply = self
            .replies
            .lock()
            .unwrap_or_else(|error| panic!("reply script: {error}"))
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected RPC"));
        let response = match &reply {
            Reply::Immediate(response) | Reply::Held { response, .. } => response,
        };
        let profiles = request.into_inner().profiles;
        eprintln!(
            "DescribeProfiles: profiles={}, generation={}",
            serde_json::json!(profiles),
            response.generation
        );
        self.requests
            .lock()
            .unwrap_or_else(|error| panic!("request journal: {error}"))
            .push((profiles, response.generation));
        let response = match reply {
            Reply::Immediate(response) => response,
            Reply::Held {
                response,
                entered,
                release,
            } => {
                entered
                    .send(())
                    .map_err(|()| Status::cancelled("probe no longer waits for handler entry"))?;
                timeout(PHASE_LIMIT, release)
                    .await
                    .map_err(|_| Status::deadline_exceeded("fixture gate timed out"))?
                    .map_err(|_| Status::cancelled("fixture gate closed"))?;
                response
            }
        };
        Ok(Response::new(response.into()))
    }
}

struct ProfileServer {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl ProfileServer {
    async fn start(
        addr: SocketAddr,
        requests: Requests,
        replies: impl Into<VecDeque<Reply>>,
    ) -> Self {
        let listener = timeout(PHASE_LIMIT, TcpListener::bind(addr))
            .await
            .unwrap_or_else(|error| panic!("listen completes: {error}"))
            .unwrap_or_else(|error| panic!("test listener binds: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("test address: {error}"));
        let service = ProfileService {
            replies: Arc::new(Mutex::new(replies.into())),
            requests,
        };
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(ClusterProfileApiServer::new(service))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    shutdown.clone().cancelled_owned(),
                ),
        );
        Self {
            addr,
            shutdown,
            task,
        }
    }

    async fn stop(mut self) {
        self.shutdown.cancel();
        timeout(PHASE_LIMIT, &mut self.task)
            .await
            .unwrap_or_else(|error| panic!("profile server shuts down within the phase: {error}"))
            .unwrap_or_else(|error| panic!("profile server task joins: {error}"))
            .unwrap_or_else(|error| panic!("profile server shuts down cleanly: {error}"));
    }
}

impl Drop for ProfileServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Default)]
struct RefreshFailures(Arc<Mutex<Vec<String>>>);

#[derive(Default)]
struct RefreshEvent {
    message: String,
    error: String,
}

impl Visit for RefreshEvent {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "error" => self.error = format!("{value:?}"),
            _ => {}
        }
    }
}

impl Subscriber for RefreshFailures {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event()
            && *metadata.level() == Level::DEBUG
            && metadata.target() == "cluster_sdk::requirements"
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        unreachable!("the subscriber enables only refresh events")
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = RefreshEvent::default();
        event.record(&mut fields);
        if fields.message == "cluster readiness: descriptor refresh failed" {
            self.0
                .lock()
                .unwrap_or_else(|error| panic!("refresh journal: {error}"))
                .push(fields.error);
        }
    }
}

async fn make_refresh_due() {
    // Advance the SDK's refresh clock between network operations.
    pause();
    advance(DESCRIPTOR_REFRESH_INTERVAL).await;
    resume();
}

async fn probe(
    check: &dyn Healthcheck,
    phase: &str,
) -> (HealthcheckStatus, Option<String>, Option<String>) {
    let result = timeout(PHASE_LIMIT, check.check())
        .await
        .unwrap_or_else(|error| panic!("readiness probe completes within the phase: {error}"));
    eprintln!("{phase}: {}", serde_json::json!(result));
    (result.status, result.message, result.code)
}

async fn cached_descriptor(client: &RemoteClusterClient) -> ProfileDescriptor {
    let descriptor = timeout(PHASE_LIMIT, client.descriptor(PROFILE))
        .await
        .unwrap_or_else(|error| {
            panic!("client descriptor read completes within the phase: {error}")
        })
        .unwrap_or_else(|error| panic!("the client retains the profile: {error}"));
    eprintln!("cached descriptor: {}", serde_json::json!(descriptor));
    descriptor
}

#[tokio::test]
async fn a_completed_descriptor_bind_survives_refused_and_hung_refreshes_until_recovery() {
    let failures = RefreshFailures::default();
    tracing::subscriber::set_global_default(failures.clone()).expect("isolated test subscriber");
    let requests: Requests = Arc::default();
    let serving = descriptor(ProfileHealth::Serving);
    let degraded = descriptor(ProfileHealth::Degraded);
    let initial = DescribeProfilesResponse {
        profiles: vec![serving.clone()],
        generation: 1,
    };
    let server = ProfileServer::start(
        (Ipv4Addr::LOCALHOST, 0).into(),
        requests.clone(),
        [
            Reply::Immediate(initial.clone()),
            Reply::Immediate(initial.clone()),
        ],
    )
    .await;
    let addr = server.addr;
    let hub = Arc::new(ClientHub::new());
    let client = Arc::new(
        RemoteClusterClient::connect_lazy(&format!("http://{addr}"), None)
            .expect("loopback endpoint"),
    );
    hub.register_remote_proxy::<dyn ClusterClient>(client.clone());
    assert_eq!(requirements().recorded_profiles(), Vec::<&str>::new());
    assert!(requests.lock().expect("request journal").is_empty());
    EventBrokerCluster::resolve(&hub)
        .await
        .expect("the broker's three primitives bind");
    assert_eq!(
        (
            requirements().recorded_count(),
            requirements().recorded_profiles()
        ),
        (3, vec![PROFILE])
    );
    let expected = [1, 1, 1, 1, 2, 3].map(|generation| (Vec::<String>::new(), generation));
    assert_eq!(*requests.lock().expect("request journal"), expected[..1]);
    assert_eq!(cached_descriptor(&client).await, serving);
    assert_eq!(*requests.lock().expect("request journal"), expected[..1]);

    let check = cluster_readiness(hub);
    assert_eq!(check.name(), "cluster-requirements");
    let healthy = (HealthcheckStatus::Healthy, None, None);
    assert_eq!(
        probe(check.as_ref(), "initial forced refresh").await,
        healthy
    );
    assert_eq!(*requests.lock().expect("request journal"), expected[..2]);
    assert_eq!(probe(check.as_ref(), "cached probe").await, healthy);
    assert_eq!(*requests.lock().expect("request journal"), expected[..2]);
    assert!(failures.0.lock().expect("refresh journal").is_empty());

    server.stop().await;
    let refusal = timeout(PHASE_LIMIT, TcpStream::connect(addr))
        .await
        .expect("loopback connection attempt completes")
        .expect_err("the stopped endpoint refuses connections");
    assert_eq!(refusal.kind(), ErrorKind::ConnectionRefused);
    eprintln!("stopped loopback endpoint refuses the connection");
    make_refresh_due().await;
    assert_eq!(probe(check.as_ref(), "refused refresh").await, healthy);
    assert_eq!(cached_descriptor(&client).await, serving);
    assert_eq!(*requests.lock().expect("request journal"), expected[..2]);
    {
        let errors = failures.0.lock().expect("refresh journal");
        let [error] = errors.as_slice() else {
            panic!("one attempted refresh failed: {errors:?}")
        };
        assert!(error.contains("cluster unreachable:"), "{error}");
        eprintln!("refused attempt: {error}");
    }

    let (direct, direct_reply) = ReplyGate::hold(initial.clone());
    let (host, host_reply) = ReplyGate::hold(initial);
    let server = ProfileServer::start(
        addr,
        requests.clone(),
        [
            direct_reply,
            host_reply,
            Reply::Immediate(DescribeProfilesResponse {
                profiles: vec![degraded.clone()],
                generation: 2,
            }),
            Reply::Immediate(DescribeProfilesResponse {
                profiles: vec![serving.clone()],
                generation: 3,
            }),
        ],
    )
    .await;
    make_refresh_due().await;
    let pending = probe(check.as_ref(), "released refresh");
    tokio::pin!(pending);
    tokio::select! {
        entered = timeout(PHASE_LIMIT, direct.entered) => {
            entered.expect("remote refresh reaches the handler").expect("handler enters");
        }
        result = &mut pending => panic!("refresh finished before handler entry: {result:?}"),
    }

    pause();
    advance(PENDING_WINDOW).await;
    let state = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await;
    assert!(
        state.is_pending(),
        "the admitted refresh finished before release: {state:?}"
    );
    eprintln!(
        "admitted refresh stays pending after {} ms of SDK time",
        PENDING_WINDOW.as_millis()
    );
    resume();
    tokio::select! {
        result = probe(check.as_ref(), "concurrent cached probe") => assert_eq!(result, healthy),
        result = &mut pending => panic!("held refresh finished during concurrent probe: {result:?}"),
    }
    assert_eq!(*requests.lock().expect("request journal"), expected[..3]);
    direct
        .release
        .send(())
        .expect("the first refresh still awaits the held response");
    assert_eq!(pending.await, healthy);

    make_refresh_due().await;
    let registry = RestHealthcheckRegistry::new();
    registry.register(PROFILE, check.clone());
    // The host measures latency with std::time::Instant. Its timeout runs in real time.
    let start = Instant::now();
    let report = tokio::spawn(async move { registry.report(HOST_DEADLINE).await });
    timeout(PHASE_LIMIT, host.entered)
        .await
        .expect("host check reaches the handler")
        .expect("handler enters");
    let report = timeout(PHASE_LIMIT, report)
        .await
        .expect("host deadline bounds the report")
        .expect("host report joins");
    eprintln!("host report: {}", serde_json::json!(report));
    let [component] = report.components.as_slice() else {
        panic!("one component: {report:?}")
    };
    assert_eq!(
        (report.status, report.is_ready()),
        (HealthcheckStatus::Unhealthy, false)
    );
    assert_eq!(
        (
            component.gear.as_str(),
            component.check.as_str(),
            component.status,
            component.message.as_deref(),
            component.code.as_deref()
        ),
        (
            PROFILE,
            "cluster-requirements",
            HealthcheckStatus::Unhealthy,
            Some("health check timed out"),
            None
        )
    );
    let latency = Duration::from_millis(component.latency_ms);
    assert!(
        (HOST_DEADLINE..=start.elapsed()).contains(&latency),
        "{report:?}"
    );
    assert_eq!(
        probe(check.as_ref(), "cached probe after host timeout").await,
        healthy
    );
    assert_eq!(*requests.lock().expect("request journal"), expected[..4]);
    drop(host.release);

    make_refresh_due().await;
    assert_eq!(
        probe(check.as_ref(), "degraded refresh").await,
        (
            HealthcheckStatus::Unhealthy,
            Some("profile `event-broker` is degraded server-side".to_owned()),
            Some("cluster-profile-degraded".to_owned())
        )
    );
    assert_eq!(cached_descriptor(&client).await, degraded);
    assert_eq!(*requests.lock().expect("request journal"), expected[..5]);

    make_refresh_due().await;
    assert_eq!(probe(check.as_ref(), "serving refresh").await, healthy);
    assert_eq!(cached_descriptor(&client).await, serving);
    assert_eq!(*requests.lock().expect("request journal"), expected);
    assert_eq!(failures.0.lock().expect("refresh journal").len(), 1);
    server.stop().await;
}
