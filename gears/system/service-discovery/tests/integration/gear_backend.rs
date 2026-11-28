use super::*;

use crate::cache_directory_backend::CacheDirectoryBackend;
use cluster_sdk::cache::ClusterCacheBackend;
use testcontainers::runners::AsyncRunner;

struct DisconnectFirstFeedBackend {
    inner: Arc<dyn DirectoryBackend>,
    first_feed_started: tokio::sync::watch::Sender<bool>,
    close_first_feed: Arc<tokio::sync::Notify>,
    subscriptions: AtomicUsize,
}

#[async_trait::async_trait]
impl DirectoryBackend for DisconnectFirstFeedBackend {
    async fn register(
        &self,
        instance: Arc<GearInstance>,
        ttl: Duration,
        grace: Duration,
    ) -> Result<DirectoryRecord, DirectoryBackendError> {
        self.inner.register(instance, ttl, grace).await
    }

    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: Uuid,
        reported: ReportedState,
        ttl: Duration,
        grace: Duration,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        self.inner
            .heartbeat(gear, instance_id, reported, ttl, grace)
            .await
    }

    async fn deregister(
        &self,
        gear: &str,
        instance_id: Uuid,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        self.inner.deregister(gear, instance_id).await
    }

    async fn load_all(
        &self,
        grace: Duration,
    ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
        self.inner.load_all(grace).await
    }

    async fn janitor(&self, grace: Duration) -> Result<(), DirectoryBackendError> {
        self.inner.janitor(grace).await
    }

    async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
        if self.subscriptions.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first_feed_started.send_replace(true);
            let close = Arc::clone(&self.close_first_feed);
            return Ok(Box::pin(futures_util::stream::once(async move {
                close.notified().await;
                Err(DirectoryBackendError::Unavailable(anyhow::anyhow!(
                    "test simulated a lost directory change feed"
                )))
            })));
        }
        self.inner.changes().await
    }
}

async fn init_with_backend(
    backend: Arc<dyn DirectoryBackend>,
    config: serde_json::Value,
) -> anyhow::Result<(ServiceDiscoveryGear, Arc<GearManager>, GearCtx)> {
    let mut wrapped = serde_json::Map::new();
    wrapped.insert("config".to_owned(), config);
    let ctx = GearCtx::new(
        "service-discovery",
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider(serde_json::Value::Object(wrapped))),
        Arc::new(ClientHub::default()),
        CancellationToken::new(),
    );
    let gear = ServiceDiscoveryGear::default();
    let manager = Arc::new(GearManager::new());
    gear.gear_manager
        .set(Arc::clone(&manager))
        .map_err(|_| anyhow::anyhow!("GearManager already set"))?;
    gear.install_directory_backend(backend)?;
    toolkit::Gear::init(&gear, &ctx).await?;
    Ok((gear, manager, ctx))
}

async fn verify_postgres_outage_recovery(
    container: &testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
    container_paused: &mut bool,
    writer_client: &dyn DirectoryClient,
    reader_manager: &GearManager,
    reader_readiness: &dyn Healthcheck,
    id: Uuid,
) -> anyhow::Result<()> {
    use toolkit::HealthcheckStatus;

    let docker = testcontainers::bollard::Docker::connect_with_local_defaults()
        .context("connect to Docker to inspect the PostgreSQL test container")?;
    let state = docker
        .inspect_container(container.id(), None)
        .await
        .context("inspect PostgreSQL test container before pausing")?
        .state
        .context("PostgreSQL test container has no state")?;
    if !state.paused.unwrap_or(false) {
        container
            .pause()
            .await
            .context("pause PostgreSQL container")?;
    }
    *container_paused = true;
    let outage = tokio::time::timeout(
        Duration::from_secs(2),
        writer_client.heartbeat(
            "replicated-gear",
            &id.to_string(),
            Some(cf_system_sdks::directory::ReportedState::Serving),
        ),
    )
    .await;
    let reader_keeps_last_snapshot = reader_manager
        .instances_of("replicated-gear")
        .iter()
        .any(|instance| instance.instance_id == id);
    container
        .unpause()
        .await
        .context("resume PostgreSQL container")?;
    *container_paused = false;
    anyhow::ensure!(
        !matches!(outage, Ok(Ok(()))),
        "a heartbeat must not report success while PostgreSQL is unavailable"
    );
    anyhow::ensure!(
        reader_keeps_last_snapshot
            && reader_readiness.check().await.status == HealthcheckStatus::Healthy,
        "a backend outage must not remove the last snapshot or fail readiness"
    );

    tokio::time::timeout(
        Duration::from_secs(30),
        writer_client.heartbeat(
            "replicated-gear",
            &id.to_string(),
            Some(cf_system_sdks::directory::ReportedState::Draining),
        ),
    )
    .await
    .context("writer heartbeat did not recover after PostgreSQL resumed")??;
    tokio::time::timeout(Duration::from_secs(15), async {
        while reader_manager
            .instances_of("replicated-gear")
            .iter()
            .find(|instance| instance.instance_id == id)
            .is_none_or(|instance| instance.state() != toolkit::runtime::InstanceState::Draining)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("reader feed did not recover after PostgreSQL resumed")?;
    Ok(())
}

async fn verify_scheduled_janitor(
    writer_gear: &ServiceDiscoveryGear,
    cache: Arc<dyn ClusterCacheBackend>,
) -> anyhow::Result<()> {
    let expired_id = Uuid::new_v4();
    writer_gear
        .directory_api
        .get()
        .context("writer DirectoryClient must remain initialized")?
        .register_instance(
            cf_system_sdks::directory::RegisterInstanceInfo::new(
                "janitor-gear",
                expired_id.to_string(),
            )
            .with_reported_state(cf_system_sdks::directory::ReportedState::Serving),
        )
        .await?;
    let suffix = expired_id.simple().to_string();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let keys = cache.scan_prefix("directory/inst/").await?;
            if keys.iter().any(|key| key.ends_with(&suffix)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        loop {
            let keys = cache.scan_prefix("directory/inst/").await?;
            if !keys.iter().any(|key| key.ends_with(&suffix)) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("gear-scheduled janitor did not remove the expired registration")??;
    Ok(())
}

#[tokio::test]
async fn real_postgres_gears_recover_after_a_change_feed_disconnect() -> anyhow::Result<()> {
    use toolkit::HealthcheckStatus;

    let container = test_containers::postgres_named("directory_gear_test")
        .start()
        .await
        .context("start PostgreSQL test container")?;
    let mut handles: Vec<postgres_cluster_plugin::PostgresClusterHandle> = Vec::new();
    let mut container_paused = false;
    let writer_cancel = CancellationToken::new();
    let reader_cancel = CancellationToken::new();

    let result = async {
        let port = container.get_host_port_ipv4(5432).await?;
        let connection_string =
            format!("postgres://postgres:postgres@127.0.0.1:{port}/directory_gear_test");
        let writer_handle = postgres_cluster_plugin::PostgresClusterPlugin::builder(
            serde_json::from_value(serde_json::json!({
                "connection_string": connection_string,
            }))?,
        )
        .build_and_start()
        .await
        .context("start writer PostgreSQL cache plugin")?;
        let writer_cache = writer_handle.cache();
        let writer_backend = Arc::new(
            CacheDirectoryBackend::new(writer_cache.clone(), Duration::from_secs(1))
                .map_err(anyhow::Error::new)?,
        );
        handles.push(writer_handle);

        let reader_handle = postgres_cluster_plugin::PostgresClusterPlugin::builder(
            serde_json::from_value(serde_json::json!({
                "connection_string": connection_string,
            }))?,
        )
        .build_and_start()
        .await
        .context("start reader PostgreSQL cache plugin")?;
        let reader_backend = Arc::new(
            CacheDirectoryBackend::new(reader_handle.cache(), Duration::from_secs(1))
                .map_err(anyhow::Error::new)?,
        );
        let (feed_started_tx, mut feed_started_rx) = tokio::sync::watch::channel(false);
        let close_first_feed = Arc::new(tokio::sync::Notify::new());
        let interrupted_reader = Arc::new(DisconnectFirstFeedBackend {
            inner: reader_backend,
            first_feed_started: feed_started_tx,
            close_first_feed: Arc::clone(&close_first_feed),
            subscriptions: AtomicUsize::new(0),
        });
        handles.push(reader_handle);

        let gear_config = serde_json::json!({
            "lease": { "ttl": "3s", "grace": "1s" },
        });
        let (writer_gear, _writer_manager, writer_ctx) =
            init_with_backend(writer_backend.clone(), gear_config.clone()).await?;
        writer_gear.set_backend_janitor_interval_for_test(Duration::from_millis(50))?;
        let (reader_gear, reader_manager, reader_ctx) =
            init_with_backend(interrupted_reader.clone(), gear_config).await?;
        let writer_readiness = RestApiCapability::healthcheck(&writer_gear, &writer_ctx)
            .context("writer gear must expose backend readiness")?;
        let reader_readiness = RestApiCapability::healthcheck(&reader_gear, &reader_ctx)
            .context("reader gear must expose backend readiness")?;

        RunnableCapability::start(&writer_gear, writer_cancel.clone()).await?;
        RunnableCapability::start(&reader_gear, reader_cancel.clone()).await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            while writer_readiness.check().await.status != HealthcheckStatus::Healthy
                || reader_readiness.check().await.status != HealthcheckStatus::Healthy
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("both service-discovery gears must load their initial snapshots")?;
        tokio::time::timeout(Duration::from_secs(5), feed_started_rx.changed())
            .await
            .context("reader change feed did not start")??;

        let id = Uuid::new_v4();
        writer_gear
            .directory_api
            .get()
            .context("writer DirectoryClient must be initialized")?
            .register_instance(
                cf_system_sdks::directory::RegisterInstanceInfo::new(
                    "replicated-gear",
                    id.to_string(),
                )
                .with_reported_state(cf_system_sdks::directory::ReportedState::Serving),
            )
            .await?;
        anyhow::ensure!(
            reader_manager.instances_of("replicated-gear").is_empty(),
            "the reader must not receive the write through its disconnected feed"
        );
        close_first_feed.notify_one();
        tokio::time::timeout(Duration::from_secs(30), async {
            while !reader_manager
                .instances_of("replicated-gear")
                .iter()
                .any(|instance| instance.instance_id == id)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("reader must recover the missed write from a resubscription snapshot")?;
        anyhow::ensure!(
            interrupted_reader.subscriptions.load(Ordering::SeqCst) >= 2,
            "reader must establish a new feed after the disconnect"
        );

        let writer_client = writer_gear
            .directory_api
            .get()
            .context("writer DirectoryClient must remain initialized")?;
        verify_postgres_outage_recovery(
            &container,
            &mut container_paused,
            writer_client.as_ref(),
            &reader_manager,
            reader_readiness.as_ref(),
            id,
        )
        .await?;
        verify_scheduled_janitor(&writer_gear, writer_cache).await?;
        Ok(())
    }
    .await;

    writer_cancel.cancel();
    reader_cancel.cancel();
    let resume = if container_paused {
        container
            .unpause()
            .await
            .context("resume PostgreSQL container during cleanup")
    } else {
        Ok(())
    };
    for handle in handles {
        handle.stop().await;
    }
    let remove = container.rm().await.context("remove PostgreSQL container");
    let cleanup = match (resume, remove) {
        (Err(error), Err(remove_error)) => {
            Err(error.context(format!("container removal also failed: {remove_error:#}")))
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    };
    match (result, cleanup) {
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("fixture cleanup also failed: {cleanup_error:#}")))
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
