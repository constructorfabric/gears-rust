use std::sync::Arc;
use std::time::{Duration, Instant};

use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, GetSecretResponse, OwnerId, SecretRef, SecretValue,
    SharingMode, TenantId,
};
use tokio::time::sleep;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use authz_resolver_sdk::pep::ResourceType;

use crate::domain::authz::{self, actions, scope_for};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, FenceVerify, Outcome, ReadOutcome,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use credstore_sdk::SecretType;
use time::OffsetDateTime;

use crate::domain::secret::fence;
use crate::domain::secret::model::{
    NewSecret, SecretRow, SecretStatus, WritePrecondition, WriteSpec,
};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::type_resolver::{ResolvedSecretType, SecretTypeResolver};
use crate::domain::secret::typing;

/// Maps a [`CredStoreError`] from the plugin layer to a [`DomainError`].
#[must_use]
pub fn map_plugin_err(e: CredStoreError) -> DomainError {
    match e {
        CredStoreError::NotFound => DomainError::NotFound,
        CredStoreError::AccessDenied => DomainError::AccessDenied { cause: None },
        CredStoreError::ServiceUnavailable {
            detail,
            retry_after,
        } => {
            // The plugin's own detail may embed backend/infra specifics (a
            // future vault-backed plugin's error text is not curated for the
            // credstore boundary the way a CF-internal sibling's is, and could
            // in principle carry sensitive material — e.g. echo a token being
            // written). The wire already redacts it; for the same reason we do
            // NOT log the raw detail either (a credential store must not risk
            // secret material in its logs — review finding #4). We record only
            // its length so operators can still tell an empty detail from a
            // populated one when correlating an outage.
            tracing::warn!(
                detail_len = detail.len(),
                "credstore: storage plugin reported unavailable (detail redacted)"
            );
            DomainError::ServiceUnavailable {
                detail: "storage backend unavailable".to_owned(),
                retry_after,
                cause: None,
            }
        }
        // Operator misconfiguration, not a transient outage: keep a stable,
        // distinguishable detail and no `retry_after` so callers don't retry.
        CredStoreError::NoPluginAvailable => DomainError::ServiceUnavailable {
            detail: "no storage plugin registered".into(),
            retry_after: None,
            cause: None,
        },
        CredStoreError::Conflict => DomainError::Conflict,
        // These are plugin contract violations (a plugin should never return
        // them). Their free-text payloads originate in the plugin, and — like
        // the `ServiceUnavailable` detail above — a future non-CF backend's
        // error text is not curated for the credstore boundary and could carry
        // secret material (e.g. echo a token being written). A credential store
        // must not risk that in its own logs, so we drop the raw text and keep
        // only its length as an internal diagnostic (review finding #4).
        CredStoreError::InvalidSecretRef { reason } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned InvalidSecretRef (detail redacted, {} bytes)",
                reason.len()
            ),
            cause: None,
        },
        CredStoreError::UnsupportedTransition { detail } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned UnsupportedTransition (detail redacted, {} bytes)",
                detail.len()
            ),
            cause: None,
        },
        CredStoreError::TypeViolation { reason, detail } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned TypeViolation (detail redacted, {} bytes)",
                reason.len() + detail.len()
            ),
            cause: None,
        },
        CredStoreError::Internal(s) => DomainError::Internal {
            diagnostic: format!(
                "plugin returned Internal error (detail redacted, {} bytes)",
                s.len()
            ),
            cause: None,
        },
    }
}

/// Bounded retries when a PUT loses the create race (the winner holds a
/// provisioning row invisible to `find_for_write`); the winner marks active
/// within ms, so a small bounded budget resolves the race.
const CREATE_RACE_MAX_RETRIES: u32 = 3;

/// Small backoff between create-race retries; the winner marks active within ms.
const CREATE_RACE_RETRY_BACKOFF_MS: u64 = 25;

/// Upper bound on stale saga rows processed per reaper tick, so one tick's
/// backend-reconciliation work stays bounded; the remainder waits for the
/// next tick.
const REAP_BATCH_LIMIT: u64 = 256;

/// Reaper cadence and saga-timeout settings (from `ReaperCfg`).
#[domain_model]
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror the serialized ReaperCfg config keys"
)]
pub struct ReaperSettings {
    pub tick_secs: u64,
    pub provisioning_timeout_secs: u64,
    pub deprovisioning_timeout_secs: u64,
}

/// Re-read the cached fence key at least this often. Bounds the window in which
/// a replica that adopted a losing key during a bootstrap race keeps using it:
/// it converges on the stored key within this TTL even if it never hits a
/// verify mismatch. The backend read is cheap and the key almost never changes.
const FENCE_KEY_CACHE_TTL: Duration = Duration::from_mins(1);

/// Minimum spacing between fingerprint-mismatch–triggered backend re-reads of
/// the fence key. A crosswise last-writer-wins interleave produces a row whose
/// value fingerprint *never* matches (a by-design, fail-closed poison), so
/// without this every read of such a row would re-read the key from the
/// backend. The cooldown bounds those forced reloads to one per window per
/// replica. It gates on the *last forced reload* (not cache age), so the first
/// mismatch after a quiet window always re-reads and heals a genuinely stale
/// key; only the repeated, never-healing poison reads are suppressed.
const FENCE_KEY_REFRESH_COOLDOWN: Duration = Duration::from_secs(15);

/// The in-process fence key, shared out of the cache. Zeroizing so the
/// process's single highest-value secret is wiped from the heap on drop.
type FenceKey = Arc<zeroize::Zeroizing<Vec<u8>>>;

/// Cached fence key plus when it was loaded, so [`Service::fence_key`] can
/// re-read it from the backend after [`FENCE_KEY_CACHE_TTL`] and converge on
/// the stored key even without a verify mismatch to trigger a refresh.
#[domain_model]
struct CachedFenceKey {
    key: FenceKey,
    loaded_at: Instant,
}

/// `CredStore` domain service — get / put / delete with walk-up, saga, and `AuthZ`.
#[domain_model]
pub struct Service {
    repo: Arc<dyn SecretRepo>,
    dir: Arc<dyn TenantDirectory>,
    enforcer: PolicyEnforcer,
    plugins: Arc<dyn PluginSelector>,
    types: Arc<dyn SecretTypeResolver>,
    metrics: Arc<dyn CredStoreMetricsPort>,
    reaper: ReaperSettings,
    /// In-process cache of the value-fingerprint fence key (auto-generated,
    /// stored in the value-store backend under [`fence::FENCE_KEY_REF`]).
    /// A fingerprint mismatch triggers a cooldown-gated, single-flighted
    /// backend re-read (swapped in place, never evicted to `None`) so a replica
    /// holding a stale key self-heals without a poisoned row thrashing the
    /// cache. Never logged, never on the wire.
    /// Wrapped in [`zeroize::Zeroizing`] so the process's single highest-value
    /// secret (it fingerprints every stored value) is wiped from the heap when
    /// the cache is replaced or the process exits, matching how every
    /// `SecretValue` in the system zeroizes on drop.
    fence_key: std::sync::RwLock<Option<CachedFenceKey>>,
    /// Serialises fence-key (re)load on a single replica so concurrent first
    /// writers don't each run the bootstrap and race one another locally; the
    /// jitter protocol in [`Service::load_fence_key`] then only has to defend
    /// against *inter*-replica races.
    bootstrap_lock: tokio::sync::Mutex<()>,
    /// When the last fingerprint-mismatch–triggered *forced* reload of the
    /// fence key ran, gating the next one to [`FENCE_KEY_REFRESH_COOLDOWN`].
    /// `None` until the first forced reload, so the first mismatch always
    /// re-reads (healing a stale key); thereafter a poisoned row cannot drive
    /// more than one backend re-read per window. Independent of the cache's own
    /// load time.
    last_fence_refresh: std::sync::Mutex<Option<Instant>>,
}

/// Arguments for [`Service::overwrite_existing`] — bundled so the backend-first
/// overwrite doesn't carry a long positional argument list.
#[domain_model]
struct OverwriteArgs<'a> {
    ctx: &'a SecurityContext,
    scope: &'a AccessScope,
    plugin: &'a Arc<dyn CredStorePluginClientV1>,
    key: &'a SecretRef,
    value: SecretValue,
    sharing: SharingMode,
    existing_id: Uuid,
    expected_version: Option<i64>,
    expires_at: Option<OffsetDateTime>,
}

impl Service {
    /// Creates a new [`Service`].
    #[must_use]
    pub fn new(
        repo: Arc<dyn SecretRepo>,
        dir: Arc<dyn TenantDirectory>,
        enforcer: PolicyEnforcer,
        plugins: Arc<dyn PluginSelector>,
        types: Arc<dyn SecretTypeResolver>,
        metrics: Arc<dyn CredStoreMetricsPort>,
        reaper: ReaperSettings,
    ) -> Self {
        Self {
            repo,
            dir,
            enforcer,
            plugins,
            types,
            metrics,
            reaper,
            fence_key: std::sync::RwLock::new(None),
            bootstrap_lock: tokio::sync::Mutex::new(()),
            last_fence_refresh: std::sync::Mutex::new(None),
        }
    }

    /// Returns the configured reaper tick interval in seconds.
    #[must_use]
    pub fn reaper_tick_secs(&self) -> u64 {
        self.reaper.tick_secs
    }

    /// Evaluate the PDP scope for `action`, recording it as a timed dependency.
    ///
    /// The PDP call gates every get/put/delete and drives 503s, so it is timed
    /// like the plugin and tenant-resolver dependencies.
    async fn scope_for_timed(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        let t0 = Instant::now();
        let result = scope_for(&self.enforcer, ctx, resource, action).await;
        // A PDP *denial* (`AccessDenied`) is a normal authorization decision —
        // the dependency answered — not a health signal; counting it as an
        // error would inflate the PDP error rate and risk false outage alerts.
        // Only an evaluation failure/outage is a dependency error. Mirrors the
        // domain/transport split the type resolver makes.
        let outcome = match &result {
            Ok(_) | Err(DomainError::AccessDenied { .. }) => Outcome::Success,
            Err(_) => Outcome::Error,
        };
        self.metrics.dependency(
            Dep::Pdp,
            DepOp::Evaluate,
            outcome,
            t0.elapsed().as_secs_f64(),
        );
        result
    }

    /// Resolve the type of an **existing** row (`secret_type_uuid` was
    /// validated when the row was written). An `UNKNOWN_SECRET_TYPE`
    /// violation here means the type was deregistered while rows persist —
    /// an operational inconsistency, not a caller error — so it is remapped
    /// onto a retryable 503; registry outages propagate as 503 already.
    async fn resolve_stored(&self, type_uuid: Uuid) -> Result<ResolvedSecretType, DomainError> {
        match self.types.resolve(type_uuid).await {
            Err(DomainError::TypeViolation { detail, .. }) => {
                tracing::warn!(
                    uuid = %type_uuid,
                    detail = %detail,
                    "stored secret type no longer resolves in the types-registry"
                );
                Err(DomainError::ServiceUnavailable {
                    detail: "secret type is not resolvable".to_owned(),
                    retry_after: None,
                    cause: None,
                })
            }
            other => other,
        }
    }

    // ── Value-fingerprint fence (docs/features/001-value-fingerprint-fence.md) ──

    /// Internal service context for fence-key backend operations. Nil subject
    /// and nil tenant: plugins are pure value stores keyed by the explicit
    /// arguments, and no external caller ever carries the nil tenant, which is
    /// what keeps the reserved key unreachable through the API.
    fn fence_ctx() -> Result<(SecurityContext, TenantId, SecretRef), DomainError> {
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::nil())
            .subject_tenant_id(Uuid::nil())
            .build()
            .map_err(|e| {
                DomainError::internal(format!("fence: failed to build service context: {e}"))
            })?;
        let key_ref = SecretRef::new(fence::FENCE_KEY_REF).map_err(|e| {
            DomainError::internal(format!("fence: reserved key reference invalid: {e}"))
        })?;
        Ok((ctx, TenantId(Uuid::nil()), key_ref))
    }

    /// The cached fence key, loading (and on first boot generating) it from the
    /// value-store backend when the cache is cold or older than
    /// [`FENCE_KEY_CACHE_TTL`]. The TTL re-read lets a replica that adopted a
    /// losing key during a bootstrap race converge on the stored key without
    /// waiting for a verify mismatch.
    async fn fence_key(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV1>,
    ) -> Result<FenceKey, DomainError> {
        {
            let guard = self
                .fence_key
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = guard.as_ref()
                && cached.loaded_at.elapsed() < FENCE_KEY_CACHE_TTL
            {
                return Ok(Arc::clone(&cached.key));
            }
        }
        self.load_fence_key(plugin, false).await
    }

    /// Re-read the fence key from the backend — the self-heal a fingerprint
    /// mismatch triggers, so a replica whose cache went stale (key re-created
    /// under it) converges without restart.
    ///
    /// Unlike a naive "evict + reload", this must not turn a *poisoned* row (a
    /// permanent, by-design mismatch from a crosswise LWW interleave) into a
    /// backend read per request nor evict the shared key for every concurrent
    /// operation. Two guards, both delegated to [`Self::load_fence_key`] under
    /// its `bootstrap_lock` (which also single-flights concurrent callers):
    /// the reload is skipped when the cache was (re)loaded within
    /// [`FENCE_KEY_REFRESH_COOLDOWN`] (a re-read cannot help inside the window),
    /// and the cache is swapped in place rather than cleared to `None`.
    async fn refresh_fence_key(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV1>,
    ) -> Result<FenceKey, DomainError> {
        self.load_fence_key(plugin, true).await
    }

    /// (Re)load the fence key from the backend's reserved entry.
    ///
    /// The plugin port has no atomic create-if-absent, so first-boot generation
    /// cannot be a true single-writer operation. Instead we make a bootstrap
    /// race both **unlikely** and **low-impact**:
    ///
    /// * A per-replica [`Self::bootstrap_lock`] serialises this so concurrent
    ///   first writers on one replica don't race each other locally.
    /// * When the key is absent we jitter, re-check, and only then generate and
    ///   `put`, de-synchronising replicas that cold-start together. The
    ///   publishing `put` is unconditional (the port has no create-if-absent),
    ///   so it can still clobber a peer whose write landed in the tiny window
    ///   between our re-check and our `put` — that residual race is the
    ///   fail-closed case below.
    /// * After publishing we jitter again and **adopt whatever is stored** —
    ///   ours if we won, a racing writer's if theirs landed last — rather than
    ///   re-`put`ting. The cache is populated only from this settle read, so no
    ///   value is ever stamped with a candidate that hasn't survived it.
    ///
    /// The key has no metadata row, so it is invisible to every API path. Any
    /// residual divergence is fail-closed: a wrong key only ever yields a
    /// fingerprint mismatch (404 + self-heal on rewrite), never a value served
    /// under foreign metadata.
    ///
    /// `force` distinguishes the two entry points. A normal load (`false`) is
    /// satisfied by any cache younger than [`FENCE_KEY_CACHE_TTL`]. A refresh
    /// (`true`, from a fingerprint mismatch) re-reads the backend unless a prior
    /// forced reload ran within [`FENCE_KEY_REFRESH_COOLDOWN`] — so the first
    /// mismatch always re-reads and heals a genuinely stale key, while a
    /// poisoned row (a permanent, never-healing mismatch) is bounded to one
    /// re-read per window. The `bootstrap_lock` single-flights either path, so a
    /// burst of concurrent callers yields at most one backend read.
    /// Whether a forced fence-key reload ran within [`FENCE_KEY_REFRESH_COOLDOWN`].
    /// `false` until the first forced reload, so an initial mismatch is never
    /// suppressed.
    fn fence_refresh_within_cooldown(&self) -> bool {
        self.last_fence_refresh
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|at| at.elapsed() < FENCE_KEY_REFRESH_COOLDOWN)
    }

    async fn load_fence_key(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV1>,
        force: bool,
    ) -> Result<FenceKey, DomainError> {
        let _bootstrap = self.bootstrap_lock.lock().await;
        // Reuse the cache rather than re-reading the backend when: a non-forced
        // load finds it still within the TTL, or a forced refresh finds a prior
        // forced reload within the cooldown (re-reading sooner cannot heal a
        // poisoned row). A concurrent caller may also have just reloaded while
        // we waited on the lock — this same check picks that up.
        {
            let guard = self
                .fence_key
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = guard.as_ref() {
                let reuse = if force {
                    self.fence_refresh_within_cooldown()
                } else {
                    cached.loaded_at.elapsed() < FENCE_KEY_CACHE_TTL
                };
                if reuse {
                    return Ok(Arc::clone(&cached.key));
                }
            }
        }
        // About to re-read on a mismatch: stamp the forced-reload clock now so
        // repeated poisoned mismatches (and concurrent ones behind the lock)
        // coalesce, even if the read below fails.
        if force {
            *self
                .last_fence_refresh
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
        }

        let (ctx, tenant, key_ref) = Self::fence_ctx()?;
        // Fast path: the key already exists (every start after the first, and
        // most concurrent cold starts once one replica has written it).
        if let Some(v) = plugin
            .get(&ctx, &tenant, &key_ref, None)
            .await
            .map_err(map_plugin_err)?
        {
            return Ok(self.store_fence_key(zeroize::Zeroizing::new(v.as_bytes().to_vec())));
        }

        // Cold-start bootstrap. Jitter, then re-check: a peer may have won.
        Self::bootstrap_jitter().await;
        if let Some(v) = plugin
            .get(&ctx, &tenant, &key_ref, None)
            .await
            .map_err(map_plugin_err)?
        {
            return Ok(self.store_fence_key(zeroize::Zeroizing::new(v.as_bytes().to_vec())));
        }

        // Still absent: publish a candidate, settle, then adopt what landed.
        // Keep the material in zeroizing buffers so no plain `Vec<u8>` copy
        // lingers on the heap after this scope.
        let candidate = zeroize::Zeroizing::new(fence::generate_key()?);
        plugin
            .put(
                &ctx,
                &tenant,
                &key_ref,
                SecretValue::new(candidate.to_vec()),
                None,
            )
            .await
            .map_err(map_plugin_err)?;
        Self::bootstrap_jitter().await;
        let stored = plugin
            .get(&ctx, &tenant, &key_ref, None)
            .await
            .map_err(map_plugin_err)?
            // Our own write vanishing is not expected; fall back to the
            // candidate so bootstrap still makes progress (fail-closed).
            .map_or(candidate, |v| {
                zeroize::Zeroizing::new(v.as_bytes().to_vec())
            });
        Ok(self.store_fence_key(stored))
    }

    /// Publish `key_bytes` as the cached fence key, timestamped for the TTL
    /// re-read, and hand back the shared handle.
    fn store_fence_key(&self, key_bytes: zeroize::Zeroizing<Vec<u8>>) -> FenceKey {
        let key: FenceKey = Arc::new(key_bytes);
        *self
            .fence_key
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedFenceKey {
            key: Arc::clone(&key),
            loaded_at: Instant::now(),
        });
        key
    }

    /// Sleep a random `0..=FENCE_BOOTSTRAP_JITTER_MAX_MS` to de-synchronise
    /// replicas racing the first-boot fence-key generation.
    async fn bootstrap_jitter() {
        use rand::RngExt as _;
        let ms = rand::rng().random_range(0..=fence::BOOTSTRAP_JITTER_MAX_MS);
        sleep(Duration::from_millis(ms)).await;
    }

    /// Fingerprint `value` under the current fence key (write-path stamping).
    async fn stamp_fp(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV1>,
        value: &SecretValue,
    ) -> Result<Vec<u8>, DomainError> {
        let key = self.fence_key(plugin).await?;
        Ok(fence::compute_fp(key.as_slice(), value.as_bytes()))
    }

    /// Best-effort lazy fingerprint backfill of an out-of-band seeded row
    /// (`value_fp IS NULL`), using the value just served. CAS on NULL in the
    /// repo, so a concurrent PUT that already stamped wins; never bumps the
    /// version (the caller's `ETag` stays stable). Failures only log + count —
    /// the read that triggered the backfill already succeeded.
    async fn backfill_row_fp(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV1>,
        row: &SecretRow,
        value: &SecretValue,
    ) {
        let fp = match self.stamp_fp(plugin, value).await {
            Ok(fp) => fp,
            Err(e) => {
                tracing::warn!(
                    id = %row.id,
                    err = %e,
                    "credstore: fence backfill skipped (fence key unavailable); retried on a later read/sweep"
                );
                self.metrics.fence_backfill(Outcome::Error);
                return;
            }
        };
        let outcome = match self
            .repo
            .backfill_fp(row.id, fp, fence::CURRENT_FENCE_KEY_ID)
            .await
        {
            Ok(true) => Outcome::Success,
            // CAS matched 0 rows: a concurrent PUT already stamped — done.
            Ok(false) => Outcome::NotFound,
            Err(e) => {
                tracing::warn!(
                    id = %row.id,
                    err = %e,
                    "credstore: fence backfill failed; retried on a later read/sweep"
                );
                Outcome::Error
            }
        };
        self.metrics.fence_backfill(outcome);
    }

    /// Retrieve a secret, walking up the tenant hierarchy.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is out of scope.
    /// Returns [`DomainError::NotFound`] if the plugin has no value for a resolved row.
    pub async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, DomainError> {
        let req = TenantId(ctx.subject_tenant_id());
        let subject = OwnerId(ctx.subject_id());
        let chain = self.dir.ancestor_chain(ctx, req).await?;

        // Resolve first (prefetch — AUTHZ_USAGE_SCENARIOS S09): a secret that does
        // not resolve is a 404 without consulting the PDP; there is nothing to
        // authorize.
        let Some(row) = self.repo.resolve_for_get(req, subject, key, &chain).await? else {
            self.metrics.read_outcome(ReadOutcome::Miss);
            return Ok(None);
        };

        // Single PDP evaluation, on the secret's full concrete type (including
        // `generic`) as resolved from the types-registry, gated on the
        // *caller's* tenant. Hierarchical visibility (a shared secret
        // inherited from an ancestor) is decided by the resolver above, so
        // the gate uses `req`, not the row's owner tenant — this is what lets
        // inherited reads work. A PDP denial or an out-of-scope tenant is
        // indistinguishable from a missing secret (anti-enumeration 404); a
        // PDP or registry *outage* propagates as 503.
        //
        // The type resolve necessarily precedes the PDP (the PDP resource *is*
        // the resolved concrete type), so an outage window is the one case
        // where an unauthorized caller can distinguish an existing secret (503)
        // from a missing one (404). This is an accepted, bounded consequence of
        // failing closed, symmetric across the PDP and registry dependencies —
        // do NOT "fix" it by returning 404 on an outage, which would mask a real
        // outage from authorized callers.
        let resolved = self.resolve_stored(row.secret_type_uuid).await?;
        let scope = match self
            .scope_for_timed(
                ctx,
                &authz::secret_type_resource(&resolved.gts_id),
                actions::READ,
            )
            .await
        {
            Ok(scope) => scope,
            Err(DomainError::AccessDenied { .. }) => {
                self.metrics.read_outcome(ReadOutcome::Miss);
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        if !self.repo.scope_includes_tenant(&scope, req.0).await? {
            self.metrics.cross_tenant_denied();
            self.metrics.read_outcome(ReadOutcome::Miss);
            return Ok(None);
        }

        let depth = chain
            .iter()
            .position(|c| *c == row.tenant_id.0)
            .unwrap_or(chain.len());
        self.metrics.walkup_depth(depth as u64);

        let owner = if row.sharing == SharingMode::Private {
            Some(row.owner_id)
        } else {
            None
        };

        let plugin = self.plugins.resolve().await?;
        let t0 = Instant::now();
        let result = plugin
            .get(ctx, &row.tenant_id, key, owner.as_ref())
            .await
            .map_err(map_plugin_err);
        let secs = t0.elapsed().as_secs_f64();
        let outcome = match &result {
            Ok(Some(_)) => Outcome::Success,
            Ok(None) => Outcome::NotFound,
            Err(_) => Outcome::Error,
        };
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginGet, outcome, secs);
        let value: SecretValue = match result {
            Ok(Some(v)) => v,
            // The row resolved but the backend has no value — a 404 (rare; a
            // storage artifact of a mid-saga row).
            Ok(None) => return Err(DomainError::NotFound),
            // A backend-plugin denial on read must NOT distinguish an existing
            // secret from a missing one: fold it into the anti-enumeration miss,
            // exactly like the PDP denial above. The bundled value-store never
            // denies; this guards a future backend whose own ACLs could deny
            // after the gear's PDP pass.
            Err(DomainError::AccessDenied { .. }) => {
                self.metrics.read_outcome(ReadOutcome::Miss);
                return Ok(None);
            }
            Err(e) => return Err(e),
        };

        // Fence verification (feature spec `flow-get-fenced-read`): the value
        // is served only when its fingerprint matches the row that authorized
        // it. A mismatch means the backend value and the metadata row are not
        // from the same writer (crosswise LWW interleave, a mid-saga artifact,
        // or a stale out-of-band re-seed) — serving it could put one writer's
        // value under another writer's sharing label, so fail closed as an
        // anti-enumeration miss. A row without a fingerprint is out-of-band
        // seeded: served on trust and backfilled best-effort.
        if let Some(stored_fp) = &row.value_fp {
            // TODO(rotation): verify under the key selected by `row.fp_key_id`,
            // not the single current key. Equivalent for v1 (one key, id
            // `CURRENT_FENCE_KEY_ID`), but once a keyring exists every
            // pre-rotation row would fail closed here — see feature spec §3
            // `inst-fcv-2` / `algo-fp-compute-verify`.
            let fkey = self.fence_key(&plugin).await?;
            let mut fp_ok = fence::verify_fp(fkey.as_slice(), value.as_bytes(), stored_fp);
            if !fp_ok {
                // One-shot key refresh: a replica whose cached key went stale
                // (key re-created under it) self-heals before failing closed.
                let fkey = self.refresh_fence_key(&plugin).await?;
                fp_ok = fence::verify_fp(fkey.as_slice(), value.as_bytes(), stored_fp);
            }
            if fp_ok {
                self.metrics.fence_verify(FenceVerify::Ok);
            } else {
                self.metrics.fence_verify(FenceVerify::Mismatch);
                self.metrics.read_outcome(ReadOutcome::Miss);
                tracing::warn!(
                    tenant = %row.tenant_id.0,
                    key = %key.as_ref(),
                    "credstore get: value fingerprint mismatch; failing closed \
                     (backend value does not match the metadata row; heals on the next successful put)"
                );
                return Ok(None);
            }
        } else {
            self.metrics.fence_verify(FenceVerify::Legacy);
            self.backfill_row_fp(&plugin, &row, &value).await;
        }

        let is_inherited = row.tenant_id != req;
        self.metrics.read_outcome(if is_inherited {
            ReadOutcome::HitInherited
        } else {
            ReadOutcome::HitOwn
        });

        Ok(Some(GetSecretResponse {
            value,
            id: row.id,
            owner_tenant_id: row.tenant_id,
            sharing: row.sharing,
            is_inherited,
            version: row.version,
            secret_type: resolved.gts_id,
            expires_at: row.expires_at,
        }))
    }

    /// Create or update a secret.
    ///
    /// A write targets the row of its own sharing class — `private` →
    /// `(tenant, ref, owner)`, `tenant`/`shared` → `(tenant, ref)` — so a private
    /// and a tenant/shared secret coexist under one reference (per design §4.1);
    /// a write of one class never affects the other.
    ///
    /// Secret-type traits (design §5.4) are enforced before any side effect:
    /// the type is immutable for an existing secret (`spec.opts.secret_type`
    /// must be absent or equal), defaults to `generic` on create, and the
    /// value/sharing/expiry are validated against the type's traits as
    /// resolved from the types-registry.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Conflict`] if `spec.create_only` and a secret of
    /// the same sharing class already exists.
    /// Returns [`DomainError::TypeViolation`] on a trait violation.
    // Saga orchestration (validate -> scope -> resolve -> backend -> commit) is
    // inherently branchy; kept as one function for readability of the flow.
    #[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
    pub async fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        spec: WriteSpec,
    ) -> Result<(), DomainError> {
        let WriteSpec {
            sharing,
            create_only,
            precondition,
            opts,
            preserve_sharing,
        } = spec;
        let tenant = TenantId(ctx.subject_tenant_id());
        let owner = OwnerId(ctx.subject_id());

        // Fail fast if no plugin is available before touching metadata.
        let plugin = self.plugins.resolve().await?;

        // When `sharing` was omitted (`preserve_sharing`), a value rotation
        // targets the secret the owner would GET: a `private` secret is
        // owner-scoped and wins over a coexisting non-private one, so rotate the
        // private row if the caller has one under this reference; otherwise fall
        // through to the non-private (tenant/shared) class. Without this an
        // omitted-sharing PUT over a private-only reference would silently
        // create a tenant row that GET never returns (the PUT 204s but the
        // owner's GET still sees the old private value).
        let sharing = if preserve_sharing
            && self
                .repo
                .find_for_write(
                    &AccessScope::allow_all(),
                    tenant,
                    owner,
                    key,
                    SharingMode::Private,
                )
                .await?
                .is_some()
        {
            SharingMode::Private
        } else {
            sharing
        };

        // Prefetch the target row of this sharing class (own-tenant, keyed by
        // tenant+owner+key+sharing) with `allow_all`; the single PDP evaluation
        // runs once the secret's concrete type is known — on create from the
        // requested/default type, on overwrite from the existing row (per
        // AUTHZ_USAGE_SCENARIOS S10/S11).
        if let Some(existing) = self
            .repo
            .find_for_write(&AccessScope::allow_all(), tenant, owner, key, sharing)
            .await?
        {
            // Traits + PDP resource come from the registry resolution of the
            // row's (immutable) type.
            let resolved = self.resolve_stored(existing.secret_type_uuid).await?;
            // Authorize on the concrete type BEFORE any 4xx that would reveal
            // the row exists: an unauthorized caller must not be able to tell a
            // create-only conflict (409) or a trait/immutability violation (400)
            // apart from a plain denial (the own-tenant reference-enumeration
            // oracle). The single PDP evaluation is gated on the caller's tenant.
            let scope = self
                .scope_for_timed(
                    ctx,
                    &authz::secret_type_resource(&resolved.gts_id),
                    actions::WRITE,
                )
                .await?;
            if !self.repo.scope_includes_tenant(&scope, tenant.0).await? {
                self.metrics.cross_tenant_denied();
                return Err(DomainError::AccessDenied { cause: None });
            }

            if create_only {
                return Err(DomainError::Conflict);
            }
            // A PUT that omitted `sharing` (`preserve_sharing`) keeps the stored
            // mode, so a value rotation never silently narrows a `shared` secret
            // back to `tenant` (review finding #8). An explicit `sharing` still
            // takes effect. `sharing` remains the class selector used above.
            let effective_sharing = if preserve_sharing {
                existing.sharing
            } else {
                sharing
            };
            // find_for_write addresses the target sharing class, so a write never
            // crosses the private boundary here — a private and a tenant/shared
            // secret coexist under one ref (per design §4.1). The guard stays as a
            // defensive invariant; tenant↔shared is the only real in-place change.
            if (existing.sharing == SharingMode::Private)
                != (effective_sharing == SharingMode::Private)
            {
                return Err(DomainError::UnsupportedTransition {
                    detail: "cannot move between private and tenant/shared".into(),
                });
            }
            // The type is immutable: an explicit differing type is rejected;
            // an absent one inherits the row's type for trait validation.
            if let Some(requested) = opts.secret_type.as_ref()
                && requested.to_uuid() != existing.secret_type_uuid
            {
                return Err(DomainError::TypeViolation {
                    field: "type",
                    reason: typing::reasons::TYPE_IMMUTABLE,
                    detail: format!(
                        "secret is of type '{}'; changing it to '{}' is not supported",
                        resolved.gts_id, requested
                    ),
                });
            }
            typing::validate_write(
                &resolved.gts_id,
                &resolved.traits,
                effective_sharing,
                &value,
                opts.expires_at.requested(),
            )?;
            let expected_version = Self::precheck_version(precondition.as_ref(), &existing)?;
            return self
                .overwrite_existing(OverwriteArgs {
                    ctx,
                    scope: &scope,
                    plugin: &plugin,
                    key,
                    value,
                    sharing: effective_sharing,
                    existing_id: existing.id,
                    expected_version,
                    expires_at: opts.expires_at.resolve(existing.expires_at),
                })
                .await;
        }

        // Create path: the target does not exist. An If-Match precondition
        // requires an existing target, so any precondition fails here.
        //
        // Note (own-tenant existence): this 409 lands before the create-path
        // PDP evaluation below, so an unauthorized caller sending `If-Match`
        // can tell "reference absent" (409 here) from "reference present"
        // (the existing-row branch above runs PDP first → 403). That is the
        // same own-tenant existence signal DELETE exposes by design (see the
        // delete-path note); it is an accepted part of the module threat model
        // (a tenant member may enumerate their own tenant's references), not a
        // cross-tenant leak — the read path still folds cross-tenant existence
        // into an anti-enumeration 404. The existing-row branch keeps PDP-first
        // because there a 409/400 would otherwise distinguish a create-only
        // conflict or a trait violation on a reference the caller cannot write.
        if precondition.is_some() {
            return Err(DomainError::VersionConflict);
        }

        // Create path: the type defaults to generic; traits + per-type access
        // validated before any side effect. The caller named the type, so an
        // unresolvable one propagates as 400 UNKNOWN_SECRET_TYPE.
        let type_uuid = opts
            .secret_type
            .map_or_else(|| SecretType::generic().uuid(), |id| id.to_uuid());
        let resolved = self.types.resolve(type_uuid).await?;
        typing::validate_write(
            &resolved.gts_id,
            &resolved.traits,
            sharing,
            &value,
            opts.expires_at.requested(),
        )?;
        // Single PDP evaluation on the concrete type being created, gated on the
        // caller's tenant.
        let scope = self
            .scope_for_timed(
                ctx,
                &authz::secret_type_resource(&resolved.gts_id),
                actions::WRITE,
            )
            .await?;
        if !self.repo.scope_includes_tenant(&scope, tenant.0).await? {
            self.metrics.cross_tenant_denied();
            return Err(DomainError::AccessDenied { cause: None });
        }

        // Create saga: insert provisioning → plugin.put → mark_active. The
        // fence fingerprint of the value this saga will write travels in the
        // INSERT, so the row is fenced from its first readable instant.
        let value_fp = self.stamp_fp(&plugin, &value).await?;
        let id = Uuid::new_v4();
        let new = NewSecret {
            id,
            tenant_id: tenant,
            reference: key.clone(),
            sharing,
            owner_id: owner,
            secret_type_uuid: type_uuid,
            expires_at: opts.expires_at.resolve(None),
            value_fp,
            fp_key_id: fence::CURRENT_FENCE_KEY_ID,
        };
        match self.repo.insert_provisioning(&scope, &new).await {
            Ok(()) => {}
            Err(DomainError::Conflict) => {
                // Lost the create race. A create-only POST surfaces the 409
                // directly; a PUT resolves to an update once the winner (whose
                // provisioning row is invisible to find_for_write) marks active.
                if create_only {
                    return Err(DomainError::Conflict);
                }
                for _ in 0..CREATE_RACE_MAX_RETRIES {
                    if let Some(existing) = self
                        .repo
                        .find_for_write(&scope, tenant, owner, key, sharing)
                        .await?
                    {
                        // Losing the race to a different type is a conflict on
                        // the immutable type, same as the explicit-mismatch path.
                        if existing.secret_type_uuid != type_uuid {
                            // Render the winner's type as its GTS id (like the
                            // overwrite path), not a raw UUID, so both sides of
                            // the message are in the same namespace.
                            let existing_resolved =
                                self.resolve_stored(existing.secret_type_uuid).await?;
                            return Err(DomainError::TypeViolation {
                                field: "type",
                                reason: typing::reasons::TYPE_IMMUTABLE,
                                detail: format!(
                                    "secret is of type '{}'; changing it to '{}' is not supported",
                                    existing_resolved.gts_id, resolved.gts_id
                                ),
                            });
                        }
                        // Preserve the winner's sharing when the caller omitted
                        // it, exactly like the direct overwrite branch — else a
                        // rotation that lost the create race could still narrow
                        // a `shared` secret back to `tenant` (finding #8).
                        let effective_sharing = if preserve_sharing {
                            existing.sharing
                        } else {
                            sharing
                        };
                        let expected_version =
                            Self::precheck_version(precondition.as_ref(), &existing)?;
                        return self
                            .overwrite_existing(OverwriteArgs {
                                ctx,
                                scope: &scope,
                                plugin: &plugin,
                                key,
                                value,
                                sharing: effective_sharing,
                                existing_id: existing.id,
                                expected_version,
                                expires_at: opts.expires_at.resolve(existing.expires_at),
                            })
                            .await;
                    }
                    sleep(Duration::from_millis(CREATE_RACE_RETRY_BACKOFF_MS)).await;
                }
                tracing::warn!(
                    tenant = %tenant.0,
                    key = %key.as_ref(),
                    "credstore put: create-race retry budget exhausted; winner appears stuck mid-saga, returning retry-safe Conflict"
                );
                return Err(DomainError::Conflict);
            }
            Err(e) => return Err(e),
        }

        let plugin_owner = if sharing == SharingMode::Private {
            Some(owner)
        } else {
            None
        };
        let t0 = Instant::now();
        let result = plugin
            .put(ctx, &tenant, key, value, plugin_owner.as_ref())
            .await
            .map_err(map_plugin_err);
        let secs = t0.elapsed().as_secs_f64();
        let outcome = if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Error
        };
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginPut, outcome, secs);
        if let Err(e) = result {
            // Compensate the saga: the backend write failed, so roll back the
            // provisioning row. The partial unique index ignores status, so a
            // lingering provisioning row wedges the reference until the reaper
            // runs — a misleading 409 for POST, exhausted retries for PUT.
            // Best-effort: a failed cleanup just defers to the reaper.
            let rollback = match self.repo.delete_by_id(&scope, id, None).await {
                Ok(()) => Outcome::Success,
                // Row already gone (concurrent reap/delete): not wedged.
                Err(DomainError::NotFound) => Outcome::NotFound,
                Err(cleanup_err) => {
                    tracing::warn!(
                        tenant = %tenant.0,
                        key = %key.as_ref(),
                        err = %cleanup_err,
                        "credstore put: provisioning rollback after backend write failure failed; reference stays wedged until reaped"
                    );
                    Outcome::Error
                }
            };
            self.metrics.provisioning_rollback(rollback);
            return Err(e);
        }

        // Transient orphaned backend value: a mark_active failure after a
        // successful plugin.put leaves the value in the backend while the row
        // stays provisioning. It is never readable (no active row) — a storage
        // artifact, not a disclosure — and it is self-healing: a client retry
        // overwrites it, and absent a retry the reaper reaps the stale
        // provisioning row *and* issues a best-effort plugin.delete for it (see
        // `reap_row` → `reap_backend_delete`), so the value is reconciled after
        // `provisioning_timeout_secs` + one reaper tick. Surfaced operationally
        // via the warn below and the provisioning_reaped metric.
        if let Err(e) = self.repo.mark_active(&scope, id).await {
            tracing::warn!(
                tenant = %tenant.0,
                key = %key.as_ref(),
                err = %e,
                "credstore put: mark_active failed after backend write; backend value orphaned until reaped/retried"
            );
            return Err(e);
        }
        Ok(())
    }

    /// Optimistic-concurrency pre-check before any backend write: returns the
    /// `version = ?` filter to gate `touch` on, or `VersionConflict` if the
    /// caller's `If-Match` validator disagrees with the current row. The
    /// validator is generation-bound (`"<id>.<version>"`): a mismatched row
    /// id means the caller's validator is from a deleted-and-recreated
    /// secret's earlier generation and must never match, even when the
    /// per-generation version counters coincide (no ABA). The row-level CAS
    /// keeps gating on `version` alone — `id` is already the UPDATE key.
    fn precheck_version(
        precondition: Option<&WritePrecondition>,
        existing: &SecretRow,
    ) -> Result<Option<i64>, DomainError> {
        match precondition {
            Some(WritePrecondition::Version { id, version }) => {
                if *id != existing.id || *version != existing.version {
                    return Err(DomainError::VersionConflict);
                }
                Ok(Some(*version))
            }
            // Multi-valued If-Match: satisfied if any listed validator matches
            // the current row's (id, version). The CAS still gates on the row's
            // own version (the matched one equals `existing.version`).
            Some(WritePrecondition::AnyVersion(validators)) => {
                if validators
                    .iter()
                    .any(|(id, version)| *id == existing.id && *version == existing.version)
                {
                    Ok(Some(existing.version))
                } else {
                    Err(DomainError::VersionConflict)
                }
            }
            Some(WritePrecondition::Exists) | None => Ok(None),
        }
    }

    /// Overwrite an existing secret. The ordering of the backend write vs the
    /// metadata version bump depends on whether the caller supplied `If-Match`;
    /// both orders stamp the fence fingerprint in the same `touch`, so any
    /// half-completed overwrite reads fail-closed instead of serving a value
    /// under metadata written for a different one:
    ///
    /// * **No precondition** (last-writer-wins): backend-first — `plugin.put`
    ///   then a `touch` version bump. A plugin failure leaves metadata
    ///   untouched (the previous value still verifies); a `touch` failure
    ///   after a committed backend write leaves the row fingerprinting the
    ///   previous value, so reads 404 (fence mismatch) until the next put
    ///   retries. Two crosswise concurrent LWW puts can end with one writer's
    ///   value under the other's row — the fence turns that from a
    ///   cross-tenant disclosure into a fail-closed 404 healed by any
    ///   subsequent successful put.
    /// * **`If-Match`** (optimistic concurrency): version-claim-first — the
    ///   version-gated `touch` runs BEFORE `plugin.put`, so a losing concurrent
    ///   writer (`touch` matches 0 rows) never reaches the backend and cannot
    ///   clobber the winner's value. The tradeoff inverts: if the backend write
    ///   then fails, the row already fingerprints the value that never landed,
    ///   so reads 404 (fence mismatch, fail-closed — not the previous value)
    ///   until the caller retries against the new version.
    ///
    /// Shared by the normal overwrite path and the create-race resolution path.
    #[allow(clippy::cognitive_complexity)]
    async fn overwrite_existing(&self, args: OverwriteArgs<'_>) -> Result<(), DomainError> {
        let OverwriteArgs {
            ctx,
            scope,
            plugin,
            key,
            value,
            sharing,
            existing_id,
            expected_version,
            expires_at,
        } = args;
        let tenant = TenantId(ctx.subject_tenant_id());
        let owner = OwnerId(ctx.subject_id());
        let plugin_owner = if sharing == SharingMode::Private {
            Some(owner)
        } else {
            None
        };

        // Fence stamp of the value this overwrite writes: `touch` persists it
        // in the same atomic UPDATE as the sharing label, so metadata and
        // fingerprint always come from one writer. On any interleaving where
        // the backend value ends up from a different writer than the row, the
        // read-side verification fails closed instead of serving the value
        // under foreign metadata.
        let value_fp = self.stamp_fp(plugin, &value).await?;

        // If-Match: claim the version bump first (see the fn doc). A 0-row
        // `touch` means the version moved or the row vanished — the caller lost
        // the CAS and, crucially, has NOT written the backend, so a concurrent
        // writer's value can never be clobbered.
        if expected_version.is_some() {
            match self
                .repo
                .touch(
                    scope,
                    existing_id,
                    sharing,
                    expected_version,
                    expires_at,
                    value_fp,
                )
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => {
                    tracing::warn!(
                        tenant = %tenant.0,
                        key = %key.as_ref(),
                        "credstore put: If-Match version precondition lost before the backend write (concurrent write/delete); no backend value written"
                    );
                    return Err(DomainError::VersionConflict);
                }
                Err(e) => return Err(e),
            }
            let t0 = Instant::now();
            let result = plugin
                .put(ctx, &tenant, key, value, plugin_owner.as_ref())
                .await
                .map_err(map_plugin_err);
            let secs = t0.elapsed().as_secs_f64();
            self.metrics.dependency(
                Dep::Plugin,
                DepOp::PluginPut,
                if result.is_ok() {
                    Outcome::Success
                } else {
                    Outcome::Error
                },
                secs,
            );
            if let Err(e) = &result {
                tracing::warn!(
                    tenant = %tenant.0,
                    key = %key.as_ref(),
                    err = %e,
                    "credstore put: backend write failed after the version was claimed; reads fail closed (fence mismatch) until a retried put lands the value"
                );
            }
            return result;
        }

        // No precondition: last-writer-wins, backend-first.
        let t0 = Instant::now();
        let result = plugin
            .put(ctx, &tenant, key, value, plugin_owner.as_ref())
            .await
            .map_err(map_plugin_err);
        let secs = t0.elapsed().as_secs_f64();
        let outcome = if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Error
        };
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginPut, outcome, secs);
        result?;

        // Version bump second. A failure here (after the backend write committed)
        // leaves the row fingerprinting the previous value, so reads fail closed
        // (fence mismatch → 404) until the next put retries.
        match self
            .repo
            .touch(
                scope,
                existing_id,
                sharing,
                expected_version,
                expires_at,
                value_fp,
            )
            .await
        {
            Ok(Some(_)) => Ok(()),
            // Row concurrently deleted/reaped: the backend write already committed
            // but no active metadata row exists, so the secret would be unreadable
            // despite an acknowledged write. Surface the lost race as a retryable
            // conflict (canonical Aborted/409) so the caller re-runs the put, which
            // recreates the metadata row deterministically.
            //
            // Orphaned backend value (accepted, review finding #3): the value
            // this put wrote now sits in the backend under `(tenant, ref[,
            // owner])` with no metadata row. It is NOT readable (resolution
            // needs a row) and it is NOT a disclosure, but if the caller does
            // not retry it lingers as plaintext-at-rest until a future create
            // of the same reference overwrites it — the reaper works off rows,
            // so it never sees a row-less backend value. A compensating delete
            // here is unsafe (it could erase a concurrent successor's value at
            // the same backend key), so this is accept-and-document: the retry
            // (the common case) reconciles it; the no-retry orphan is bounded
            // by the next create at that reference.
            Ok(None) => {
                tracing::warn!(
                    tenant = %tenant.0,
                    key = %key.as_ref(),
                    "credstore put: row vanished before version bump (concurrent delete/reap); returning conflict so the caller retries"
                );
                Err(DomainError::VersionConflict)
            }
            Err(e) => {
                tracing::warn!(
                    tenant = %tenant.0,
                    key = %key.as_ref(),
                    err = %e,
                    "credstore put: version bump (touch) failed after backend write; version/sharing metadata lags the backend until the next put retries"
                );
                Err(e)
            }
        }
    }

    /// Delete an owned secret via the deprovisioning saga:
    /// mark `deprovisioning` (the secret atomically stops resolving, the row
    /// keeps holding the unique index) → backend delete → row delete.
    ///
    /// A retry of `DELETE` resumes a stuck saga: `find_own` returns the
    /// `deprovisioning` row and the backend/row steps re-run idempotently.
    /// Absent a retry, the reaper completes the saga (`reap_and_refresh`).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotFound`] if no own-tenant row exists for the key.
    /// Returns [`DomainError::VersionConflict`] if `precondition` is set and the
    /// current version does not satisfy it.
    // Saga orchestration (scope -> gate -> mark -> backend -> commit) is
    // inherently branchy; kept as one function for readability of the flow.
    #[allow(clippy::cognitive_complexity)]
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        precondition: Option<WritePrecondition>,
    ) -> Result<(), DomainError> {
        let tenant = TenantId(ctx.subject_tenant_id());
        let owner = OwnerId(ctx.subject_id());

        // Prefetch the caller's own row (allow_all, keyed by tenant+owner+key);
        // a missing row is 404 without consulting the PDP.
        let Some(row) = self
            .repo
            .find_own(&AccessScope::allow_all(), tenant, owner, key)
            .await?
        else {
            return Err(DomainError::NotFound);
        };

        // Optimistic-concurrency gate, in two layers mirroring the put path.
        // `Exists` is satisfied by find_own; a `Version`/`AnyVersion`
        // precondition becomes:
        //   1. a generation-bound pre-check here, before any side effect —
        //      both the row id (a validator minted for a recreated secret's
        //      earlier generation must never match, no ABA) and the version;
        //   2. the same `version = ?` filter on mark_deprovisioning below
        //      (mark_deprovisioning does not bump the version, so a saga
        //      resume still sees the version the client knew; the id is
        //      already the UPDATE key).
        let expected_version = Self::precheck_version(precondition.as_ref(), &row)?;

        // Single PDP evaluation on the secret's full concrete type, gated on the
        // caller's tenant. Delete reveals reference existence within the caller's
        // own tenant — the 404-before-PDP (no row) vs 403-after-PDP (row present,
        // denied) split is an own-tenant existence signal available to any member
        // of the tenant (find_own matches any subject for tenant/shared rows, not
        // only the private owner). This is an accepted part of the module's
        // threat model (a tenant member may enumerate their own tenant's
        // references); cross-tenant existence stays hidden by the anti-enumeration
        // 404 on the read path. A denial is therefore a plain operation-level 403.
        let resolved = self.resolve_stored(row.secret_type_uuid).await?;
        let scope = self
            .scope_for_timed(
                ctx,
                &authz::secret_type_resource(&resolved.gts_id),
                actions::DELETE,
            )
            .await?;
        if !self.repo.scope_includes_tenant(&scope, tenant.0).await? {
            self.metrics.cross_tenant_denied();
            return Err(DomainError::AccessDenied { cause: None });
        }

        // Fail fast if no plugin is available BEFORE marking deprovisioning —
        // symmetric with the put saga (which resolves the plugin before
        // touching metadata). Otherwise a misconfigured/absent plugin would
        // flip the row to deprovisioning (the secret instantly stops
        // resolving) and only then 503, wedging the reference: the caller is
        // told the delete failed, yet it visibly took effect, and the reaper
        // cannot finish it either (no plugin to reconcile the backend).
        let plugin = self.plugins.resolve().await?;

        // Step 1 — mark deprovisioning (skip when resuming a stuck saga).
        // From this instant the secret is invisible to resolution while the
        // row keeps holding the partial unique index, so a concurrent create
        // of the same reference stays a retryable Conflict until cleanup
        // finishes — releasing the name earlier would let this saga's lagging
        // backend delete erase the new secret's value (same backend key).
        if row.status == SecretStatus::Active
            && !self
                .repo
                .mark_deprovisioning(&scope, row.id, expected_version)
                .await?
        {
            // 0 rows: the row moved or vanished between find_own and the gated
            // flip. Under a version precondition that is an optimistic-lock
            // conflict; otherwise a concurrent delete won — report NotFound,
            // matching the pre-saga behavior.
            return if expected_version.is_some() {
                Err(DomainError::VersionConflict)
            } else {
                Err(DomainError::NotFound)
            };
        }

        // Step 2 — backend delete. A missing backend value is success
        // (idempotent). A failure leaves the deprovisioning row for a client
        // retry or the reaper; the caller sees a retryable error while the
        // secret already no longer resolves.
        let plugin_owner = if row.sharing == SharingMode::Private {
            Some(row.owner_id)
        } else {
            None
        };
        let t0 = Instant::now();
        let plugin_result = plugin
            .delete(ctx, &tenant, key, plugin_owner.as_ref())
            .await;
        let secs = t0.elapsed().as_secs_f64();
        let plugin_result = match plugin_result {
            Ok(()) | Err(CredStoreError::NotFound) => Ok(()),
            Err(e) => Err(map_plugin_err(e)),
        };
        self.metrics.dependency(
            Dep::Plugin,
            DepOp::PluginDelete,
            if plugin_result.is_ok() {
                Outcome::Success
            } else {
                Outcome::Error
            },
            secs,
        );
        if let Err(e) = plugin_result {
            tracing::warn!(
                tenant = %tenant.0,
                key = %key.as_ref(),
                err = %e,
                "credstore delete: backend delete failed; row stays deprovisioning until retried or reaped"
            );
            return Err(e);
        }

        // Step 3 — remove the metadata row, releasing the reference. The mark
        // was the version-gated step, so this delete is unconditional; a row
        // already gone (concurrent resume/reap finished first) is success.
        match self.repo.delete_by_id(&scope, row.id, None).await {
            Ok(()) | Err(DomainError::NotFound) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    tenant = %tenant.0,
                    key = %key.as_ref(),
                    err = %e,
                    "credstore delete: row delete after backend cleanup failed; reference stays held until retried or reaped"
                );
                Err(e)
            }
        }
    }

    /// Complete stale saga rows and refresh inventory gauges.
    ///
    /// Called by the reaper loop on a periodic tick. For every stale
    /// non-active row a best-effort backend delete reconciles the value store
    /// (closing the create-saga orphaned-value debt), then:
    /// - `provisioning` rows are removed unconditionally (the write never
    ///   became visible; the reference must not stay wedged);
    /// - `deprovisioning` rows are removed only after a successful backend
    ///   delete — otherwise the row (and the reference) is kept for the next
    ///   tick, because releasing the name before backend cleanup could let
    ///   this saga erase a successor secret's value.
    ///
    /// Errors are logged, not propagated.
    pub async fn reap_and_refresh(&self) {
        self.expire_secrets().await;
        self.reap_stale().await;
        self.backfill_unfenced().await;
        match self.repo.inventory().await {
            Ok(counts) => self.metrics.record_inventory(counts),
            Err(e) => {
                tracing::warn!(err = %e, "inventory failed");
            }
        }
    }

    /// Fence-backfill sweep: stamp out-of-band seeded rows (`value_fp IS
    /// NULL`) that nobody reads, so the fleet converges to fully fenced
    /// without traffic (feature spec `algo-reaper-fp-sweep`). One bounded
    /// batch per tick; every failure logs and continues — the reaper must
    /// survive transient backend/DB errors, and an unstamped row is only ever
    /// served on trust, never wrongly rejected.
    // Per-row: read value from backend -> stamp -> CAS; branchy but one flow.
    #[allow(clippy::cognitive_complexity)]
    async fn backfill_unfenced(&self) {
        let rows = match self.repo.list_unfenced(REAP_BATCH_LIMIT).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(err = %e, "reaper: list_unfenced failed");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let plugin = match self.plugins.resolve().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(err = %e, "reaper: no plugin for fence backfill this tick");
                return;
            }
        };
        for row in rows {
            let Some((ctx, key)) = Self::reaper_ctx_and_key(&row) else {
                continue;
            };
            let owner = if row.sharing == SharingMode::Private {
                Some(row.owner_id)
            } else {
                None
            };
            let value = match plugin.get(&ctx, &row.tenant_id, &key, owner.as_ref()).await {
                Ok(Some(v)) => v,
                // No backend value: nothing to stamp (the row 404s on read
                // anyway); leave it for a future seed/put.
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(id = %row.id, err = %e, "reaper: fence backfill read failed");
                    continue;
                }
            };
            self.backfill_row_fp(&plugin, &row, &value).await;
        }
    }

    /// Move expired active rows into the ordinary deprovisioning saga:
    /// invisible immediately, backend value + row cleaned by the pending sweep.
    async fn expire_secrets(&self) {
        match self.repo.mark_expired_deprovisioning().await {
            Ok(n) if n > 0 => {
                tracing::info!(count = n, "reaper: expired secrets marked deprovisioning");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(err = %e, "mark_expired_deprovisioning failed");
            }
        }
    }

    /// Reap one batch of stale saga rows and emit the reap counters.
    async fn reap_stale(&self) {
        let stale = self.list_stale().await;
        if stale.is_empty() {
            return;
        }

        // Backend reconciliation needs the plugin; without one, still remove
        // provisioning rows (pre-reconciliation behavior — un-wedge the
        // reference) but keep deprovisioning rows for the next tick.
        let plugin = match self.plugins.resolve().await {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(err = %e, "reaper: no plugin for backend reconciliation this tick");
                None
            }
        };

        let (prov_reaped, deprov_reaped) = self.reap_batch(plugin.as_ref(), stale).await;
        if prov_reaped > 0 {
            self.metrics.provisioning_reaped(prov_reaped);
        }
        if deprov_reaped > 0 {
            self.metrics.deprovisioning_reaped(deprov_reaped);
        }
    }

    /// One bounded batch of stale saga rows, or empty (with a warning) when
    /// the listing itself fails — the reaper must survive transient DB errors.
    async fn list_stale(&self) -> Vec<SecretRow> {
        self.repo
            .list_stale_pending(
                self.reaper.provisioning_timeout_secs,
                self.reaper.deprovisioning_timeout_secs,
                REAP_BATCH_LIMIT,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(err = %e, "list_stale_pending failed");
                Vec::new()
            })
    }

    /// Reap every row in the batch; returns
    /// `(provisioning_reaped, deprovisioning_reaped)`.
    async fn reap_batch(
        &self,
        plugin: Option<&Arc<dyn CredStorePluginClientV1>>,
        stale: Vec<SecretRow>,
    ) -> (u64, u64) {
        let mut prov_reaped = 0u64;
        let mut deprov_reaped = 0u64;
        for row in stale {
            match self.reap_row(plugin, &row).await {
                Some(SecretStatus::Provisioning) => prov_reaped += 1,
                Some(SecretStatus::Deprovisioning) => deprov_reaped += 1,
                _ => {}
            }
        }
        (prov_reaped, deprov_reaped)
    }

    /// Reap a single stale saga row. Ordering differs by status so the reaper
    /// can never delete a value out from under a create that has just
    /// succeeded:
    ///
    /// * `provisioning` — claim the row with a status-gated delete *first*. The
    ///   delete loses to a concurrent `mark_active` (the slow create finally
    ///   landing): 0 rows removed means the saga won the race and the now-active
    ///   secret, plus its backend value, must be left alone. Only once we own
    ///   the row do we issue the best-effort backend cleanup — so a reported
    ///   create success can never lose its row and backend value to the reaper.
    /// * `deprovisioning` — the reference name is held until the backend value
    ///   is gone, so the backend delete precedes the (still status-gated) row
    ///   delete.
    ///
    /// Returns the row's status when this call actually removed it.
    async fn reap_row(
        &self,
        plugin: Option<&Arc<dyn CredStorePluginClientV1>>,
        row: &SecretRow,
    ) -> Option<SecretStatus> {
        match row.status {
            SecretStatus::Provisioning => {
                // Claim first: if the status-gated delete loses to a concurrent
                // mark_active/delete, leave the row (and its value) alone.
                if !self.reap_delete_gated(row).await {
                    return None;
                }
                if let Some(p) = plugin {
                    self.reap_backend_delete(p, row).await;
                }
                Some(SecretStatus::Provisioning)
            }
            SecretStatus::Deprovisioning => {
                let backend_deleted = match plugin {
                    Some(p) => self.reap_backend_delete(p, row).await,
                    None => false,
                };
                if !backend_deleted {
                    return None;
                }
                self.reap_delete_gated(row)
                    .await
                    .then_some(SecretStatus::Deprovisioning)
            }
            // list_stale_pending never returns active rows; ignore defensively.
            SecretStatus::Active => None,
        }
    }

    /// Status-gated row delete for the reaper: removes the row only while it
    /// still holds the status the reaper observed, swallowing (but logging) a
    /// delete error. Returns whether this call actually removed the row.
    async fn reap_delete_gated(&self, row: &SecretRow) -> bool {
        match self.repo.reap_by_id(row.id, row.status).await {
            Ok(removed) => removed,
            Err(e) => {
                tracing::warn!(id = %row.id, status = ?row.status, err = %e, "reaper: row delete failed");
                false
            }
        }
    }

    /// Best-effort backend delete for a reaped row. Returns `true` when the
    /// backend no longer holds the value (deleted or was never there).
    async fn reap_backend_delete(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV1>,
        row: &SecretRow,
    ) -> bool {
        let Some((ctx, key)) = Self::reaper_ctx_and_key(row) else {
            return false;
        };
        let owner = if row.sharing == SharingMode::Private {
            Some(row.owner_id)
        } else {
            None
        };
        let t0 = Instant::now();
        let result = plugin
            .delete(&ctx, &row.tenant_id, &key, owner.as_ref())
            .await;
        let secs = t0.elapsed().as_secs_f64();
        let (outcome, deleted) = match result {
            Ok(()) => (Outcome::Success, true),
            Err(CredStoreError::NotFound) => (Outcome::NotFound, true),
            Err(e) => {
                tracing::warn!(
                    id = %row.id,
                    tenant = %row.tenant_id.0,
                    err = %e,
                    "reaper: backend delete failed"
                );
                (Outcome::Error, false)
            }
        };
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginDelete, outcome, secs);
        deleted
    }

    /// The reaper has no caller: build a minimal service context for the row's
    /// tenant plus the validated reference. Plugins are pure value stores keyed
    /// by the explicit tenant/key/owner arguments and must not rely on caller
    /// identity. `None` (with a warning) on the never-expected invalid row.
    fn reaper_ctx_and_key(row: &SecretRow) -> Option<(SecurityContext, SecretRef)> {
        let ctx = match SecurityContext::builder()
            .subject_id(Uuid::nil())
            .subject_tenant_id(row.tenant_id.0)
            .build()
        {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::warn!(id = %row.id, err = %e, "reaper: failed to build service context");
                return None;
            }
        };
        // The DB CHECK guarantees the stored reference is well-formed.
        match SecretRef::new(row.reference.clone()) {
            Ok(key) => Some((ctx, key)),
            Err(e) => {
                tracing::warn!(id = %row.id, err = %e, "reaper: stored reference is invalid");
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
