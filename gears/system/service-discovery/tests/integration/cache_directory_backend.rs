use std::future::Future;
use std::sync::atomic::Ordering;

use anyhow::{Context, ensure};
use cluster_sdk::ClusterError;
use futures_util::StreamExt;
use testcontainers::runners::AsyncRunner;

use super::tests::CountingCache;
use super::*;

const GRACE: Duration = Duration::from_secs(2);
const LIVE_TTL: Duration = Duration::from_secs(300);
const SHORT_TTL: Duration = Duration::from_secs(2);
const EVENT_TIMEOUT: Duration = Duration::from_secs(20);
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(60);
const PHASE_TIMEOUT: Duration = Duration::from_secs(90);
const GEAR: &str = "integration-gear";
const SERVICE: &str = "integration.v1.Directory";
const EXPIRED_GEAR: &str = "expired-integration-gear";
const EXPIRED_SERVICE: &str = "integration.v1.Expired";

type TestPlugin = (Arc<dyn ClusterCacheBackend>, StoragePluginHandle);

fn finish_fixture(result: anyhow::Result<()>, cleanup: anyhow::Result<()>) -> anyhow::Result<()> {
    match (result, cleanup) {
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("fixture cleanup also failed: {cleanup_error:#}")))
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

struct RegisteredInstances {
    live_id: Uuid,
    live_generation: u64,
    live_revision: u64,
    expired_id: Uuid,
    expired_generation: u64,
    expired_at: SystemTime,
}

fn instance(gear: &str, id: Uuid, service: &str, state: ReportedState) -> Arc<GearInstance> {
    Arc::new(
        GearInstance::new(gear, id)
            .with_grpc_service(service, Endpoint::http("127.0.0.1", 50051))
            .with_reported_state(state),
    )
}

async fn wait_for_change(
    changes: &mut DirectoryChangeStream,
    description: &str,
    predicate: impl Fn(&DirectoryChange) -> bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let change = changes
                .next()
                .await
                .context("directory change feed ended")??;
            if predicate(&change) {
                return Ok(());
            }
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {description}"))?
}

async fn wait_for_record(
    changes: &mut DirectoryChangeStream,
    id: Uuid,
    state: ReportedState,
    revision: u64,
) -> anyhow::Result<()> {
    wait_for_change(changes, "a remote directory update", |change| {
        matches!(
            change,
            DirectoryChange::Upsert(record) | DirectoryChange::Renew(record)
                if record.instance.instance_id == id
                    && record.instance.reported_state == Some(state)
                    && record.instance.grpc_services.contains_key(SERVICE)
                    && record.revision >= revision
        )
    })
    .await
}

async fn verify_value_guarded_cas(
    first: &dyn ClusterCacheBackend,
    second: &dyn ClusterCacheBackend,
) -> anyhow::Result<()> {
    const KEY: &str = "integration/value-fence";
    let old = first
        .put_if_absent(PutRequest {
            key: KEY,
            value: b"old",
            ttl: Ttl::Indefinite,
        })
        .await?
        .context("create original CAS value")?;
    ensure!(
        first.compare_and_delete(KEY, b"old").await?,
        "delete original CAS value"
    );
    let successor = second
        .put_if_absent(PutRequest {
            key: KEY,
            value: b"successor",
            ttl: Ttl::Indefinite,
        })
        .await?
        .context("create successor CAS value")?;
    ensure!(
        old.version == successor.version,
        "test requires a version reset after deletion"
    );
    // A version-only CAS would accept this stale write because the recreated
    // entry reused the old version. The distinct value is what must fence it.
    let stale = first
        .compare_and_swap_value(KEY, b"old", b"stale", Ttl::Indefinite)
        .await;
    ensure!(
        matches!(stale, Err(ClusterError::CasConflict { .. })),
        "a stale value must not overwrite a recreated key: {stale:?}"
    );
    let unchanged = second.get(KEY).await?.context("read recreated CAS value")?;
    ensure!(
        unchanged.value == b"successor",
        "a rejected stale CAS must leave the successor unchanged"
    );

    let (a, b) = tokio::join!(
        first.compare_and_swap_value(KEY, b"successor", b"first", Ttl::Indefinite),
        second.compare_and_swap_value(KEY, b"successor", b"second", Ttl::Indefinite),
    );
    ensure!(
        matches!(
            (&a, &b),
            (Ok(_), Err(ClusterError::CasConflict { .. }))
                | (Err(ClusterError::CasConflict { .. }), Ok(_))
        ),
        "exactly one value-guarded CAS must succeed: {a:?}, {b:?}"
    );
    let current = second.get(KEY).await?.context("read winning CAS value")?;
    ensure!(
        current.value == b"first" || current.value == b"second",
        "the stored CAS value must belong to the winner"
    );
    Ok(())
}

async fn exercise_live_replicas(
    writer_cache: Arc<dyn ClusterCacheBackend>,
    reader_cache: Arc<dyn ClusterCacheBackend>,
) -> anyhow::Result<RegisteredInstances> {
    let counted = Arc::new(CountingCache::new(Arc::clone(&writer_cache)));
    let writer = CacheDirectoryBackend::new(counted.clone(), GRACE)?;
    let reader = CacheDirectoryBackend::new(Arc::clone(&reader_cache), GRACE)?;
    let mut changes = reader.changes().await?;
    let id = Uuid::new_v4();
    let registered = writer
        .register(
            instance(GEAR, id, SERVICE, ReportedState::Starting),
            LIVE_TTL,
            GRACE,
        )
        .await?;
    wait_for_record(
        &mut changes,
        id,
        ReportedState::Starting,
        registered.revision,
    )
    .await?;

    counted.reset();
    let heartbeat = writer
        .heartbeat(GEAR, id, ReportedState::Serving, LIVE_TTL, GRACE)
        .await?
        .context("heartbeat must find the registered instance")?;
    ensure!(
        counted.reads.load(Ordering::SeqCst) == 0,
        "steady-state heartbeat must issue no cache reads"
    );
    ensure!(
        counted.mutations.load(Ordering::SeqCst) == 1,
        "steady-state heartbeat must issue one cache mutation"
    );
    ensure!(
        heartbeat.generation == registered.generation && heartbeat.revision > registered.revision,
        "heartbeat must advance the existing registration"
    );
    wait_for_record(&mut changes, id, ReportedState::Serving, heartbeat.revision).await?;

    for round in 0..2 {
        let service = format!("integration.v1.Contended{round}");
        let contenders = (0..8).map(|contender| {
            let cache = Arc::clone(&writer_cache);
            let service = service.clone();
            async move {
                let backend = CacheDirectoryBackend::new(cache, GRACE)?;
                let gear = format!("claim-{round}-{contender}");
                let result = backend
                    .register(
                        instance(&gear, Uuid::new_v4(), &service, ReportedState::Serving),
                        LIVE_TTL,
                        GRACE,
                    )
                    .await;
                Ok::<_, anyhow::Error>((gear, result))
            }
        });
        let results = futures_util::future::join_all(contenders).await;
        let mut winner = None;
        for result in results {
            let (gear, result) = result?;
            match result {
                Ok(_) => {
                    ensure!(
                        winner.replace(gear).is_none(),
                        "more than one contender registered {service}"
                    );
                }
                Err(DirectoryBackendError::Conflict(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let winner = winner.with_context(|| format!("no contender registered {service}"))?;
        let snapshot = reader.load_all(GRACE).await?;
        let owners: Vec<_> = snapshot
            .iter()
            .filter(|record| record.instance.grpc_services.contains_key(&service))
            .collect();
        ensure!(
            owners.len() == 1 && owners[0].instance.gear == winner,
            "only the successful contender may advertise {service}"
        );
    }

    verify_value_guarded_cas(counted.as_ref(), reader_cache.as_ref()).await?;

    let expired_id = Uuid::new_v4();
    let expired = writer
        .register(
            instance(
                EXPIRED_GEAR,
                expired_id,
                EXPIRED_SERVICE,
                ReportedState::Serving,
            ),
            SHORT_TTL,
            GRACE,
        )
        .await?;
    Ok(RegisteredInstances {
        live_id: id,
        live_generation: registered.generation,
        live_revision: heartbeat.revision,
        expired_id,
        expired_generation: expired.generation,
        expired_at: expired.lease_expires_at,
    })
}

async fn exercise_restarted_replicas(
    restarted_cache: Arc<dyn ClusterCacheBackend>,
    reader_cache: Arc<dyn ClusterCacheBackend>,
    registered: RegisteredInstances,
) -> anyhow::Result<()> {
    let restarted = CacheDirectoryBackend::new(Arc::clone(&restarted_cache), GRACE)?;
    let reader = CacheDirectoryBackend::new(Arc::clone(&reader_cache), GRACE)?;
    let stale_reader = CacheDirectoryBackend::new(reader_cache, GRACE)?;
    let snapshot = restarted.load_all(GRACE).await?;
    let live = snapshot
        .iter()
        .find(|record| record.instance.instance_id == registered.live_id)
        .context("a valid registration must survive restart of every replica")?;
    ensure!(
        live.generation == registered.live_generation
            && live.revision == registered.live_revision
            && live.instance.reported_state == Some(ReportedState::Serving)
            && live.instance.grpc_services.contains_key(SERVICE),
        "the restarted replica must restore the complete live registration"
    );
    ensure!(
        snapshot
            .iter()
            .all(|record| record.instance.instance_id != registered.expired_id),
        "the expired registration must not appear after downtime"
    );
    let reader_snapshot = reader.load_all(GRACE).await?;
    ensure!(
        reader_snapshot
            .iter()
            .any(|record| record.instance.instance_id == registered.live_id),
        "a second restarted replica must independently restore the live registration"
    );
    ensure!(
        restarted
            .heartbeat(
                EXPIRED_GEAR,
                registered.expired_id,
                ReportedState::Serving,
                LIVE_TTL,
                GRACE,
            )
            .await?
            .is_none(),
        "an expired registration must require re-registration"
    );
    ensure!(
        restarted_cache
            .get(&CacheDirectoryBackend::instance_key(
                EXPIRED_GEAR,
                registered.expired_id
            ))
            .await?
            .is_some(),
        "the expired instance key must exist before the janitor runs"
    );
    restarted.janitor(GRACE).await?;
    ensure!(
        restarted_cache
            .get(&CacheDirectoryBackend::instance_key(
                EXPIRED_GEAR,
                registered.expired_id
            ))
            .await?
            .is_none(),
        "the janitor must remove the expired instance key"
    );
    ensure!(
        restarted_cache
            .get(&CacheDirectoryBackend::lease_key(
                EXPIRED_GEAR,
                registered.expired_id
            ))
            .await?
            .is_none(),
        "the expired lease must no longer be present"
    );
    ensure!(
        restarted.claim_owner(EXPIRED_SERVICE).await?.is_none(),
        "the janitor must release the expired service claim"
    );
    let replacement = restarted
        .register(
            instance(
                EXPIRED_GEAR,
                registered.expired_id,
                EXPIRED_SERVICE,
                ReportedState::Serving,
            ),
            LIVE_TTL,
            GRACE,
        )
        .await?;
    ensure!(
        replacement.generation > registered.expired_generation,
        "re-registering an expired instance must use a new generation"
    );

    stale_reader.load_all(GRACE).await?;
    let mut changes = reader.changes().await?;
    let heartbeat = restarted
        .heartbeat(
            GEAR,
            registered.live_id,
            ReportedState::Draining,
            LIVE_TTL,
            GRACE,
        )
        .await?
        .context("restored instance must accept a heartbeat")?;
    wait_for_record(
        &mut changes,
        registered.live_id,
        ReportedState::Draining,
        heartbeat.revision,
    )
    .await?;
    let removed = restarted
        .deregister(GEAR, registered.live_id)
        .await?
        .context("restored instance must be deregistered")?;
    wait_for_change(&mut changes, "remote deregistration", |change| {
        matches!(
            change,
            DirectoryChange::Remove { gear, instance_id, generation }
                if gear == GEAR
                    && *instance_id == registered.live_id
                    && *generation == removed.generation
        )
    })
    .await?;
    ensure!(
        restarted_cache
            .get(&CacheDirectoryBackend::instance_key(
                GEAR,
                registered.live_id
            ))
            .await?
            .is_none(),
        "deregistration must remove the stored instance after a state-changing heartbeat"
    );
    let successor = restarted
        .register(
            instance(GEAR, registered.live_id, SERVICE, ReportedState::Serving),
            LIVE_TTL,
            GRACE,
        )
        .await?;
    ensure!(
        successor.generation > removed.generation,
        "a new registration must advance the generation"
    );
    ensure!(
        stale_reader
            .heartbeat(
                GEAR,
                registered.live_id,
                ReportedState::Serving,
                LIVE_TTL,
                GRACE,
            )
            .await?
            .is_none(),
        "a stale replica must not renew a successor generation"
    );
    Ok(())
}

async fn start_plugin<F, Fut>(make_plugin: &F, description: &str) -> anyhow::Result<TestPlugin>
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<TestPlugin>>,
{
    tokio::time::timeout(PLUGIN_TIMEOUT, make_plugin())
        .await
        .with_context(|| format!("timed out starting {description}"))?
        .with_context(|| format!("start {description}"))
}

async fn run_backend_contract<F, Fut>(make_plugin: F) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<TestPlugin>>,
{
    let (writer_cache, writer_handle) = start_plugin(&make_plugin, "writer plugin").await?;
    let (reader_cache, reader_handle) = match start_plugin(&make_plugin, "reader plugin").await {
        Ok(plugin) => plugin,
        Err(error) => {
            writer_handle.stop().await;
            return Err(error);
        }
    };
    let live = tokio::time::timeout(
        PHASE_TIMEOUT,
        exercise_live_replicas(writer_cache, reader_cache),
    )
    .await
    .context("timed out during two-replica checks")
    .and_then(|result| result);
    writer_handle.stop().await;
    reader_handle.stop().await;

    let registered = live.context("two-replica checks")?;
    let visible_until = registered
        .expired_at
        .checked_add(GRACE)
        .context("expired lease deadline overflow")?;
    if let Ok(remaining) = visible_until.duration_since(SystemTime::now()) {
        tokio::time::sleep(remaining + Duration::from_millis(250)).await;
    }

    let (restarted_cache, restarted_handle) =
        start_plugin(&make_plugin, "restarted writer plugin").await?;
    let (reader_cache, reader_handle) =
        match start_plugin(&make_plugin, "restarted reader plugin").await {
            Ok(plugin) => plugin,
            Err(error) => {
                restarted_handle.stop().await;
                return Err(error);
            }
        };
    let result = tokio::time::timeout(
        PHASE_TIMEOUT,
        exercise_restarted_replicas(restarted_cache, reader_cache, registered),
    )
    .await
    .context("timed out during restart and expiry checks")
    .and_then(|result| result);
    restarted_handle.stop().await;
    reader_handle.stop().await;
    result.context("restart and expiry checks")
}

#[tokio::test]
async fn postgres_shared_directory() -> anyhow::Result<()> {
    let container = test_containers::postgres_named("directory_test")
        .start()
        .await
        .context("start PostgreSQL test container")?;
    let result = async {
        let port = container.get_host_port_ipv4(5432).await?;
        let connection_string =
            format!("postgres://postgres:postgres@127.0.0.1:{port}/directory_test");
        run_backend_contract(|| {
            let connection_string = connection_string.clone();
            async move {
                let config = serde_json::from_value(serde_json::json!({
                    "connection_string": connection_string,
                }))?;
                let handle = postgres_cluster_plugin::PostgresClusterPlugin::builder(config)
                    .build_and_start()
                    .await?;
                Ok((handle.cache(), StoragePluginHandle::Postgres(handle)))
            }
        })
        .await
    }
    .await;
    let cleanup = container.rm().await.context("remove PostgreSQL container");
    finish_fixture(result, cleanup)
}

#[tokio::test]
async fn redis_shared_directory() -> anyhow::Result<()> {
    use testcontainers::ImageExt;
    use testcontainers::core::Mount;

    let container = test_containers::redis()
        .with_cmd([
            "redis-server",
            "--save",
            "",
            "--notify-keyspace-events",
            "Kxe",
            "--appendonly",
            "yes",
            "--appendfsync",
            "always",
        ])
        .with_mount(Mount::tmpfs_mount("/data"))
        .start()
        .await
        .context("start Redis test container")?;
    let result = async {
        let port = container.get_host_port_ipv4(6379).await?;
        let url = format!("redis://127.0.0.1:{port}");
        run_backend_contract(|| {
            let url = url.clone();
            async move {
                let config = serde_json::from_value(serde_json::json!({ "url": url }))?;
                let handle = redis_cluster_plugin::RedisClusterPlugin::builder(config)
                    .build_and_start()
                    .await?;
                Ok((handle.cache(), StoragePluginHandle::Redis(handle)))
            }
        })
        .await
    }
    .await;
    let cleanup = container.rm().await.context("remove Redis container");
    finish_fixture(result, cleanup)
}

#[tokio::test]
async fn k8s_shared_directory() -> anyhow::Result<()> {
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use kube::api::{Api, PostParams};
    use kube::config::{KubeConfigOptions, Kubeconfig};
    use kube::runtime::wait::{await_condition, conditions};
    use kube::{Client, Config, ResourceExt};
    use testcontainers_modules::k3s::KUBE_SECURE_PORT;

    let kubeconfig_dir = tempfile::tempdir().context("create k3s kubeconfig directory")?;
    let container = test_containers::k3s(kubeconfig_dir.path())
        .start()
        .await
        .context("start privileged k3s test container")?;
    let result = async {
        let port = container.get_host_port_ipv4(KUBE_SECURE_PORT).await?;
        let raw = container.image().read_kube_config()?;
        let mut kubeconfig = Kubeconfig::from_yaml(&raw)?;
        for cluster in &mut kubeconfig.clusters {
            if let Some(server) = cluster
                .cluster
                .as_mut()
                .and_then(|item| item.server.as_mut())
            {
                *server = format!("https://127.0.0.1:{port}");
            }
        }
        let client_config =
            Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
        let client = Client::try_from(client_config.clone())?;
        let crd: CustomResourceDefinition = serde_saphyr::from_str(include_str!(
            "../../../cluster/plugins/k8s-cluster-plugin/deploy/crd.yaml"
        ))?;
        let name = crd.name_any();
        let api: Api<CustomResourceDefinition> = Api::all(client.clone());
        api.create(&PostParams::default(), &crd).await?;
        tokio::time::timeout(
            Duration::from_secs(30),
            await_condition(api, &name, conditions::is_crd_established()),
        )
        .await
        .context("timed out waiting for cache CRD")??;
        run_backend_contract(|| {
            let client_config = client_config.clone();
            async move {
                let client = Client::try_from(client_config)?;
                let config = serde_json::from_value(serde_json::json!({
                    "namespace": "default",
                    "identity": format!("directory-{}", Uuid::new_v4().simple()),
                }))?;
                let handle = k8s_cluster_plugin::K8sCachePlugin::builder(config)
                    .with_client(client)
                    .build_and_start()
                    .await?;
                Ok((handle.cache(), StoragePluginHandle::K8s(handle)))
            }
        })
        .await
    }
    .await;
    let cleanup = container.rm().await.context("remove k3s container");
    finish_fixture(result, cleanup)
}
