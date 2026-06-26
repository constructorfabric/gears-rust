//! Leader election via k8s `coordination.k8s.io/v1` Lease.
//!
//! Uses the same mechanism as `client-go` leader election:
//! create-or-acquire a Lease, renew periodically, release on shutdown.

use std::time::Duration;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kube::Client;
use kube::api::{Api, ObjectMeta, PostParams};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{LeaderElector, LeaderWorkFn};

const WORK_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const K8S_API_TIMEOUT: Duration = Duration::from_secs(10);
const POD_NAMESPACE_ENV: &str = "POD_NAMESPACE";
const POD_NAME_ENV: &str = "POD_NAME";

/// Configuration for k8s Lease-based leader election.
#[derive(Debug, Clone)]
pub struct K8sLeaseConfig {
    /// Kubernetes namespace where the Lease object lives.
    pub namespace: String,
    /// Unique identity of this pod (typically `POD_NAME` from downward API).
    pub identity: String,
    /// Prefix for Lease object names: `"{lease_prefix}-{role}"`.
    pub lease_prefix: String,
    /// How long before a Lease is considered expired.
    pub lease_duration: Duration,
    /// How often the holder renews the Lease.
    pub renew_period: Duration,
}

impl K8sLeaseConfig {
    /// Build config from environment variables.
    ///
    /// - `POD_NAMESPACE` -> namespace (**required**)
    /// - `POD_NAME` -> identity (**required**)
    pub fn from_env(lease_prefix: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            namespace: required_env(POD_NAMESPACE_ENV)?,
            identity: required_env(POD_NAME_ENV)?,
            lease_prefix: lease_prefix.into(),
            lease_duration: Duration::from_secs(15),
            renew_period: Duration::from_secs(2),
        })
    }

    /// Override timing parameters.
    #[must_use]
    #[allow(dead_code)]
    pub fn with_timing(mut self, lease_duration: Duration, renew_period: Duration) -> Self {
        self.lease_duration = lease_duration;
        self.renew_period = renew_period;
        self
    }

    /// Validate config for safe operation.
    ///
    /// # Errors
    /// Returns an error when required invariants are violated.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.namespace.trim().is_empty() {
            return Err(anyhow!("k8s leader: namespace must be non-empty"));
        }
        if self.identity.trim().is_empty() {
            return Err(anyhow!("k8s leader: identity must be non-empty"));
        }
        if self.lease_prefix.trim().is_empty() {
            return Err(anyhow!("k8s leader: lease_prefix must be non-empty"));
        }
        if self.renew_period.is_zero() {
            return Err(anyhow!("k8s leader: renew_period must be > 0"));
        }
        if self.lease_duration <= self.renew_period {
            return Err(anyhow!(
                "k8s leader: lease_duration ({:?}) must be > renew_period ({:?})",
                self.lease_duration,
                self.renew_period
            ));
        }
        if self.lease_duration.as_secs() == 0 {
            return Err(anyhow!(
                "k8s leader: lease_duration ({:?}) must be at least 1 second",
                self.lease_duration
            ));
        }
        i32::try_from(self.lease_duration.as_secs()).map_err(|_| {
            anyhow!(
                "k8s leader: lease_duration ({:?}) exceeds Kubernetes Lease i32 seconds range",
                self.lease_duration
            )
        })?;
        Ok(())
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).map_err(|_| anyhow!("k8s leader: {name} must be set"))?;
    if value.trim().is_empty() {
        return Err(anyhow!("k8s leader: {name} must be non-empty"));
    }
    Ok(value)
}

/// Leader elector backed by a k8s `coordination.k8s.io/v1` Lease.
pub struct K8sLeaseElector {
    client: Client,
    config: K8sLeaseConfig,
}

impl std::fmt::Debug for K8sLeaseElector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("K8sLeaseElector")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl K8sLeaseElector {
    /// Create an elector with an existing kube [`Client`].
    #[must_use]
    #[allow(dead_code)]
    pub fn with_client(client: Client, config: K8sLeaseConfig) -> Self {
        Self { client, config }
    }

    /// Create an elector using the default in-cluster / kubeconfig client.
    ///
    /// # Errors
    ///
    /// Fails if kube client cannot be initialised (no kubeconfig, no
    /// in-cluster service account).
    pub async fn from_default(config: K8sLeaseConfig) -> anyhow::Result<Self> {
        config.validate().context("invalid k8s lease config")?;
        let client = Client::try_default().await.context("kube client init")?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl LeaderElector for K8sLeaseElector {
    async fn run_role(
        &self,
        role: &str,
        cancel: CancellationToken,
        work: LeaderWorkFn,
    ) -> anyhow::Result<()> {
        self.config.validate().context("invalid k8s lease config")?;
        let lease_name = format!("{}-{role}", self.config.lease_prefix);
        let api: Api<Lease> = Api::namespaced(self.client.clone(), &self.config.namespace);
        let mut backoff = Backoff::new();

        loop {
            let acquired = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                result = self.try_acquire(&api, &lease_name) => result,
            };

            match acquired {
                Ok(true) => {
                    backoff.reset();
                    info!(role, identity = %self.config.identity, %lease_name, "acquired leadership");
                    if let Err(e) = self
                        .run_while_leader(role, &api, &lease_name, cancel.clone(), &work)
                        .await
                    {
                        warn!(role, error = %e, "leader loop ended (leadership lost or error)");
                    }
                }
                Ok(false) => {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Ok(()),
                        () = sleep(self.config.renew_period) => {}
                    }
                }
                Err(e) => {
                    let delay = backoff.next_delay();
                    #[allow(clippy::cast_possible_truncation)]
                    let delay_ms = delay.as_millis() as u64;
                    warn!(role, error = %e, delay_ms, "leader acquire failed");
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Ok(()),
                        () = sleep(delay) => {}
                    }
                }
            }
        }
    }
}

impl K8sLeaseElector {
    async fn try_acquire(&self, api: &Api<Lease>, lease_name: &str) -> anyhow::Result<bool> {
        ensure_lease_exists(api, &self.config.namespace, lease_name).await?;

        let lease = timeout(K8S_API_TIMEOUT, api.get(lease_name))
            .await
            .with_context(|| {
                format!(
                    "timeout getting Lease {} for acquire",
                    lease_ref(&self.config.namespace, lease_name)
                )
            })?
            .with_context(|| {
                format!(
                    "get Lease {} for acquire",
                    lease_ref(&self.config.namespace, lease_name)
                )
            })?;

        let now = Timestamp::now();
        let (holder, renew_time, duration_s) = read_lease_state(&lease);
        let lease_duration_s = duration_s.unwrap_or(self.default_lease_duration_seconds()?);

        if !can_acquire_leadership(
            holder.as_deref(),
            renew_time,
            lease_duration_s,
            self.config.identity.as_str(),
            now,
        ) {
            return Ok(false);
        }

        // Acquire leadership using resourceVersion-guarded replace to avoid split-brain.
        let mut new_lease = lease.clone();
        new_lease.spec.get_or_insert_default().holder_identity = Some(self.config.identity.clone());
        new_lease
            .spec
            .get_or_insert_default()
            .lease_duration_seconds = Some(lease_duration_s);
        new_lease.spec.get_or_insert_default().renew_time = Some(
            k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now),
        );

        match timeout(
            K8S_API_TIMEOUT,
            api.replace(lease_name, &PostParams::default(), &new_lease),
        )
        .await
        {
            Ok(Ok(_)) => Ok(true),
            Ok(Err(kube::Error::Api(resp))) if resp.code == 409 => Ok(false),
            Ok(Err(e)) => Err(e).with_context(|| {
                format!(
                    "replace Lease {} to acquire leadership for {}",
                    lease_ref(&self.config.namespace, lease_name),
                    self.config.identity
                )
            }),
            Err(_) => Err(anyhow!(
                "timeout replacing Lease {} to acquire leadership for {}",
                lease_ref(&self.config.namespace, lease_name),
                self.config.identity
            )),
        }
    }

    async fn run_while_leader(
        &self,
        role: &str,
        api: &Api<Lease>,
        lease_name: &str,
        cancel: CancellationToken,
        work: &LeaderWorkFn,
    ) -> anyhow::Result<()> {
        let mut active: Option<ActiveRole> = None;

        loop {
            // Ensure work is running while we hold leadership.
            if active.as_ref().is_none_or(|a| a.handle.is_finished()) {
                if let Some(r) = active.take() {
                    r.await_and_log(role).await;
                }
                let child = CancellationToken::new();
                let handle = tokio::spawn(work(child.clone()));
                active = Some(ActiveRole { child, handle });
            }

            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    stop_and_release(&mut active, role, api, lease_name, &self.config.identity).await;
                    return Ok(());
                }
                () = sleep(self.config.renew_period) => {
                    if let Err(e) = self.renew_once(api, lease_name).await {
                        if let Some(r) = active.take() {
                            r.stop(role).await;
                        }
                        return Err(e);
                    }
                }
            }
        }
    }

    async fn renew_once(&self, api: &Api<Lease>, lease_name: &str) -> anyhow::Result<()> {
        let lease = timeout(K8S_API_TIMEOUT, api.get(lease_name))
            .await
            .with_context(|| {
                format!(
                    "timeout getting Lease {} for renew",
                    lease_ref(&self.config.namespace, lease_name)
                )
            })?
            .with_context(|| {
                format!(
                    "get Lease {} for renew",
                    lease_ref(&self.config.namespace, lease_name)
                )
            })?;
        let (holder, renew_time, duration_s) = read_lease_state(&lease);

        if holder.as_deref() != Some(self.config.identity.as_str()) {
            return Err(anyhow!("lease holder changed to {holder:?}"));
        }

        let now = Timestamp::now();
        let lease_duration_s = duration_s.unwrap_or(self.default_lease_duration_seconds()?);

        if lease_expired(renew_time, lease_duration_s, now) {
            return Err(anyhow!("lease expired before renew"));
        }

        let mut new_lease = lease.clone();
        new_lease.spec.get_or_insert_default().renew_time = Some(
            k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now),
        );

        match timeout(
            K8S_API_TIMEOUT,
            api.replace(lease_name, &PostParams::default(), &new_lease),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(kube::Error::Api(resp))) if resp.code == 409 => Err(anyhow!("renew conflict")),
            Ok(Err(e)) => Err(e).with_context(|| {
                format!(
                    "replace Lease {} to renew leadership for {}",
                    lease_ref(&self.config.namespace, lease_name),
                    self.config.identity
                )
            }),
            Err(_) => Err(anyhow!(
                "timeout replacing Lease {} to renew leadership for {}",
                lease_ref(&self.config.namespace, lease_name),
                self.config.identity
            )),
        }
    }

    fn default_lease_duration_seconds(&self) -> anyhow::Result<i32> {
        i32::try_from(self.config.lease_duration.as_secs()).map_err(|_| {
            anyhow!(
                "k8s leader: lease_duration ({:?}) exceeds Kubernetes Lease i32 seconds range",
                self.config.lease_duration
            )
        })
    }
}

struct ActiveRole {
    child: CancellationToken,
    handle: JoinHandle<anyhow::Result<()>>,
}

impl ActiveRole {
    async fn stop(self, role: &str) {
        self.stop_with_timeout(role, WORK_STOP_TIMEOUT).await;
    }

    /// Stops the worker, returning `true` if shutdown was clean (no error, no
    /// panic, no timeout).  Callers should only release the lease on a clean
    /// shutdown; dirty shutdowns must rely on lease expiry as a safety net.
    #[allow(clippy::cognitive_complexity)] // inflated by tokio::select! + tracing macros
    async fn stop_with_timeout(self, role: &str, stop_timeout: Duration) -> bool {
        let ActiveRole { child, mut handle } = self;
        child.cancel();
        match timeout(stop_timeout, &mut handle).await {
            Ok(Ok(Ok(()))) => true,
            Ok(Ok(Err(e))) => {
                warn!(role, error = %e, "leader work exited with error");
                false
            }
            Ok(Err(e)) => {
                warn!(role, error = %e, "leader work task panicked");
                false
            }
            Err(_) => {
                #[allow(clippy::cast_possible_truncation)]
                let timeout_ms = stop_timeout.as_millis().min(u128::from(u64::MAX)) as u64;
                warn!(
                    role,
                    timeout_ms, "leader work did not stop in time; aborting task"
                );
                handle.abort();
                match handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!(role, error = %e, "leader work exited with error after abort");
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => warn!(role, error = %e, "leader work task panicked after abort"),
                }
                false
            }
        }
    }

    #[allow(clippy::cognitive_complexity)] // inflated by tracing macros
    async fn await_and_log(self, role: &str) {
        match self.handle.await {
            Ok(Ok(())) => warn!(role, "leader work exited unexpectedly (Ok)"),
            Ok(Err(e)) => warn!(role, error = %e, "leader work exited unexpectedly (Err)"),
            Err(e) => warn!(role, error = %e, "leader work panicked"),
        }
    }
}

async fn stop_and_release(
    active: &mut Option<ActiveRole>,
    role: &str,
    api: &Api<Lease>,
    lease_name: &str,
    identity: &str,
) {
    let clean = match active.take() {
        Some(r) => r.stop_with_timeout(role, WORK_STOP_TIMEOUT).await,
        None => true,
    };
    if clean {
        if let Err(e) = release_lease(api, lease_name, identity).await {
            warn!(role, error = %e, "best-effort lease release failed");
        }
    } else {
        warn!(
            role,
            "skipping lease release after dirty shutdown; relying on lease expiry"
        );
    }
}

/// Best-effort release: clear `holderIdentity` to speed up re-election.
async fn release_lease(api: &Api<Lease>, lease_name: &str, identity: &str) -> anyhow::Result<()> {
    let lease = timeout(K8S_API_TIMEOUT, api.get(lease_name))
        .await
        .with_context(|| format!("timeout getting lease {lease_name} for release by {identity}"))?
        .with_context(|| format!("get lease {lease_name} for release by {identity}"))?;
    let (holder, _, _) = read_lease_state(&lease);

    if holder.as_deref() != Some(identity) {
        return Ok(());
    }

    let mut new_lease = lease.clone();
    new_lease.spec.get_or_insert_default().holder_identity = None;

    // resourceVersion-guarded replace; conflict means someone else already updated.
    match timeout(
        K8S_API_TIMEOUT,
        api.replace(lease_name, &PostParams::default(), &new_lease),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(kube::Error::Api(resp))) if resp.code == 409 => {}
        Ok(Err(e)) => {
            return Err(e).with_context(|| {
                format!("replace lease {lease_name} to release leadership for {identity}")
            });
        }
        Err(_) => {
            return Err(anyhow!(
                "timeout replacing lease {lease_name} to release leadership for {identity}"
            ));
        }
    }
    Ok(())
}

async fn ensure_lease_exists(
    api: &Api<Lease>,
    namespace: &str,
    lease_name: &str,
) -> anyhow::Result<()> {
    match timeout(K8S_API_TIMEOUT, api.get(lease_name)).await {
        Ok(Ok(_)) => return Ok(()),
        Ok(Err(kube::Error::Api(resp))) if resp.code == 404 => {}
        Ok(Err(e)) => {
            return Err(e).with_context(|| {
                format!(
                    "get Lease {} before create",
                    lease_ref(namespace, lease_name)
                )
            });
        }
        Err(_) => {
            return Err(anyhow!(
                "timeout getting Lease {} before create",
                lease_ref(namespace, lease_name)
            ));
        }
    }

    let lease = Lease {
        metadata: ObjectMeta {
            name: Some(lease_name.to_owned()),
            ..ObjectMeta::default()
        },
        ..Lease::default()
    };
    match timeout(K8S_API_TIMEOUT, api.create(&PostParams::default(), &lease)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(kube::Error::Api(resp))) if resp.code == 409 => Ok(()),
        Ok(Err(e)) => {
            Err(e).with_context(|| format!("create Lease {}", lease_ref(namespace, lease_name)))
        }
        Err(_) => Err(anyhow!(
            "timeout creating Lease {}",
            lease_ref(namespace, lease_name)
        )),
    }
}

fn read_lease_state(lease: &Lease) -> (Option<String>, Option<Timestamp>, Option<i32>) {
    let spec = lease.spec.as_ref();
    let holder = spec.and_then(|s| s.holder_identity.clone());
    let duration = spec.and_then(|s| s.lease_duration_seconds);
    let renew_time = spec.and_then(|s| s.renew_time.as_ref()).map(|t| t.0);
    (holder, renew_time, duration)
}

fn can_acquire_leadership(
    holder: Option<&str>,
    renew_time: Option<Timestamp>,
    lease_duration_s: i32,
    identity: &str,
    now: Timestamp,
) -> bool {
    let i_am_holder = holder == Some(identity);
    lease_expired(renew_time, lease_duration_s, now) || i_am_holder || holder.is_none()
}

fn lease_expired(renew_time: Option<Timestamp>, lease_duration_s: i32, now: Timestamp) -> bool {
    let lease_span = SignedDuration::from_secs(i64::from(lease_duration_s));
    renew_time.is_none_or(|rt| {
        rt.checked_add(lease_span)
            .map_or(true, |expiry| expiry < now)
    })
}

fn lease_ref(namespace: &str, lease_name: &str) -> String {
    format!("{namespace}/{lease_name}")
}

/// Exponential backoff with "equal jitter" for leader acquisition retries.
///
/// Strategy: each delay is `base/2 + rand(0..base/2)`, where `base` doubles
/// on each attempt up to `max_delay`.  This is the "equal jitter" approach
/// from the AWS Architecture Blog — it guarantees a minimum forward progress
/// of `base/2` while still desynchronising competing pods effectively.
///
/// The internal PRNG is a 64-bit xorshift64* — fast, non-cryptographic,
/// and good enough for retry jitter.  Seeded once at construction from
/// coarse time + thread id so that concurrent `Backoff` instances on
/// different pods (or even different tasks within the same pod) diverge.
struct Backoff {
    initial: Duration,
    max_delay: Duration,
    current: Duration,
    rng: XorShift64,
}

impl Backoff {
    fn new() -> Self {
        let initial = Duration::from_millis(200);
        Self {
            initial,
            max_delay: Duration::from_secs(5),
            current: initial,
            rng: XorShift64::seed_from_time(),
        }
    }

    fn reset(&mut self) {
        self.current = self.initial;
    }

    fn next_delay(&mut self) -> Duration {
        let base = self.current;
        // Advance for the *next* call, clamped to max.
        self.current = self.max_delay.min(base.saturating_mul(2));

        // Equal jitter: deterministic half + random half.
        #[allow(clippy::cast_possible_truncation)] // clamped to u64::MAX
        let base_ms = base.as_millis().min(u128::from(u64::MAX)) as u64;
        let half = base_ms >> 1;
        let jitter = if half > 0 {
            self.rng.next_u64() % half
        } else {
            0
        };
        Duration::from_millis(half + jitter)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tiny non-cryptographic PRNG (xorshift64*)
// ────────────────────────────────────────────────────────────────────────────

/// Minimal xorshift64* — 8 bytes of state, period 2^64-1.
///
/// NOT suitable for cryptography; perfectly fine for retry jitter.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    /// Seed from coarse monotonic time and thread identity.
    ///
    /// Two `Backoff` instances created in different pods (or different tasks)
    /// will almost certainly get different seeds, which is all we need.
    fn seed_from_time() -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::thread::current().id().hash(&mut h);
        let s = h.finish();
        // xorshift64 requires a non-zero seed.
        Self {
            state: if s == 0 { 1 } else { s },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut s = self.state;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.state = s;
        // xorshift64*: multiply by a large odd constant for better bit mixing.
        s.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::Instant;

    #[test]
    fn validate_rejects_subsecond_lease_duration() {
        let err = test_config()
            .with_timing(Duration::from_millis(500), Duration::from_millis(100))
            .validate()
            .expect_err("should reject sub-second lease_duration");

        assert!(err.to_string().contains("at least 1 second"));
    }

    #[test]
    fn validate_rejects_lease_duration_outside_k8s_range() {
        let err = test_config()
            .with_timing(
                Duration::from_secs(i32::MAX as u64 + 1),
                Duration::from_secs(1),
            )
            .validate()
            .expect_err("should reject lease_duration outside i32 range");

        assert!(err.to_string().contains("i32 seconds range"));
    }

    #[test]
    fn cannot_acquire_when_other_holder_is_still_fresh() {
        let now = Timestamp::now();
        let renew_time = now
            .checked_add(SignedDuration::from_secs(-3))
            .expect("trivial timestamp arithmetic");

        let acquired = can_acquire_leadership(Some("pod-b"), Some(renew_time), 15, "pod-a", now);

        assert!(!acquired);
    }

    #[test]
    fn can_acquire_when_other_holder_is_expired() {
        let now = Timestamp::now();
        let renew_time = now
            .checked_add(SignedDuration::from_secs(-30))
            .expect("trivial timestamp arithmetic");

        let acquired = can_acquire_leadership(Some("pod-b"), Some(renew_time), 15, "pod-a", now);

        assert!(acquired);
    }

    #[test]
    fn can_acquire_when_already_holder() {
        let now = Timestamp::now();
        let renew_time = now
            .checked_add(SignedDuration::from_secs(-3))
            .expect("trivial timestamp arithmetic");

        let acquired = can_acquire_leadership(Some("pod-a"), Some(renew_time), 15, "pod-a", now);

        assert!(acquired);
    }

    #[tokio::test]
    async fn stop_times_out_and_aborts_unresponsive_work() {
        let started = Arc::new(AtomicBool::new(false));
        let started_flag = Arc::clone(&started);
        let child = CancellationToken::new();
        let handle = tokio::spawn(async move {
            started_flag.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_mins(1)).await;
            Ok::<(), anyhow::Error>(())
        });
        let role = ActiveRole { child, handle };

        let begun = Instant::now();
        role.stop_with_timeout("test", Duration::from_millis(20))
            .await;

        assert!(started.load(Ordering::SeqCst));
        assert!(begun.elapsed() < Duration::from_secs(1));
    }

    fn test_config() -> K8sLeaseConfig {
        K8sLeaseConfig {
            namespace: "default".to_owned(),
            identity: "pod-a".to_owned(),
            lease_prefix: "chat-engine".to_owned(),
            lease_duration: Duration::from_secs(15),
            renew_period: Duration::from_secs(2),
        }
    }
}
