use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(any(
    feature = "storage-k8s",
    feature = "storage-postgres",
    feature = "storage-redis"
))]
use anyhow::Context;
use async_trait::async_trait;
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheWatchEvent, ClusterCacheBackend,
    PollingPrefixWatch, PutRequest, Ttl,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use toolkit::runtime::{Endpoint, GearInstance, GrpcServiceNameConflict, ReportedState};
#[cfg(any(
    feature = "storage-k8s",
    feature = "storage-postgres",
    feature = "storage-redis"
))]
use toolkit::var_expand::ExpandVars;
use uuid::Uuid;

use crate::config::{StorageBackend, StorageConfig};
use crate::directory_backend::{
    DirectoryBackend, DirectoryBackendError, DirectoryChange, DirectoryChangeStream,
    DirectoryRecord,
};

/// Owns the selected shared storage plugin.
pub enum StoragePluginHandle {
    /// Kubernetes cache plugin.
    #[cfg(feature = "storage-k8s")]
    K8s(k8s_cluster_plugin::K8sCacheHandle),
    /// PostgreSQL cache plugin.
    #[cfg(feature = "storage-postgres")]
    Postgres(postgres_cluster_plugin::PostgresClusterHandle),
    /// Redis cache plugin.
    #[cfg(feature = "storage-redis")]
    Redis(redis_cluster_plugin::RedisClusterHandle),
}

impl StoragePluginHandle {
    /// Stops the selected storage plugin.
    pub async fn stop(self) {
        match self {
            #[cfg(feature = "storage-k8s")]
            Self::K8s(handle) => handle.stop().await,
            #[cfg(feature = "storage-postgres")]
            Self::Postgres(handle) => handle.stop().await,
            #[cfg(feature = "storage-redis")]
            Self::Redis(handle) => handle.stop().await,
        }
    }
}

/// Builds the configured shared backend, or returns `None` for in-memory storage.
///
/// # Errors
/// Returns an error if the backend is unavailable or its configuration or startup fails.
pub async fn build_selected_backend(
    config: StorageConfig,
    grace: Duration,
) -> anyhow::Result<Option<(Arc<dyn DirectoryBackend>, StoragePluginHandle)>> {
    #[cfg(not(any(
        feature = "storage-k8s",
        feature = "storage-postgres",
        feature = "storage-redis"
    )))]
    let _ = (&config.k8s, &config.postgres, &config.redis, grace);
    match config.backend {
        StorageBackend::Memory => Ok(None),
        StorageBackend::K8s => {
            #[cfg(feature = "storage-k8s")]
            {
                let value = config
                    .k8s
                    .ok_or_else(|| anyhow::anyhow!("storage.k8s is required for backend: k8s"))?;
                let mut plugin_config: k8s_cluster_plugin::K8sCacheConfig =
                    serde_json::from_value(value).context("invalid storage.k8s configuration")?;
                plugin_config
                    .expand_vars()
                    .context("failed to expand storage.k8s environment variables")?;
                let handle = k8s_cluster_plugin::K8sCachePlugin::builder(plugin_config)
                    .build_and_start()
                    .await
                    .context("failed to start Kubernetes cache plugin")?;
                let backend = Arc::new(
                    CacheDirectoryBackend::new(handle.cache(), grace)
                        .map_err(anyhow::Error::new)?,
                );
                Ok(Some((backend, StoragePluginHandle::K8s(handle))))
            }
            #[cfg(not(feature = "storage-k8s"))]
            {
                let _ = config;
                anyhow::bail!("storage backend 'k8s' is unavailable; enable storage-k8s")
            }
        }
        StorageBackend::Postgres => {
            #[cfg(feature = "storage-postgres")]
            {
                let value = config.postgres.ok_or_else(|| {
                    anyhow::anyhow!("storage.postgres is required for backend: postgres")
                })?;
                let mut plugin_config: postgres_cluster_plugin::PostgresClusterConfig =
                    serde_json::from_value(value)
                        .context("invalid storage.postgres configuration")?;
                plugin_config
                    .expand_vars()
                    .context("failed to expand storage.postgres environment variables")?;
                let handle = postgres_cluster_plugin::PostgresClusterPlugin::builder(plugin_config)
                    .build_and_start()
                    .await
                    .context("failed to start Postgres cache plugin")?;
                let backend = Arc::new(
                    CacheDirectoryBackend::new(handle.cache(), grace)
                        .map_err(anyhow::Error::new)?,
                );
                Ok(Some((backend, StoragePluginHandle::Postgres(handle))))
            }
            #[cfg(not(feature = "storage-postgres"))]
            {
                let _ = config;
                anyhow::bail!("storage backend 'postgres' is unavailable; enable storage-postgres")
            }
        }
        StorageBackend::Redis => {
            #[cfg(feature = "storage-redis")]
            {
                let value = config.redis.ok_or_else(|| {
                    anyhow::anyhow!("storage.redis is required for backend: redis")
                })?;
                let mut plugin_config: redis_cluster_plugin::RedisClusterConfig =
                    serde_json::from_value(value).context("invalid storage.redis configuration")?;
                plugin_config
                    .expand_vars()
                    .context("failed to expand storage.redis environment variables")?;
                let handle = redis_cluster_plugin::RedisClusterPlugin::builder(plugin_config)
                    .build_and_start()
                    .await
                    .context("failed to start Redis cache plugin")?;
                let backend = Arc::new(
                    CacheDirectoryBackend::new(handle.cache(), grace)
                        .map_err(anyhow::Error::new)?,
                );
                Ok(Some((backend, StoragePluginHandle::Redis(handle))))
            }
            #[cfg(not(feature = "storage-redis"))]
            {
                let _ = config;
                anyhow::bail!("storage backend 'redis' is unavailable; enable storage-redis")
            }
        }
    }
}

const PREFIX: &str = "directory/";
const INSTANCE_PREFIX: &str = "directory/inst/";
const LEASE_PREFIX: &str = "directory/lease/";
const GENERATION_SEQUENCE_PREFIX: &str = "directory/generation-sequence/";
const CLAIM_PREFIX: &str = "directory/claim/";
const PREFIX_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_CAS_RETRIES: usize = 16;
const CLAIM_RESERVATION_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredInstance {
    gear: String,
    instance_id: Uuid,
    control: Option<String>,
    grpc_services: BTreeMap<String, String>,
    version: Option<String>,
    rest_endpoint: Option<String>,
    openapi_spec_hash: Option<String>,
    labels: BTreeMap<String, String>,
    reported_state: ReportedValue,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportedValue {
    Starting,
    Serving,
    Draining,
}

impl From<ReportedState> for ReportedValue {
    fn from(value: ReportedState) -> Self {
        match value {
            ReportedState::Starting => Self::Starting,
            ReportedState::Serving => Self::Serving,
            ReportedState::Draining => Self::Draining,
        }
    }
}

impl From<ReportedValue> for ReportedState {
    fn from(value: ReportedValue) -> Self {
        match value {
            ReportedValue::Starting => Self::Starting,
            ReportedValue::Serving => Self::Serving,
            ReportedValue::Draining => Self::Draining,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct StoredLease {
    generation: u64,
    revision: u64,
    lease_expires_at_ms: u64,
    reported_state: ReportedValue,
}

struct LeaseWriteOptions {
    preserve_existing: bool,
    ttl: Duration,
    grace: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum StoredClaim {
    Stable {
        owner: String,
    },
    Pending {
        owner: String,
        previous_owner: Option<String>,
        token: Uuid,
        started_at_ms: u64,
    },
}

type InstanceKey = (String, Uuid);
type LeaseVersions = Arc<Mutex<HashMap<InstanceKey, CacheEntry>>>;
type KnownInstances = Arc<Mutex<HashMap<InstanceKey, StoredInstance>>>;

#[derive(Clone)]
struct ClaimReservation {
    key: String,
    owner: String,
    pending_version: u64,
    previous_value: Vec<u8>,
}

/// Shared directory implementation over a linearizable cluster cache.
pub struct CacheDirectoryBackend {
    cache: Arc<dyn ClusterCacheBackend>,
    lease_versions: LeaseVersions,
    known_instances: KnownInstances,
    grace: Duration,
}

impl CacheDirectoryBackend {
    /// Creates a directory backend over a linearizable cluster cache.
    ///
    /// # Errors
    /// Returns an error if the cache does not provide linearizable consistency.
    pub fn new(
        cache: Arc<dyn ClusterCacheBackend>,
        grace: Duration,
    ) -> Result<Self, DirectoryBackendError> {
        if cache.consistency() != CacheConsistency::Linearizable {
            return Err(unavailable(anyhow::anyhow!(
                "service-discovery requires a linearizable cache; {} is {:?}",
                cache.provider_name(),
                cache.consistency()
            )));
        }
        Ok(Self {
            cache,
            lease_versions: Arc::new(Mutex::new(HashMap::new())),
            known_instances: Arc::new(Mutex::new(HashMap::new())),
            grace,
        })
    }

    fn cache_error(error: cluster_sdk::ClusterError) -> DirectoryBackendError {
        unavailable(anyhow::Error::new(error))
    }

    fn log_cleanup_error(key: &str, error: &cluster_sdk::ClusterError) {
        if !matches!(error, cluster_sdk::ClusterError::CasConflict { .. }) {
            tracing::warn!(%key, %error, "failed to clean up service-discovery cache state");
        }
    }

    async fn get_entry(&self, key: &str) -> Result<Option<CacheEntry>, DirectoryBackendError> {
        self.cache.get(key).await.map_err(Self::cache_error)
    }

    async fn scan(&self, prefix: &str) -> Result<Vec<String>, DirectoryBackendError> {
        self.cache
            .scan_prefix(prefix)
            .await
            .map_err(Self::cache_error)
    }

    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, DirectoryBackendError> {
        serde_json::to_vec(value).map_err(|error| unavailable(error.into()))
    }

    fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, DirectoryBackendError> {
        serde_json::from_slice(bytes).map_err(|error| unavailable(error.into()))
    }

    fn instance_key(gear: &str, instance_id: Uuid) -> String {
        format!(
            "{INSTANCE_PREFIX}{}/{}/",
            hex(gear.as_bytes()),
            instance_id.simple()
        )
        .trim_end_matches('/')
        .to_owned()
    }

    fn lease_key(gear: &str, instance_id: Uuid) -> String {
        format!(
            "{LEASE_PREFIX}{}/{}/",
            hex(gear.as_bytes()),
            instance_id.simple()
        )
        .trim_end_matches('/')
        .to_owned()
    }

    fn claim_key(service: &str) -> String {
        format!("{CLAIM_PREFIX}{}", hex(service.as_bytes()))
    }

    async fn load_instance(
        &self,
        gear: &str,
        instance_id: Uuid,
    ) -> Result<Option<(StoredInstance, StoredLease, CacheEntry, Vec<u8>)>, DirectoryBackendError>
    {
        let instance_key = Self::instance_key(gear, instance_id);
        let lease_key = Self::lease_key(gear, instance_id);
        let Some(instance_entry) = self.get_entry(&instance_key).await? else {
            return Ok(None);
        };
        let mut stored: StoredInstance = Self::decode(&instance_entry.value)?;
        let Some(lease_entry) = self.get_entry(&lease_key).await? else {
            return Ok(None);
        };
        let lease: StoredLease = Self::decode(&lease_entry.value)?;
        if stored.generation != lease.generation {
            return Ok(None);
        }
        stored.reported_state = lease.reported_state;
        self.lease_versions
            .lock()
            .await
            .insert((gear.to_owned(), instance_id), lease_entry.clone());
        self.known_instances
            .lock()
            .await
            .insert((gear.to_owned(), instance_id), stored.clone());
        Ok(Some((stored, lease, lease_entry, instance_entry.value)))
    }

    async fn load_records(
        &self,
        grace: Duration,
        filter_claims: bool,
    ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
        let keys = self.scan(INSTANCE_PREFIX).await?;
        let now = SystemTime::now();
        let mut records = Vec::new();
        for key in keys {
            let Some((gear, instance_id)) = parse_instance_key(&key) else {
                continue;
            };
            let Some((stored, lease, lease_entry, _)) =
                self.load_instance(&gear, instance_id).await?
            else {
                continue;
            };
            let expires_at = from_millis(lease.lease_expires_at_ms)?;
            if expires_at
                .checked_add(grace)
                .is_none_or(|visible_until| visible_until <= now)
            {
                continue;
            }
            let mut instance = to_gear_instance(&stored);
            if filter_claims {
                let mut visible = HashMap::new();
                for (service, endpoint) in instance.grpc_services {
                    if self.claim_owner(&service).await?.as_deref() == Some(gear.as_str()) {
                        visible.insert(service, endpoint);
                    }
                }
                instance.grpc_services = visible;
            }
            let record = DirectoryRecord {
                instance: Arc::new(instance),
                generation: lease.generation,
                revision: lease.revision,
                lease_expires_at: expires_at,
            };
            if lease_entry.version == 0 {
                return Err(unavailable(anyhow::anyhow!(
                    "cache returned version zero for lease key {key}"
                )));
            }
            records.push(record);
        }
        Ok(records)
    }

    async fn claim_owner(&self, service: &str) -> Result<Option<String>, DirectoryBackendError> {
        let Some(entry) = self.get_entry(&Self::claim_key(service)).await? else {
            return Ok(None);
        };
        match Self::decode::<StoredClaim>(&entry.value)? {
            StoredClaim::Stable { owner } if !owner.is_empty() => Ok(Some(owner)),
            StoredClaim::Stable { .. } | StoredClaim::Pending { .. } => Ok(None),
        }
    }

    async fn claim_matches_registration(
        &self,
        service: &str,
        gear: &str,
    ) -> Result<bool, DirectoryBackendError> {
        let Some(entry) = self.get_entry(&Self::claim_key(service)).await? else {
            return Ok(false);
        };
        Ok(match Self::decode::<StoredClaim>(&entry.value)? {
            StoredClaim::Stable { owner } | StoredClaim::Pending { owner, .. } => owner == gear,
        })
    }

    async fn live_owner_for_gear(
        &self,
        service: &str,
        gear: &str,
        grace: Duration,
    ) -> Result<bool, DirectoryBackendError> {
        Ok(self.load_records(grace, false).await?.iter().any(|record| {
            record.instance.gear == gear && record.instance.grpc_services.contains_key(service)
        }))
    }

    async fn reserve_claim(
        &self,
        service: &str,
        gear: &str,
        grace: Duration,
    ) -> Result<Option<ClaimReservation>, DirectoryBackendError> {
        let key = Self::claim_key(service);
        for _ in 0..MAX_CAS_RETRIES {
            let current = self.get_entry(&key).await?;
            let previous_owner = match current
                .as_ref()
                .map(|entry| Self::decode::<StoredClaim>(&entry.value))
                .transpose()?
            {
                Some(StoredClaim::Stable { owner }) if owner == gear => return Ok(None),
                Some(StoredClaim::Stable { owner }) if owner.is_empty() => None,
                Some(StoredClaim::Stable { owner }) => {
                    if self.live_owner_for_gear(service, &owner, grace).await? {
                        return Err(conflict(service, owner));
                    }
                    Some(owner)
                }
                Some(StoredClaim::Pending { owner, .. }) => {
                    return Err(conflict(service, owner));
                }
                None => None,
            };
            let pending = StoredClaim::Pending {
                owner: gear.to_owned(),
                previous_owner: previous_owner.clone(),
                token: Uuid::new_v4(),
                started_at_ms: to_millis(SystemTime::now())?,
            };
            let pending_value = Self::encode(&pending)?;
            let reserved = match current.as_ref() {
                Some(entry) => self
                    .cache
                    .compare_and_swap(&key, entry.version, &pending_value, Ttl::Indefinite)
                    .await
                    .map(Some),
                None => {
                    self.cache
                        .put_if_absent(PutRequest {
                            key: &key,
                            value: &pending_value,
                            ttl: Ttl::Indefinite,
                        })
                        .await
                }
            };
            match reserved {
                Ok(Some(reserved_entry)) => {
                    if let Some(previous_owner) = &previous_owner
                        && self
                            .live_owner_for_gear(service, previous_owner, grace)
                            .await?
                    {
                        self.restore_claim(
                            &key,
                            reserved_entry.version,
                            current.as_ref().map_or_else(
                                || {
                                    Self::encode(&StoredClaim::Stable {
                                        owner: String::new(),
                                    })
                                },
                                |entry| Ok(entry.value.clone()),
                            )?,
                        )
                        .await;
                        return Err(conflict(service, previous_owner.clone()));
                    }
                    return Ok(Some(ClaimReservation {
                        key,
                        owner: gear.to_owned(),
                        pending_version: reserved_entry.version,
                        previous_value: current.map_or_else(
                            || {
                                Self::encode(&StoredClaim::Stable {
                                    owner: String::new(),
                                })
                            },
                            |entry| Ok(entry.value),
                        )?,
                    }));
                }
                Ok(None) | Err(cluster_sdk::ClusterError::CasConflict { .. }) => {}
                Err(error) => return Err(Self::cache_error(error)),
            }
        }
        Err(unavailable(anyhow::anyhow!(
            "claim reservation contention exceeded retry limit for service {service}"
        )))
    }

    async fn release_claims(&self, reservations: &[ClaimReservation]) {
        for reservation in reservations.iter().rev() {
            if let Err(error) = self
                .cache
                .compare_and_swap(
                    &reservation.key,
                    reservation.pending_version,
                    &reservation.previous_value,
                    Ttl::Indefinite,
                )
                .await
            {
                Self::log_cleanup_error(&reservation.key, &error);
            }
        }
    }

    async fn restore_claim(&self, key: &str, pending_version: u64, previous_value: Vec<u8>) {
        if let Err(error) = self
            .cache
            .compare_and_swap(key, pending_version, &previous_value, Ttl::Indefinite)
            .await
        {
            Self::log_cleanup_error(key, &error);
        }
    }

    async fn finalize_claims(
        &self,
        reservations: &[ClaimReservation],
    ) -> Result<(), DirectoryBackendError> {
        for reservation in reservations {
            let stable_value = Self::encode(&StoredClaim::Stable {
                owner: reservation.owner.clone(),
            })?;
            self.cache
                .compare_and_swap(
                    &reservation.key,
                    reservation.pending_version,
                    &stable_value,
                    Ttl::Indefinite,
                )
                .await
                .map_err(Self::cache_error)?;
        }
        Ok(())
    }

    async fn cleanup_failed_registration(
        &self,
        stored: &StoredInstance,
        lease_entry: &CacheEntry,
        preserve_existing: bool,
    ) {
        if preserve_existing {
            return;
        }
        let value = match Self::encode(stored) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "failed to encode failed service-discovery registration cleanup");
                return;
            }
        };
        if let Err(error) = self
            .cache
            .compare_and_delete(
                &Self::instance_key(&stored.gear, stored.instance_id),
                &value,
            )
            .await
        {
            Self::log_cleanup_error(
                &Self::instance_key(&stored.gear, stored.instance_id),
                &error,
            );
        }
        if let Err(error) = self
            .cache
            .compare_and_delete(
                &Self::lease_key(&stored.gear, stored.instance_id),
                &lease_entry.value,
            )
            .await
        {
            Self::log_cleanup_error(&Self::lease_key(&stored.gear, stored.instance_id), &error);
        }
    }

    async fn allocate_generation(&self, instance_id: Uuid) -> Result<u64, DirectoryBackendError> {
        // UUIDs with the same first byte share a sequence. This bounds
        // persistent generation metadata to 256 keys while preserving
        // per-instance monotonicity and avoiding one hot counter for all writes.
        let key = format!(
            "{GENERATION_SEQUENCE_PREFIX}{:02x}",
            instance_id.as_bytes()[0]
        );
        for _ in 0..MAX_CAS_RETRIES {
            if let Some(entry) = self.get_entry(&key).await? {
                let previous = parse_u64(&entry.value)?;
                let next = previous.checked_add(1).ok_or_else(|| {
                    unavailable(anyhow::anyhow!("registration generation exhausted"))
                })?;
                match self
                    .cache
                    .compare_and_swap(
                        &key,
                        entry.version,
                        next.to_string().as_bytes(),
                        Ttl::Indefinite,
                    )
                    .await
                {
                    Ok(_) => return Ok(next),
                    Err(cluster_sdk::ClusterError::CasConflict { .. }) => {}
                    Err(error) => return Err(Self::cache_error(error)),
                }
            } else {
                let request = PutRequest {
                    key: &key,
                    value: b"1",
                    ttl: Ttl::Indefinite,
                };
                if self
                    .cache
                    .put_if_absent(request)
                    .await
                    .map_err(Self::cache_error)?
                    .is_some()
                {
                    return Ok(1);
                }
            }
        }
        Err(unavailable(anyhow::anyhow!(
            "generation sequence contention exceeded retry limit"
        )))
    }

    async fn write_lease(
        &self,
        gear: &str,
        instance_id: Uuid,
        generation: u64,
        reported_state: ReportedState,
        options: LeaseWriteOptions,
    ) -> Result<(StoredLease, CacheEntry), DirectoryBackendError> {
        let key = Self::lease_key(gear, instance_id);
        let now = SystemTime::now();
        let expires_at = now
            .checked_add(options.ttl)
            .ok_or_else(|| unavailable(anyhow::anyhow!("lease deadline overflow")))?;
        let expires_at_ms = to_millis(expires_at)?;
        for _ in 0..MAX_CAS_RETRIES {
            let current = self.get_entry(&key).await?;
            let previous_lease = current
                .as_ref()
                .map(|entry| Self::decode::<StoredLease>(&entry.value))
                .transpose()?;
            if options.preserve_existing {
                let Some(previous) = previous_lease.as_ref() else {
                    return Err(unavailable(anyhow::anyhow!(
                        "registration lease disappeared concurrently for {gear}/{instance_id}"
                    )));
                };
                if previous.generation != generation
                    || from_millis(previous.lease_expires_at_ms)?
                        .checked_add(options.grace)
                        .is_none_or(|visible_until| visible_until <= now)
                {
                    return Err(unavailable(anyhow::anyhow!(
                        "registration lease generation changed or expired concurrently for \
                         {gear}/{instance_id}"
                    )));
                }
            }
            if let Some(previous) = &previous_lease
                && (previous.generation > generation
                    || (previous.generation != generation
                        && from_millis(previous.lease_expires_at_ms)?
                            .checked_add(options.grace)
                            .is_some_and(|visible_until| visible_until > now)))
            {
                return Err(unavailable(anyhow::anyhow!(
                    "registration generation changed concurrently for {gear}/{instance_id}"
                )));
            }
            let revision = previous_lease.as_ref().map_or(0, |lease| lease.revision);
            let lease = StoredLease {
                generation,
                revision: revision
                    .checked_add(1)
                    .ok_or_else(|| unavailable(anyhow::anyhow!("directory revision exhausted")))?,
                lease_expires_at_ms: expires_at_ms,
                reported_state: reported_state.into(),
            };
            let value = Self::encode(&lease)?;
            let backend_ttl = Ttl::Of(options.ttl.saturating_add(options.grace));
            let written = if let Some(entry) = current {
                match self
                    .cache
                    .compare_and_swap_value(&key, &entry.value, &value, backend_ttl)
                    .await
                {
                    Ok(entry) => Some(entry),
                    Err(cluster_sdk::ClusterError::CasConflict { .. }) => None,
                    Err(error) => return Err(Self::cache_error(error)),
                }
            } else {
                let request = PutRequest {
                    key: &key,
                    value: &value,
                    ttl: backend_ttl,
                };
                self.cache
                    .put_if_absent(request)
                    .await
                    .map_err(Self::cache_error)?
            };
            if let Some(entry) = written {
                self.lease_versions
                    .lock()
                    .await
                    .insert((gear.to_owned(), instance_id), entry.clone());
                return Ok((lease, entry));
            }
        }
        Err(unavailable(anyhow::anyhow!(
            "lease contention exceeded retry limit for {gear}/{instance_id}"
        )))
    }

    async fn write_instance(
        &self,
        incoming: &GearInstance,
        generation: u64,
        reported: ReportedState,
        preserve_existing: bool,
    ) -> Result<StoredInstance, DirectoryBackendError> {
        let key = Self::instance_key(&incoming.gear, incoming.instance_id);
        for _ in 0..MAX_CAS_RETRIES {
            let current = self.get_entry(&key).await?;
            let stored = if let Some(entry) = current {
                let previous: StoredInstance = Self::decode(&entry.value)?;
                let stored = if previous.generation == generation {
                    let merged = merge_instance(Some(&to_gear_instance(&previous)), incoming)
                        .with_reported_state(reported);
                    from_gear_instance(&merged, generation, reported)
                } else {
                    let current_lease = self
                        .get_entry(&Self::lease_key(&incoming.gear, incoming.instance_id))
                        .await?
                        .map(|entry| Self::decode::<StoredLease>(&entry.value))
                        .transpose()?;
                    if current_lease
                        .as_ref()
                        .is_none_or(|lease| lease.generation != generation)
                    {
                        return Err(unavailable(anyhow::anyhow!(
                            "instance generation changed concurrently for {}/{}",
                            incoming.gear,
                            incoming.instance_id
                        )));
                    }
                    from_gear_instance(incoming, generation, reported)
                };
                let value = Self::encode(&stored)?;
                match self
                    .cache
                    .compare_and_swap_value(&key, &entry.value, &value, Ttl::Indefinite)
                    .await
                {
                    Ok(_) => stored,
                    Err(cluster_sdk::ClusterError::CasConflict { .. }) => continue,
                    Err(error) => return Err(Self::cache_error(error)),
                }
            } else {
                let stored = from_gear_instance(incoming, generation, reported);
                let value = Self::encode(&stored)?;
                let request = PutRequest {
                    key: &key,
                    value: &value,
                    ttl: Ttl::Indefinite,
                };
                if self
                    .cache
                    .put_if_absent(request)
                    .await
                    .map_err(Self::cache_error)?
                    .is_none()
                {
                    continue;
                }
                stored
            };
            for service in stored.grpc_services.keys() {
                if !self
                    .claim_matches_registration(service, &incoming.gear)
                    .await?
                {
                    let value = Self::encode(&stored)?;
                    if !preserve_existing
                        && let Err(error) = self.cache.compare_and_delete(&key, &value).await
                    {
                        Self::log_cleanup_error(&key, &error);
                    }
                    return Err(conflict(
                        service,
                        self.claim_owner(service)
                            .await?
                            .unwrap_or_else(|| "unknown".to_owned()),
                    ));
                }
            }
            self.known_instances.lock().await.insert(
                (incoming.gear.clone(), incoming.instance_id),
                stored.clone(),
            );
            return Ok(stored);
        }
        Err(unavailable(anyhow::anyhow!(
            "instance contention exceeded retry limit for {}/{}",
            incoming.gear,
            incoming.instance_id
        )))
    }

    async fn event_record(
        &self,
        gear: &str,
        instance_id: Uuid,
        grace: Duration,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        let Some((stored, lease, _, _)) = self.load_instance(gear, instance_id).await? else {
            return Ok(None);
        };
        let expires_at = from_millis(lease.lease_expires_at_ms)?;
        if expires_at
            .checked_add(grace)
            .is_none_or(|visible_until| visible_until <= SystemTime::now())
        {
            return Ok(None);
        }
        let mut instance = to_gear_instance(&stored);
        let mut visible = HashMap::new();
        for (service, endpoint) in instance.grpc_services {
            if self.claim_owner(&service).await?.as_deref() == Some(gear) {
                visible.insert(service, endpoint);
            }
        }
        instance.grpc_services = visible;
        Ok(Some(DirectoryRecord {
            instance: Arc::new(instance),
            generation: lease.generation,
            revision: lease.revision,
            lease_expires_at: expires_at,
        }))
    }

    async fn apply_cache_event(
        &self,
        event: CacheEvent,
        grace: Duration,
    ) -> Result<Vec<DirectoryChange>, DirectoryBackendError> {
        let key = event.key();
        if key.starts_with(INSTANCE_PREFIX) || key.starts_with(LEASE_PREFIX) {
            let Some((gear, instance_id)) = parse_record_key(key) else {
                return Ok(Vec::new());
            };
            if let Some(record) = self.event_record(&gear, instance_id, grace).await? {
                return Ok(vec![DirectoryChange::Upsert(record)]);
            }
            // A partial successor must not tombstone its as-yet-unseen generation.
            let known = self
                .known_instances
                .lock()
                .await
                .remove(&(gear.clone(), instance_id));
            self.lease_versions
                .lock()
                .await
                .remove(&(gear.clone(), instance_id));
            return Ok(known
                .map(|stored| {
                    vec![DirectoryChange::Remove {
                        gear,
                        instance_id,
                        generation: stored.generation,
                    }]
                })
                .unwrap_or_default());
        }
        if key.starts_with(CLAIM_PREFIX) {
            let records = self.load_records(grace, true).await?;
            return Ok(records.into_iter().map(DirectoryChange::Upsert).collect());
        }
        Ok(Vec::new())
    }

    async fn janitor(&self, grace: Duration) -> Result<(), DirectoryBackendError> {
        let now = SystemTime::now();
        self.janitor_instance_keys(grace, now).await?;
        self.janitor_lease_keys().await?;
        let live_records = self.load_records(grace, false).await?;
        self.janitor_claims(&live_records, now).await?;
        self.restore_missing_claims(&live_records).await
    }

    async fn janitor_instance_keys(
        &self,
        grace: Duration,
        now: SystemTime,
    ) -> Result<(), DirectoryBackendError> {
        for key in self.scan(INSTANCE_PREFIX).await? {
            let Some((gear, instance_id)) = parse_instance_key(&key) else {
                continue;
            };
            let Some(instance_entry) = self.get_entry(&key).await? else {
                continue;
            };
            let lease_key = Self::lease_key(&gear, instance_id);
            let live_lease = match self.get_entry(&lease_key).await? {
                Some(entry) => {
                    let lease = Self::decode::<StoredLease>(&entry.value)?;
                    from_millis(lease.lease_expires_at_ms)?
                        .checked_add(grace)
                        .is_some_and(|visible_until| visible_until > now)
                }
                None => false,
            };
            if !live_lease {
                if let Err(error) = self
                    .cache
                    .compare_and_delete(&key, &instance_entry.value)
                    .await
                {
                    Self::log_cleanup_error(&key, &error);
                }
                self.lease_versions
                    .lock()
                    .await
                    .remove(&(gear.clone(), instance_id));
                self.known_instances
                    .lock()
                    .await
                    .remove(&(gear, instance_id));
            }
        }
        Ok(())
    }

    async fn janitor_lease_keys(&self) -> Result<(), DirectoryBackendError> {
        for key in self.scan(LEASE_PREFIX).await? {
            let Some((gear, instance_id)) = parse_record_key(&key) else {
                continue;
            };
            if self
                .get_entry(&Self::instance_key(&gear, instance_id))
                .await?
                .is_none()
                && let Some(entry) = self.get_entry(&key).await?
                && let Err(error) = self.cache.compare_and_delete(&key, &entry.value).await
            {
                Self::log_cleanup_error(&key, &error);
            }
        }
        Ok(())
    }

    async fn janitor_claims(
        &self,
        live_records: &[DirectoryRecord],
        now: SystemTime,
    ) -> Result<(), DirectoryBackendError> {
        for key in self.scan(CLAIM_PREFIX).await? {
            let Some(service) = key
                .strip_prefix(CLAIM_PREFIX)
                .and_then(unhex)
                .and_then(|bytes| String::from_utf8(bytes).ok())
            else {
                continue;
            };
            let Some(entry) = self.get_entry(&key).await? else {
                continue;
            };
            let current = Self::decode::<StoredClaim>(&entry.value)?;
            if let Some(owner) = claim_reconciliation_target(current, &service, live_records, now)?
            {
                let value = Self::encode(&StoredClaim::Stable { owner })?;
                if let Err(error) = self
                    .cache
                    .compare_and_swap(&key, entry.version, &value, Ttl::Indefinite)
                    .await
                {
                    Self::log_cleanup_error(&key, &error);
                }
            }
        }
        Ok(())
    }

    async fn restore_missing_claims(
        &self,
        live_records: &[DirectoryRecord],
    ) -> Result<(), DirectoryBackendError> {
        for record in live_records {
            for service in record.instance.grpc_services.keys() {
                let key = Self::claim_key(service);
                let value = Self::encode(&StoredClaim::Stable {
                    owner: record.instance.gear.clone(),
                })?;
                match self.get_entry(&key).await? {
                    Some(entry) => {
                        if let StoredClaim::Stable { owner } =
                            Self::decode::<StoredClaim>(&entry.value)?
                            && owner.is_empty()
                            && let Err(error) = self
                                .cache
                                .compare_and_swap(&key, entry.version, &value, Ttl::Indefinite)
                                .await
                        {
                            Self::log_cleanup_error(&key, &error);
                        }
                    }
                    None => {
                        self.cache
                            .put_if_absent(PutRequest {
                                key: &key,
                                value: &value,
                                ttl: Ttl::Indefinite,
                            })
                            .await
                            .map_err(Self::cache_error)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn claim_reconciliation_target(
    claim: StoredClaim,
    service: &str,
    live_records: &[DirectoryRecord],
    now: SystemTime,
) -> Result<Option<String>, DirectoryBackendError> {
    let has_owner = |owner: &str| {
        live_records.iter().any(|record| {
            record.instance.gear == owner && record.instance.grpc_services.contains_key(service)
        })
    };
    match claim {
        StoredClaim::Pending {
            owner,
            previous_owner,
            started_at_ms,
            ..
        } => {
            if from_millis(started_at_ms)?
                .checked_add(CLAIM_RESERVATION_TTL)
                .is_none_or(|expires| expires > now)
            {
                return Ok(None);
            }
            let owner = if has_owner(&owner) {
                owner
            } else if let Some(previous_owner) = previous_owner
                && has_owner(&previous_owner)
            {
                previous_owner
            } else {
                String::new()
            };
            Ok(Some(owner))
        }
        StoredClaim::Stable { owner } => {
            if !owner.is_empty() && has_owner(&owner) {
                return Ok(None);
            }
            let target = live_records
                .iter()
                .find(|record| record.instance.grpc_services.contains_key(service))
                .map_or_else(String::new, |record| record.instance.gear.clone());
            Ok((target != owner).then_some(target))
        }
    }
}

#[async_trait]
impl DirectoryBackend for CacheDirectoryBackend {
    async fn register(
        &self,
        instance: Arc<GearInstance>,
        ttl: Duration,
        grace: Duration,
    ) -> Result<DirectoryRecord, DirectoryBackendError> {
        let key = (instance.gear.clone(), instance.instance_id);
        let existing = self.load_instance(&key.0, key.1).await?;
        let live_existing = existing
            .as_ref()
            .filter(|(_, lease, _, _)| {
                from_millis(lease.lease_expires_at_ms)
                    .and_then(|expires| {
                        expires
                            .checked_add(grace)
                            .ok_or_else(|| unavailable(anyhow::anyhow!("lease deadline overflow")))
                    })
                    .is_ok_and(|visible_until| visible_until > SystemTime::now())
            })
            .map(|(stored, _, _, _)| to_gear_instance(stored));
        let preserve_existing = live_existing.is_some();
        let merged = merge_instance(live_existing.as_ref(), &instance);
        let reported = merged
            .reported_state
            .or_else(|| {
                live_existing
                    .as_ref()
                    .and_then(|stored| stored.reported_state)
            })
            .unwrap_or_default();
        let merged = merged.with_reported_state(reported);

        let mut reservations = Vec::new();
        for service in merged.grpc_services.keys() {
            match self.reserve_claim(service, &merged.gear, grace).await {
                Ok(Some(reservation)) => reservations.push(reservation),
                Ok(None) => {}
                Err(error) => {
                    self.release_claims(&reservations).await;
                    return Err(error);
                }
            }
        }

        let generation_result = match live_existing.as_ref().and(existing.as_ref()) {
            Some((_, lease, _, _)) => lease.generation,
            None => match self.allocate_generation(key.1).await {
                Ok(generation) => generation,
                Err(error) => {
                    self.release_claims(&reservations).await;
                    return Err(error);
                }
            },
        };
        let generation = generation_result;
        let (_, lease_entry) = match self
            .write_lease(
                &key.0,
                key.1,
                generation,
                reported,
                LeaseWriteOptions {
                    preserve_existing,
                    ttl,
                    grace,
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.release_claims(&reservations).await;
                return Err(error);
            }
        };

        let stored = match self
            .write_instance(&merged, generation, reported, preserve_existing)
            .await
        {
            Ok(stored) => stored,
            Err(error) => {
                if !preserve_existing
                    && let Err(cleanup_error) = self
                        .cache
                        .compare_and_delete(&Self::lease_key(&key.0, key.1), &lease_entry.value)
                        .await
                {
                    Self::log_cleanup_error(&Self::lease_key(&key.0, key.1), &cleanup_error);
                }
                self.release_claims(&reservations).await;
                return Err(error);
            }
        };
        if let Err(error) = self.finalize_claims(&reservations).await {
            self.cleanup_failed_registration(&stored, &lease_entry, preserve_existing)
                .await;
            self.release_claims(&reservations).await;
            return Err(error);
        }

        for service in stored.grpc_services.keys() {
            if self.claim_owner(service).await?.as_deref() != Some(merged.gear.as_str()) {
                self.cleanup_failed_registration(&stored, &lease_entry, preserve_existing)
                    .await;
                self.release_claims(&reservations).await;
                return Err(conflict(
                    service,
                    self.claim_owner(service)
                        .await?
                        .unwrap_or_else(|| "unknown".to_owned()),
                ));
            }
        }

        let current_lease_entry = self
            .get_entry(&Self::lease_key(&key.0, key.1))
            .await?
            .ok_or_else(|| {
                unavailable(anyhow::anyhow!(
                    "registration lease disappeared before commit for {}/{}",
                    key.0,
                    key.1
                ))
            })?;
        let lease = Self::decode::<StoredLease>(&current_lease_entry.value)?;
        if lease.generation != generation {
            self.cleanup_failed_registration(&stored, &lease_entry, preserve_existing)
                .await;
            self.release_claims(&reservations).await;
            return Err(unavailable(anyhow::anyhow!(
                "registration generation changed before commit for {}/{}",
                key.0,
                key.1
            )));
        }
        self.lease_versions
            .lock()
            .await
            .insert(key.clone(), current_lease_entry);
        Ok(DirectoryRecord {
            instance: Arc::new(to_gear_instance(&stored)),
            generation,
            revision: lease.revision,
            lease_expires_at: from_millis(lease.lease_expires_at_ms)?,
        })
    }

    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: Uuid,
        reported: ReportedState,
        ttl: Duration,
        grace: Duration,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        let key = (gear.to_owned(), instance_id);
        let lease_key = Self::lease_key(gear, instance_id);
        let mut stored = if let Some(stored) = self.known_instances.lock().await.get(&key).cloned()
        {
            stored
        } else {
            let Some((stored, _, entry, _)) = self.load_instance(gear, instance_id).await? else {
                return Ok(None);
            };
            self.lease_versions.lock().await.insert(key.clone(), entry);
            stored
        };
        let expected_generation = stored.generation;
        let mut cached = self.lease_versions.lock().await.get(&key).cloned();
        for _ in 0..MAX_CAS_RETRIES {
            let entry = match cached.take() {
                Some(entry) => entry,
                None => match self.get_entry(&lease_key).await? {
                    Some(entry) => entry,
                    None => return Ok(None),
                },
            };
            let lease: StoredLease = Self::decode(&entry.value)?;
            if lease.generation != expected_generation {
                return Ok(None);
            }
            let expires_at = from_millis(lease.lease_expires_at_ms)?;
            if expires_at
                .checked_add(grace)
                .is_none_or(|visible_until| visible_until <= SystemTime::now())
            {
                return Ok(None);
            }
            let next_expires = SystemTime::now()
                .checked_add(ttl)
                .ok_or_else(|| unavailable(anyhow::anyhow!("lease deadline overflow")))?;
            let next = StoredLease {
                generation: lease.generation,
                revision: lease
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| unavailable(anyhow::anyhow!("directory revision exhausted")))?,
                lease_expires_at_ms: to_millis(next_expires)?,
                reported_state: reported.into(),
            };
            let value = Self::encode(&next)?;
            match self
                .cache
                .compare_and_swap_value(
                    &lease_key,
                    &entry.value,
                    &value,
                    Ttl::Of(ttl.saturating_add(grace)),
                )
                .await
            {
                Ok(updated) => {
                    self.lease_versions
                        .lock()
                        .await
                        .insert(key.clone(), updated);
                    if stored.generation != next.generation {
                        return Ok(None);
                    }
                    stored.reported_state = reported.into();
                    self.known_instances
                        .lock()
                        .await
                        .insert(key.clone(), stored.clone());
                    let record = DirectoryRecord {
                        instance: Arc::new(to_gear_instance(&stored)),
                        generation: next.generation,
                        revision: next.revision,
                        lease_expires_at: next_expires,
                    };
                    return Ok(Some(record));
                }
                Err(cluster_sdk::ClusterError::CasConflict { current, .. }) => {
                    cached = current;
                }
                Err(error) => return Err(Self::cache_error(error)),
            }
        }
        Err(unavailable(anyhow::anyhow!(
            "heartbeat contention exceeded retry limit for {gear}/{instance_id}"
        )))
    }

    async fn deregister(
        &self,
        gear: &str,
        instance_id: Uuid,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        let instance_key = Self::instance_key(gear, instance_id);
        let lease_key = Self::lease_key(gear, instance_id);
        let mut removed = None;
        for _ in 0..MAX_CAS_RETRIES {
            let Some((stored, lease, lease_entry, instance_value)) =
                self.load_instance(gear, instance_id).await?
            else {
                return Ok(removed);
            };
            let record = DirectoryRecord {
                instance: Arc::new(to_gear_instance(&stored)),
                generation: lease.generation,
                revision: lease.revision,
                lease_expires_at: from_millis(lease.lease_expires_at_ms)?,
            };
            if !self
                .cache
                .compare_and_delete(&lease_key, &lease_entry.value)
                .await
                .map_err(Self::cache_error)?
            {
                continue;
            }
            self.cache
                .compare_and_delete(&instance_key, &instance_value)
                .await
                .map_err(Self::cache_error)?;
            removed = Some(record);
            self.lease_versions
                .lock()
                .await
                .remove(&(gear.to_owned(), instance_id));
            for service in stored.grpc_services.keys() {
                if !self
                    .live_owner_for_gear(service, gear, Duration::ZERO)
                    .await?
                {
                    let claim_key = Self::claim_key(service);
                    if let Some(entry) = self.get_entry(&claim_key).await?
                        && matches!(
                            Self::decode::<StoredClaim>(&entry.value)?,
                            StoredClaim::Stable { owner } if owner == gear
                        )
                    {
                        let empty_claim = Self::encode(&StoredClaim::Stable {
                            owner: String::new(),
                        })?;
                        if let Err(error) = self
                            .cache
                            .compare_and_swap(
                                &claim_key,
                                entry.version,
                                &empty_claim,
                                Ttl::Indefinite,
                            )
                            .await
                        {
                            Self::log_cleanup_error(&claim_key, &error);
                        }
                    }
                }
            }
            break;
        }
        Ok(removed)
    }

    async fn load_all(
        &self,
        grace: Duration,
    ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
        self.load_records(grace, true).await
    }

    async fn janitor(&self, grace: Duration) -> Result<(), DirectoryBackendError> {
        CacheDirectoryBackend::janitor(self, grace).await
    }

    async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
        let watch = if self.cache.features().prefix_watch() {
            self.cache
                .watch_prefix(PREFIX)
                .await
                .map_err(Self::cache_error)?
        } else {
            PollingPrefixWatch::spawn(Arc::clone(&self.cache), PREFIX, PREFIX_POLL_INTERVAL)
        };
        let cache = Arc::clone(&self.cache);
        let versions = Arc::clone(&self.lease_versions);
        let known_instances = Arc::clone(&self.known_instances);
        let grace = self.grace;
        let stream = stream::unfold(
            (
                watch,
                cache,
                versions,
                known_instances,
                grace,
                Vec::<DirectoryChange>::new(),
            ),
            move |(mut watch, cache, versions, known_instances, grace, mut pending)| async move {
                loop {
                    if let Some(change) = pending.pop() {
                        return Some((
                            Ok(change),
                            (watch, cache, versions, known_instances, grace, pending),
                        ));
                    }
                    match watch.recv().await {
                        Some(CacheWatchEvent::Event(event)) => {
                            let backend = CacheDirectoryBackend {
                                cache: Arc::clone(&cache),
                                lease_versions: Arc::clone(&versions),
                                known_instances: Arc::clone(&known_instances),
                                grace,
                            };
                            let changes = backend.apply_cache_event(event, grace).await;
                            match changes {
                                Ok(mut changes) => {
                                    if let Some(change) = changes.pop() {
                                        pending = changes;
                                        return Some((
                                            Ok(change),
                                            (
                                                watch,
                                                cache,
                                                versions,
                                                known_instances,
                                                grace,
                                                pending,
                                            ),
                                        ));
                                    }
                                }
                                Err(error) => {
                                    return Some((
                                        Err(error),
                                        (watch, cache, versions, known_instances, grace, pending),
                                    ));
                                }
                            }
                        }
                        Some(CacheWatchEvent::Lagged { dropped }) => {
                            return Some((
                                Err(unavailable(anyhow::anyhow!(
                                    "directory cache watch lagged by {dropped} events"
                                ))),
                                (watch, cache, versions, known_instances, grace, pending),
                            ));
                        }
                        Some(CacheWatchEvent::Reset) => {
                            return Some((
                                Err(unavailable(anyhow::anyhow!("directory cache watch reset"))),
                                (watch, cache, versions, known_instances, grace, pending),
                            ));
                        }
                        Some(CacheWatchEvent::Closed(error)) => {
                            return Some((
                                Err(unavailable(anyhow::Error::new(error))),
                                (watch, cache, versions, known_instances, grace, pending),
                            ));
                        }
                        Some(_) => {
                            return Some((
                                Err(unavailable(anyhow::anyhow!(
                                    "unsupported directory cache watch event"
                                ))),
                                (watch, cache, versions, known_instances, grace, pending),
                            ));
                        }
                        None => return None,
                    }
                }
            },
        );
        Ok(Box::pin(stream))
    }
}

fn merge_instance(existing: Option<&GearInstance>, incoming: &GearInstance) -> GearInstance {
    let mut merged = GearInstance::new(incoming.gear.clone(), incoming.instance_id);
    merged.control = incoming
        .control
        .clone()
        .or_else(|| existing.and_then(|instance| instance.control.clone()));
    merged.grpc_services = if incoming.grpc_services.is_empty() {
        existing.map_or_else(HashMap::new, |instance| instance.grpc_services.clone())
    } else {
        incoming.grpc_services.clone()
    };
    merged.version.clone_from(&incoming.version);
    merged.rest_endpoint = incoming
        .rest_endpoint
        .clone()
        .or_else(|| existing.and_then(|instance| instance.rest_endpoint.clone()));
    merged.openapi_spec_hash = incoming
        .openapi_spec_hash
        .clone()
        .or_else(|| existing.and_then(|instance| instance.openapi_spec_hash.clone()));
    merged.labels = if incoming.labels.is_empty() {
        existing.map_or_else(BTreeMap::new, |instance| instance.labels.clone())
    } else {
        incoming.labels.clone()
    };
    merged.reported_state = incoming
        .reported_state
        .or_else(|| existing.and_then(|instance| instance.reported_state));
    merged
}

fn from_gear_instance(
    instance: &GearInstance,
    generation: u64,
    reported: ReportedState,
) -> StoredInstance {
    StoredInstance {
        gear: instance.gear.clone(),
        instance_id: instance.instance_id,
        control: instance
            .control
            .as_ref()
            .map(|endpoint| endpoint.uri.clone()),
        grpc_services: instance
            .grpc_services
            .iter()
            .map(|(name, endpoint)| (name.clone(), endpoint.uri.clone()))
            .collect(),
        version: instance.version.clone(),
        rest_endpoint: instance
            .rest_endpoint
            .as_ref()
            .map(|endpoint| endpoint.uri.clone()),
        openapi_spec_hash: instance.openapi_spec_hash.clone(),
        labels: instance.labels.clone(),
        reported_state: reported.into(),
        generation,
    }
}

fn to_gear_instance(stored: &StoredInstance) -> GearInstance {
    let mut instance = GearInstance::new(stored.gear.clone(), stored.instance_id);
    instance.control = stored.control.as_ref().map(Endpoint::from_uri);
    instance.grpc_services = stored
        .grpc_services
        .iter()
        .map(|(name, uri)| (name.clone(), Endpoint::from_uri(uri.clone())))
        .collect();
    instance.version.clone_from(&stored.version);
    instance.rest_endpoint = stored.rest_endpoint.as_ref().map(Endpoint::from_uri);
    instance
        .openapi_spec_hash
        .clone_from(&stored.openapi_spec_hash);
    instance.labels = stored.labels.clone();
    instance.reported_state = Some(stored.reported_state.into());
    instance
}

fn parse_instance_key(key: &str) -> Option<(String, Uuid)> {
    parse_record_segments(key.strip_prefix(INSTANCE_PREFIX)?)
}

fn parse_record_key(key: &str) -> Option<(String, Uuid)> {
    let key = key
        .strip_prefix(INSTANCE_PREFIX)
        .or_else(|| key.strip_prefix(LEASE_PREFIX))?;
    parse_record_segments(key)
}

fn parse_record_segments(key: &str) -> Option<(String, Uuid)> {
    let (gear, id) = key.split_once('/')?;
    Some((
        String::from_utf8(unhex(gear)?).ok()?,
        Uuid::parse_str(id).ok()?,
    ))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn unhex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            Some(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}

fn parse_u64(value: &[u8]) -> Result<u64, DirectoryBackendError> {
    std::str::from_utf8(value)
        .map_err(|error| unavailable(error.into()))?
        .parse()
        .map_err(|error| unavailable(anyhow::Error::new(error)))
}

fn to_millis(time: SystemTime) -> Result<u64, DirectoryBackendError> {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_err(|error| unavailable(error.into()))?
        .as_millis();
    u64::try_from(millis).map_err(|error| unavailable(error.into()))
}

fn from_millis(millis: u64) -> Result<SystemTime, DirectoryBackendError> {
    UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .ok_or_else(|| unavailable(anyhow::anyhow!("stored lease deadline overflow")))
}

fn conflict(service: &str, owner: String) -> DirectoryBackendError {
    DirectoryBackendError::Conflict(GrpcServiceNameConflict {
        service_name: service.to_owned(),
        owner,
        recoverable: true,
    })
}

fn unavailable(error: anyhow::Error) -> DirectoryBackendError {
    DirectoryBackendError::Unavailable(error)
}

#[cfg(all(test, feature = "integration"))]
#[path = "../tests/integration/cache_directory_backend.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use cluster_sdk::cache::{CacheFeatures, CacheWatch, ClusterCacheBackend};
    use cluster_sdk::error::ClusterError;
    use futures_util::StreamExt;
    use standalone_cluster_plugin::StandaloneClusterPlugin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_instance(gear: &str, id: Uuid, service: &str) -> Arc<GearInstance> {
        Arc::new(
            GearInstance::new(gear, id)
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 50051))
                .with_reported_state(ReportedState::Serving),
        )
    }

    pub(super) struct CountingCache {
        inner: Arc<dyn ClusterCacheBackend>,
        pub(super) reads: AtomicUsize,
        pub(super) mutations: AtomicUsize,
    }

    impl CountingCache {
        pub(super) fn new(inner: Arc<dyn ClusterCacheBackend>) -> Self {
            Self {
                inner,
                reads: AtomicUsize::new(0),
                mutations: AtomicUsize::new(0),
            }
        }

        pub(super) fn reset(&self) {
            self.reads.store(0, Ordering::SeqCst);
            self.mutations.store(0, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl ClusterCacheBackend for CountingCache {
        fn consistency(&self) -> CacheConsistency {
            self.inner.consistency()
        }

        fn features(&self) -> CacheFeatures {
            self.inner.features()
        }

        async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.get(key).await
        }

        async fn put(&self, request: PutRequest<'_>) -> Result<(), ClusterError> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            self.inner.put(request).await
        }

        async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            self.inner.delete(key).await
        }

        async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.contains(key).await
        }

        async fn put_if_absent(
            &self,
            request: PutRequest<'_>,
        ) -> Result<Option<CacheEntry>, ClusterError> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            self.inner.put_if_absent(request).await
        }

        async fn compare_and_swap(
            &self,
            key: &str,
            expected_version: u64,
            new_value: &[u8],
            ttl: Ttl,
        ) -> Result<CacheEntry, ClusterError> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            self.inner
                .compare_and_swap(key, expected_version, new_value, ttl)
                .await
        }

        async fn compare_and_swap_value(
            &self,
            key: &str,
            expected_value: &[u8],
            new_value: &[u8],
            ttl: Ttl,
        ) -> Result<CacheEntry, ClusterError> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            self.inner
                .compare_and_swap_value(key, expected_value, new_value, ttl)
                .await
        }

        async fn compare_and_delete(
            &self,
            key: &str,
            expected_value: &[u8],
        ) -> Result<bool, ClusterError> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            self.inner.compare_and_delete(key, expected_value).await
        }

        async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
            self.inner.watch(key).await
        }

        async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
            self.inner.watch_prefix(prefix).await
        }

        async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.scan_prefix(prefix).await
        }

        async fn probe(&self) -> Result<(), ClusterError> {
            self.inner.probe().await
        }
    }

    #[tokio::test]
    async fn leases_and_generations_round_trip_through_shared_cache() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let backend = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let id = Uuid::new_v4();

        let first = backend
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        let heartbeat = backend
            .heartbeat(
                "gear-a",
                id,
                ReportedState::Draining,
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("heartbeat")
            .expect("registered lease");
        assert_eq!(heartbeat.generation, first.generation);
        assert!(heartbeat.revision > first.revision);
        assert_eq!(
            backend
                .load_all(Duration::from_secs(2))
                .await
                .expect("load records")
                .len(),
            1
        );

        backend
            .deregister("gear-a", id)
            .await
            .expect("deregister")
            .expect("record existed");
        assert!(
            backend
                .get_entry(&CacheDirectoryBackend::instance_key("gear-a", id))
                .await
                .expect("read instance key")
                .is_none(),
            "deregistration removes the stored instance after a state-changing heartbeat"
        );
        let second = backend
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register new generation");
        assert!(second.generation > first.generation);

        let another_id = Uuid::new_v4();
        let third = backend
            .register(
                test_instance("gear-a", another_id, "example.OtherService"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register another instance");
        assert_eq!(
            backend
                .deregister("gear-a", another_id)
                .await
                .expect("deregister another instance")
                .expect("another instance existed")
                .generation,
            third.generation
        );
        let fourth = backend
            .register(
                test_instance("gear-a", another_id, "example.OtherService"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register another instance again");
        assert!(fourth.generation > third.generation);
        let mut generation_keys = plugin
            .cache()
            .scan_prefix(GENERATION_SEQUENCE_PREFIX)
            .await
            .expect("scan generation metadata");
        generation_keys.sort();
        let mut expected_generation_keys = vec![
            format!("{GENERATION_SEQUENCE_PREFIX}{:02x}", id.as_bytes()[0]),
            format!(
                "{GENERATION_SEQUENCE_PREFIX}{:02x}",
                another_id.as_bytes()[0]
            ),
        ];
        expected_generation_keys.sort();
        expected_generation_keys.dedup();
        assert_eq!(
            generation_keys, expected_generation_keys,
            "generation metadata must use bounded UUID shards, not one permanent key per instance"
        );
        plugin.stop().await;
    }

    #[tokio::test]
    async fn new_replica_reloads_persistent_records_from_shared_cache() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let id = Uuid::new_v4();
        let original = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let registered = original
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        drop(original);

        let restarted = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let restored = restarted
            .load_all(Duration::from_secs(2))
            .await
            .expect("restore startup snapshot");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].generation, registered.generation);
        assert_eq!(restored[0].instance.gear, "gear-a");
        assert!(
            restored[0]
                .instance
                .grpc_services
                .contains_key("example.Service")
        );
        plugin.stop().await;
    }

    #[tokio::test]
    async fn steady_state_heartbeat_uses_one_mutation_and_no_cache_reads() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let cache = Arc::new(CountingCache::new(plugin.cache()));
        let backend = CacheDirectoryBackend::new(cache.clone(), Duration::from_secs(2))
            .expect("linearizable cache");
        let id = Uuid::new_v4();
        backend
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        cache.reset();

        backend
            .heartbeat(
                "gear-a",
                id,
                ReportedState::Serving,
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("heartbeat")
            .expect("registered lease");

        assert_eq!(cache.reads.load(Ordering::SeqCst), 0);
        assert_eq!(cache.mutations.load(Ordering::SeqCst), 1);
        plugin.stop().await;
    }

    #[tokio::test]
    async fn failed_update_cleanup_preserves_a_live_registration() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let backend = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let id = Uuid::new_v4();
        backend
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        let (stored, _, lease_entry, _) = backend
            .load_instance("gear-a", id)
            .await
            .expect("load registration")
            .expect("registration exists");

        backend
            .cleanup_failed_registration(&stored, &lease_entry, true)
            .await;

        assert_eq!(
            backend
                .load_all(Duration::from_secs(2))
                .await
                .expect("load records")
                .len(),
            1
        );
        plugin.stop().await;
    }

    #[tokio::test]
    async fn stale_registration_cannot_recreate_a_deleted_lease_generation() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let backend = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let id = Uuid::new_v4();
        let first = backend
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        let (_, _, old_lease, _) = backend
            .load_instance("gear-a", id)
            .await
            .expect("load registration")
            .expect("registration exists");

        assert!(
            backend
                .cache
                .compare_and_delete(
                    &CacheDirectoryBackend::lease_key("gear-a", id),
                    &old_lease.value,
                )
                .await
                .expect("remove lease")
        );
        assert!(
            backend
                .write_lease(
                    "gear-a",
                    id,
                    first.generation,
                    ReportedState::Serving,
                    LeaseWriteOptions {
                        preserve_existing: true,
                        ttl: Duration::from_secs(10),
                        grace: Duration::from_secs(2),
                    },
                )
                .await
                .is_err(),
            "an in-flight registration must not revive its removed generation"
        );
        assert!(
            backend
                .get_entry(&CacheDirectoryBackend::lease_key("gear-a", id))
                .await
                .expect("read lease")
                .is_none()
        );

        let successor = backend
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("retry as a fresh registration");
        assert!(successor.generation > first.generation);
        plugin.stop().await;
    }

    #[tokio::test]
    async fn competing_gears_cannot_both_claim_one_service() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let cache: Arc<dyn ClusterCacheBackend> = plugin.cache();
        let backend_a = CacheDirectoryBackend::new(Arc::clone(&cache), Duration::from_secs(2))
            .expect("linearizable cache");
        let backend_b =
            CacheDirectoryBackend::new(cache, Duration::from_secs(2)).expect("linearizable cache");

        let (a, b) = tokio::join!(
            backend_a.register(
                test_instance("gear-a", Uuid::new_v4(), "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            ),
            backend_b.register(
                test_instance("gear-b", Uuid::new_v4(), "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            ),
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert!(matches!(
            (a, b),
            (Err(DirectoryBackendError::Conflict(_)), Ok(_))
                | (Ok(_), Err(DirectoryBackendError::Conflict(_)))
        ));
        plugin.stop().await;
    }

    #[tokio::test]
    async fn stale_replica_cannot_heartbeat_a_successor_generation() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let cache: Arc<dyn ClusterCacheBackend> = plugin.cache();
        let old_replica = CacheDirectoryBackend::new(Arc::clone(&cache), Duration::from_secs(2))
            .expect("linearizable cache");
        let current_replica =
            CacheDirectoryBackend::new(cache, Duration::from_secs(2)).expect("linearizable cache");
        let id = Uuid::new_v4();

        let first = old_replica
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("first registration");
        current_replica
            .deregister("gear-a", id)
            .await
            .expect("deregister old generation");
        let successor = current_replica
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("successor registration");
        assert!(successor.generation > first.generation);

        assert!(
            old_replica
                .heartbeat(
                    "gear-a",
                    id,
                    ReportedState::Serving,
                    Duration::from_secs(10),
                    Duration::from_secs(2),
                )
                .await
                .expect("stale heartbeat check")
                .is_none()
        );
        plugin.stop().await;
    }

    #[tokio::test]
    async fn change_feed_delivers_remote_instance_updates() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let backend = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let mut changes = backend.changes().await.expect("subscribe");
        backend
            .register(
                test_instance("gear-a", Uuid::new_v4(), "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");

        let change = tokio::time::timeout(Duration::from_secs(2), changes.next())
            .await
            .expect("change arrives")
            .expect("stream remains open")
            .expect("valid change");
        assert!(matches!(change, DirectoryChange::Upsert(_)));
        plugin.stop().await;
    }

    #[tokio::test]
    async fn remote_heartbeat_feed_carries_authoritative_reported_state() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let cache: Arc<dyn ClusterCacheBackend> = plugin.cache();
        let writer = CacheDirectoryBackend::new(Arc::clone(&cache), Duration::from_secs(2))
            .expect("linearizable cache");
        let reader =
            CacheDirectoryBackend::new(cache, Duration::from_secs(2)).expect("linearizable cache");
        let id = Uuid::new_v4();
        writer
            .register(
                Arc::new(
                    GearInstance::new("gear-a", id)
                        .with_grpc_service("example.Service", Endpoint::http("127.0.0.1", 50051))
                        .with_reported_state(ReportedState::Starting),
                ),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register starting instance");
        reader
            .load_all(Duration::from_secs(2))
            .await
            .expect("load initial record");
        let mut changes = reader.changes().await.expect("subscribe");

        writer
            .heartbeat(
                "gear-a",
                id,
                ReportedState::Serving,
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("heartbeat")
            .expect("registered lease");

        let change = tokio::time::timeout(Duration::from_secs(2), changes.next())
            .await
            .expect("heartbeat change arrives")
            .expect("stream remains open")
            .expect("valid change");
        let DirectoryChange::Upsert(record) = change else {
            panic!("heartbeat should upsert the changed lease state");
        };
        assert_eq!(record.instance.reported_state, Some(ReportedState::Serving));
        plugin.stop().await;
    }

    #[tokio::test]
    async fn deleted_cache_keys_emit_removal_for_the_known_generation() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let writer = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let reader = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let id = Uuid::new_v4();
        let record = writer
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        reader
            .load_all(Duration::from_secs(2))
            .await
            .expect("load initial snapshot");
        writer
            .deregister("gear-a", id)
            .await
            .expect("deregister instance");

        let changes = reader
            .apply_cache_event(
                CacheEvent::Deleted {
                    key: CacheDirectoryBackend::instance_key("gear-a", id),
                },
                Duration::from_secs(2),
            )
            .await
            .expect("apply deletion");
        assert!(matches!(
            changes.as_slice(),
            [DirectoryChange::Remove { gear, instance_id, generation }]
                if gear == "gear-a" && *instance_id == id && *generation == record.generation
        ));
        plugin.stop().await;
    }

    #[tokio::test]
    async fn incomplete_successor_lease_does_not_tombstone_the_new_generation() {
        let plugin = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("start standalone cache");
        let writer = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let reader = CacheDirectoryBackend::new(plugin.cache(), Duration::from_secs(2))
            .expect("linearizable cache");
        let id = Uuid::new_v4();
        let record = writer
            .register(
                test_instance("gear-a", id, "example.Service"),
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
            .await
            .expect("register instance");
        reader
            .load_all(Duration::from_secs(2))
            .await
            .expect("load initial snapshot");
        writer
            .deregister("gear-a", id)
            .await
            .expect("deregister instance");
        let lease = StoredLease {
            generation: record.generation + 1,
            revision: 1,
            lease_expires_at_ms: to_millis(SystemTime::now() + Duration::from_secs(10))
                .expect("lease deadline"),
            reported_state: ReportedValue::Serving,
        };
        let key = CacheDirectoryBackend::lease_key("gear-a", id);
        writer
            .cache
            .put(PutRequest {
                key: &key,
                value: &CacheDirectoryBackend::encode(&lease).expect("encode lease"),
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("write successor lease before its instance");

        let changes = reader
            .apply_cache_event(
                CacheEvent::Changed { key: key.clone() },
                Duration::from_secs(2),
            )
            .await
            .expect("apply successor lease change");
        assert!(matches!(
            changes.as_slice(),
            [DirectoryChange::Remove { generation, .. }] if *generation == record.generation
        ));
        assert!(
            reader
                .apply_cache_event(CacheEvent::Changed { key }, Duration::from_secs(2))
                .await
                .expect("reapply successor lease change")
                .is_empty(),
            "an unknown, incomplete generation must not be tombstoned"
        );
        plugin.stop().await;
    }
}
