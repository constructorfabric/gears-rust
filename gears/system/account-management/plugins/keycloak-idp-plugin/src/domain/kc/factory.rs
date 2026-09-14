//! `KeycloakAdminClientFactory` — single entry-point for Keycloak Admin REST.
//!
//! See DESIGN "Component Model" (KC admin client factory) and "Credentials,
//! Token Cache, and Rotation".
//!
//! Static infra creds (bootstrap admin, default-shared-realm admin) are
//! resolved inline from config (after toolkit's `${VAR}` expansion at
//! config-load time); dynamic per-realm secrets (Adopted / Created) are
//! resolved via [`CredStoreReader`]. Both flows funnel through the private
//! [`KeycloakAdminClientFactory::acquire_client`] so cache, token-fetch, and
//! error semantics are identical.

use std::sync::Arc;
use std::time::{Duration, Instant};

use credstore_sdk::SecretRef;
use secrecy::{ExposeSecret, SecretString};
use toolkit_macros::domain_model;

use crate::config::KeycloakConfig;
use crate::domain::credstore::CredStoreReader;
use crate::domain::error::{KcStatusKind, PluginError, redact_secrets};
use crate::domain::kc::client::KcAdminClient;
use crate::domain::kc::token_cache::{CachedToken, InflightLocks, TokenCache};
use crate::domain::kc::transport::{KcTransport, truncate_2kb};
use crate::domain::ports::metrics::{KcAdminMetricsPort, TokenRefreshOutcome, TokenTier};

/// Subset of the OIDC token endpoint response the factory needs.
/// Other fields (`token_type`, `refresh_token`, `scope`, …) are ignored.
// The derive is the decoder for the Keycloak response described above.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: SecretString,
    expires_in: u64,
}

/// Floor on the effective cache TTL after subtracting the safety margin.
/// Prevents a tight token-fetch loop when Keycloak returns `expires_in`
/// at or below the safety margin (misconfigured client, rotation corner
/// case). Five seconds is short enough that a genuinely-expiring token
/// surfaces as a 401 retry rather than a stale-cache hit, and long
/// enough that the per-`(realm, client_id)` inflight gate serialises
/// concurrent misses.
const MIN_TOKEN_TTL: Duration = Duration::from_secs(5);

/// Defensive ceiling for an admin token's in-memory cache lifetime.
///
/// Keycloak controls `expires_in`; refreshing earlier than the reported expiry
/// is harmless, while trusting an unbounded value can overflow `Instant` and
/// panic the request path. A day is well above the deployment's normal token
/// lifetime and bounds both arithmetic and stale-token exposure.
const MAX_TOKEN_TTL: Duration = Duration::from_hours(24);

/// Origin for an admin client's `client_secret`.
///
/// `Inline` — static infra cred (bootstrap admin, default-shared-realm admin)
/// carried as a pre-resolved [`SecretString`] per DESIGN "Security and Data Protection". The
/// value comes from config after toolkit's `${VAR}` expansion at config-load
/// time (symmetric with the AM DB DSN's `${PGPASSWORD}`). `OpenBaoRef` —
/// dynamic per-realm secret (Adopted / Created) resolved via
/// [`CredStoreReader`] using the plugin-owned `SecurityContext`.
#[domain_model]
#[derive(Debug, Clone)]
pub enum SecretSource {
    Inline(SecretString),
    OpenBaoRef(SecretRef),
}

/// Map a [`SecretSource`] variant to the typed [`TokenTier`] label
/// consumed by [`KcAdminMetricsPort::credential_refresh`] /
/// [`KcAdminMetricsPort::kc_admin_token_refresh`] (DESIGN "Observability", ports/metrics
/// `TokenTier`). Bootstrap-admin refreshes always carry `StaticEnv` —
/// the bootstrap secret is itself an inline-resolved config value, so
/// there is no separate `bootstrap` tier on the typed label set; the
/// distinction between bootstrap and realm-admin static creds is
/// already carried by the `realm` label.
#[must_use]
fn tier_for(source: &SecretSource) -> TokenTier {
    match source {
        SecretSource::Inline(_) => TokenTier::StaticEnv,
        SecretSource::OpenBaoRef(_) => TokenTier::OpenBao,
    }
}

/// Single entry-point through which all Keycloak Admin REST work flows.
///
/// Owns the [`KcTransport`] handle and the in-memory [`TokenCache`].
/// Construct once at `Gear::init`; clone (cheap — `Arc`-shared cache and
/// transport) into call sites that need realm-admin access.
#[domain_model]
pub struct KeycloakAdminClientFactory {
    cfg: KeycloakConfig,
    transport: Arc<dyn KcTransport>,
    cs_reader: CredStoreReader,
    token_cache: Arc<TokenCache>,
    /// Per-key locks coalescing concurrent cold-cache fetches into a single
    /// token-endpoint call (DESIGN "Component Model" — single-flight on miss to
    /// avoid thundering-herd refreshes after a deploy or expiry burst). Map
    /// grows O(realms); bounded eviction is a future concern.
    inflight: Arc<InflightLocks>,
    base_url: url::Url,
    /// Pre-built bootstrap admin [`SecretSource`] — avoids a new
    /// `SecretString::from(...)` allocation on every `bootstrap_client()` call.
    bootstrap_source: SecretSource,
    /// Telemetry sink for `kc_admin_token_refresh` and
    /// `credential_refresh` (DESIGN "Observability"). Every cache-miss token-endpoint
    /// POST in `fetch_token` ticks both counters labelled by
    /// `(outcome, tier, realm)`; the reactive-401 retry path drives
    /// through `fetch_token` after `invalidate`, so the rotation
    /// signal naturally surfaces on the same counters without a
    /// separate call site.
    kc_metrics: Arc<dyn KcAdminMetricsPort>,
}

impl KeycloakAdminClientFactory {
    /// Build the factory with a pre-constructed transport + an empty cache.
    ///
    /// The transport is constructed by the caller (typically `module.rs`) so
    /// that TLS / timeout configuration lives entirely in the infra layer. This
    /// keeps `KeycloakAdminClientFactory` free of any reqwest imports.
    ///
    /// Normalize `base_url` to always end with `/` so `url::Url::join` against
    /// a relative path (e.g. `realms/{realm}/...`) preserves any path prefix
    /// the operator configured (e.g. `https://kc.example/auth/`). Without
    /// this, `https://kc.example/auth` joined with `realms/...` clobbers the
    /// `/auth` segment.
    #[must_use]
    pub fn new(
        cfg: KeycloakConfig,
        transport: Arc<dyn KcTransport>,
        cs_reader: CredStoreReader,
        kc_metrics: Arc<dyn KcAdminMetricsPort>,
    ) -> Self {
        let base_url = if cfg.base_url.path().ends_with('/') {
            cfg.base_url.clone()
        } else {
            let mut normalised = cfg.base_url.clone();
            let new_path = format!("{}/", cfg.base_url.path());
            normalised.set_path(&new_path);
            normalised
        };
        let bootstrap_source =
            SecretSource::Inline(cfg.bootstrap_admin.client_secret.clone_into_secret_string());
        Self {
            cfg,
            transport,
            cs_reader,
            token_cache: Arc::default(),
            inflight: Arc::default(),
            base_url,
            bootstrap_source,
            kc_metrics,
        }
    }

    /// Acquire a `master`-realm admin client per `bootstrap_admin.*` config.
    /// Used for `Created` realm creation + `Shared` / `Adopted` existence
    /// probes (DESIGN "Component Model").
    ///
    /// The [`SecretSource`] is pre-built at construction time to avoid
    /// allocating a new `SecretString` on every call.
    ///
    /// # Errors
    /// * [`PluginError::KcRest`] when the token endpoint fails.
    /// * [`PluginError::Config`] when the configured `base_url` cannot be
    ///   joined with the token endpoint path.
    pub async fn bootstrap_client(&self) -> Result<KcAdminClient, PluginError> {
        self.acquire_client(
            &self.cfg.bootstrap_admin.realm,
            &self.cfg.bootstrap_admin.client_id,
            &self.bootstrap_source,
        )
        .await
    }

    /// Acquire a per-realm admin client. The caller supplies the realm,
    /// `client_id`, and where the `client_secret` lives. The factory caches
    /// the resulting token by `(realm_name, admin_client_id)`.
    ///
    /// # Errors
    /// Same shape as [`Self::bootstrap_client`]; in addition,
    /// [`PluginError::CredStoreRead`] when `source` is
    /// [`SecretSource::OpenBaoRef`] and the read fails / returns `None`.
    pub async fn realm_client(
        &self,
        realm_name: &str,
        admin_client_id: &str,
        source: &SecretSource,
    ) -> Result<KcAdminClient, PluginError> {
        self.acquire_client(realm_name, admin_client_id, source)
            .await
    }

    /// Drop the cached token AND the inflight-lock entry for
    /// `(realm, client_id)`. Called by the reactive-401 path
    /// ([`Self::with_reactive_401`]) when Keycloak rejects the cached
    /// secret, and by deprovision paths to release per-realm state.
    /// The paired drop keeps the `InflightLocks` map bounded by live
    /// operator footprint instead of leaking one entry per ever-seen
    /// realm. No-op when either entry is absent.
    pub fn invalidate(&self, realm: &str, client_id: &str) {
        self.token_cache.invalidate(realm, client_id);
        self.inflight.forget(realm, client_id);
    }

    /// Invalidate the cached bootstrap admin token. Symmetric with
    /// [`Self::bootstrap_client`]: same `(realm, client_id)` key, so the next
    /// `bootstrap_client()` / `with_reactive_401_bootstrap(...)` call goes
    /// through `fetch_token` and re-mints from the bootstrap secret.
    ///
    /// Required mid-saga in Created-mode tenant provisioning: when the
    /// bootstrap client itself creates a new realm via `POST /admin/realms`,
    /// Keycloak auto-grants the creator the `realm-admin` (composite) role on
    /// the freshly-created realm. But access tokens carry a *point-in-time*
    /// snapshot of role claims — the in-flight bootstrap token issued
    /// **before** the realm existed does not include the new mapping, so the
    /// next bootstrap call against `/admin/realms/{new_realm}/clients` returns
    /// `403 Forbidden`. The reactive-401 wrapper re-mints only on 401, so a
    /// 403 from this race surfaces as an `AmbiguousCreated { stage:
    /// KcClientCreate }` and bricks the saga.
    ///
    /// Calling this between `create_realm` (step 2) and
    /// `create_realm_admin_client` (step 3) forces the next bearer to be
    /// minted *after* the role grant, picking up the new claims and clearing
    /// the 403.
    ///
    /// # Scope and concurrency
    ///
    /// Drops only the `(bootstrap.realm, bootstrap.client_id)` cache entry —
    /// per-realm admin tokens (`(realm_name, keycloak-idp-plugin-realm-admin)`) are
    /// keyed differently and untouched, so the user-op path and per-realm
    /// admin work are unaffected.
    ///
    /// In-flight bootstrap operations on other tasks are unaffected: a caller
    /// already inside `with_reactive_401_bootstrap` holds its `KcAdminClient`
    /// (with its `Arc<Token>`) locally and finishes against the live KC
    /// session. `invalidate` only clears the cache map for *future* lookups.
    ///
    /// The only observable side-effect for parallel sagas (concurrent Shared
    /// / Adopted / Deprovision / another Created-mode tenant create) is one
    /// extra token-endpoint POST on whichever of them next consults the
    /// cache. The re-minted token is strictly a superset of the dropped one
    /// (it carries any roles granted in the meantime), so correctness is
    /// monotonic. Under N concurrent bootstrap-using sagas the worst case is
    /// N extra mints across the burst — negligible against KC's ~10 ms token
    /// endpoint and the low-frequency nature of Created-mode tenant create.
    pub fn invalidate_bootstrap(&self) {
        self.invalidate(
            &self.cfg.bootstrap_admin.realm,
            &self.cfg.bootstrap_admin.client_id,
        );
    }

    /// Normalised base URL (always ends with `/`). Used by facades that need
    /// to build admin REST paths outside the realm-scoped `KcAdminClient`
    /// (e.g. probing realm existence with the bootstrap admin token).
    #[must_use]
    pub fn base_url_str(&self) -> &str {
        self.base_url.as_str()
    }

    /// Plugin-internal preflight invoked at the start of provision/deprovision
    /// before issuing any destructive call. Performs a lightweight unauth
    /// `GET /realms/master` against the configured base URL — Keycloak serves
    /// the public OIDC realm metadata on this path, which is cheap, idempotent,
    /// and short-circuits cleanly when Keycloak is unambiguously unreachable
    /// (DESIGN "Component Model" `health_probe`).
    ///
    /// Wired into [`crate::domain::tenant_facade::TenantFacade::provision_tenant_inner`]
    /// and `deprovision_tenant_inner` saga preludes so a flat-out KC outage
    /// surfaces as `IdpProvisionFailure::CleanFailure` /
    /// `IdpDeprovisionFailure::Retryable` at the boundary instead of getting
    /// buried in a half-done saga.
    ///
    /// # Errors
    ///
    /// [`PluginError::KcRest`] with the appropriate [`KcStatusKind`]:
    /// `Timeout` / `Transport` on send failure, `Http(status)` on non-2xx.
    /// [`PluginError::Config`] if the configured base URL cannot be joined
    /// with `realms/master`.
    pub async fn health_probe(&self) -> Result<(), PluginError> {
        let url = self
            .base_url
            .join("realms/master")
            .map_err(|e| PluginError::Config {
                detail: e.to_string(),
            })?;
        let (status, body) = self
            .transport
            .get_unauthenticated(url.as_str(), "/realms/master")
            .await?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/realms/master".into(),
                status: KcStatusKind::Http(status),
                // Every other `KcRest` construction site truncates + redacts;
                // health-probe must too, otherwise an unbounded / unredacted
                // KC response slips into `body_first_2kb` and breaks the
                // documented 2 KiB / defence-in-depth contract.
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            });
        }
        Ok(())
    }

    /// Reactive-401 refresh wrapper (DESIGN "Credentials, Token Cache, and Rotation").
    ///
    /// Acquires a client for `(realm, client_id)` from the cache, runs `f`,
    /// and on a [`PluginError::KcRest`] carrying [`KcStatusKind::Http(401)`]
    /// invalidates the cached token, re-resolves the secret, and retries `f`
    /// exactly once. A second 401 propagates verbatim — the rotation ordering
    /// contract (write to KC first, then `OpenBao`) makes a second 401 the
    /// operator's problem.
    ///
    /// Wired around per-realm Admin REST work in both
    /// [`crate::domain::tenant_facade::TenantFacade`] saga steps and
    /// [`crate::domain::user_facade::UserFacade`] user-op steps. The
    /// retry path re-enters [`Self::acquire_client`] after `invalidate`,
    /// so a rotation surfaces as a second tick on `credential_refresh` /
    /// `kc_admin_token_refresh` without a separate emission site.
    ///
    /// # Errors
    ///
    /// Propagates `f`'s error verbatim except on a single retryable 401.
    pub async fn with_reactive_401<F, Fut, T>(
        &self,
        realm: &str,
        client_id: &str,
        source: &SecretSource,
        f: F,
    ) -> Result<T, PluginError>
    where
        F: Fn(KcAdminClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, PluginError>>,
    {
        let client = self.acquire_client(realm, client_id, source).await?;
        match f(client).await {
            Err(PluginError::KcRest {
                status: KcStatusKind::Http(401),
                ..
            }) => {
                self.invalidate(realm, client_id);
                let fresh = self.acquire_client(realm, client_id, source).await?;
                f(fresh).await
            }
            other => other,
        }
    }

    /// Bootstrap-admin variant of [`Self::with_reactive_401`]. Wraps a
    /// block of bootstrap-admin REST work so an operator-rotated
    /// `bootstrap_admin.client_secret` is picked up on the first 401
    /// without operator-driven plugin restart. The closure is invoked
    /// once on the happy path and at most twice on a single 401 hit.
    ///
    /// # Errors
    ///
    /// Propagates `f`'s error verbatim except on a single retryable 401.
    pub async fn with_reactive_401_bootstrap<F, Fut, T>(&self, f: F) -> Result<T, PluginError>
    where
        F: Fn(KcAdminClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, PluginError>>,
    {
        self.with_reactive_401(
            &self.cfg.bootstrap_admin.realm,
            &self.cfg.bootstrap_admin.client_id,
            &self.bootstrap_source,
            f,
        )
        .await
    }

    async fn acquire_client(
        &self,
        realm: &str,
        client_id: &str,
        source: &SecretSource,
    ) -> Result<KcAdminClient, PluginError> {
        // Fast path: warm cache, no lock acquisition. Cache hits do NOT
        // emit refresh metrics — the rotation-observability invariant is
        // "every counter tick = a real secret/token resolution from its
        // source". A cache hit reuses the same bearer token, no
        // resolution happened.
        if let Some(token) = self.token_cache.get(realm, client_id) {
            return Ok(KcAdminClient::new(Arc::clone(&self.transport), token));
        }
        // Slow path: serialize concurrent misses for this `(realm, client_id)`
        // behind a per-key `tokio::sync::Mutex` so N replicas hitting a cold
        // cache after deploy / expiry burst trigger one token-endpoint POST,
        // not N (DESIGN "Component Model").
        let lock = self.inflight.lock_for(realm, client_id);
        // HELD ACROSS AWAIT BY DESIGN — see DESIGN "Component Model". The guard
        // spans `resolve_secret().await` + `fetch_token().await` so concurrent
        // cold-cache misses for the same `(realm, client_id)` serialise into
        // one token-endpoint POST. Per-key mutex (not global), so unrelated
        // realms never contend.
        let _guard = lock.lock().await;
        // Re-check the cache after winning the lock — another task may have
        // populated it while we waited.
        if let Some(token) = self.token_cache.get(realm, client_id) {
            return Ok(KcAdminClient::new(Arc::clone(&self.transport), token));
        }
        // Real miss: every path from here emits exactly one refresh
        // metric per stage so the operator can distinguish
        // "OpenBao unreachable" (credential_refresh{outcome=error})
        // from "KC token endpoint rejected secret"
        // (kc_admin_token_refresh{outcome=error}). The reactive-401
        // retry path re-enters this method after `invalidate`, so a
        // rotation surfaces as a second tick on both counters without
        // a separate emission site (DESIGN "Credentials, Token Cache, and Rotation").
        let tier = tier_for(source);
        let secret = match self.resolve_secret(source).await {
            Ok(s) => {
                self.kc_metrics
                    .credential_refresh(TokenRefreshOutcome::Success, tier, realm);
                s
            }
            Err(e) => {
                self.kc_metrics
                    .credential_refresh(TokenRefreshOutcome::Error, tier, realm);
                return Err(e);
            }
        };
        let cached = match self.fetch_token(realm, client_id, &secret).await {
            Ok(c) => {
                self.kc_metrics
                    .kc_admin_token_refresh(TokenRefreshOutcome::Success, tier, realm);
                c
            }
            Err(e) => {
                self.kc_metrics
                    .kc_admin_token_refresh(TokenRefreshOutcome::Error, tier, realm);
                return Err(e);
            }
        };
        self.token_cache
            .insert(realm, client_id, Arc::clone(&cached));
        // Opportunistic GC on the slow path: every cold-cache acquire
        // (every TTL boundary per realm, or every reactive-401 retry)
        // sweeps both maps. Read-time eviction in `TokenCache::get`
        // already keeps the cache bounded by *active-fetch* realms;
        // this sweep additionally evicts (a) tokens for realms that
        // were fetched once and never re-read AND (b) inflight-lock
        // entries whose realm no longer has a live token. The work is
        // O(N) per slow-path acquire — fine because slow-path acquires
        // are rare by design (single-flight + TTL minutes apart).
        // Best-effort housekeeping; the pruned counts are not consumed here.
        let _pruned = self.token_cache.prune_expired();
        let _reaped = self.inflight.prune_inactive(&self.token_cache);
        Ok(KcAdminClient::new(Arc::clone(&self.transport), cached))
    }

    async fn resolve_secret(&self, source: &SecretSource) -> Result<SecretString, PluginError> {
        match source {
            SecretSource::Inline(s) => Ok(s.clone()),
            SecretSource::OpenBaoRef(r) => self
                .cs_reader
                .get(r)
                .await
                .map_err(|e| PluginError::CredStoreRead {
                    detail: e.to_string(),
                })?
                .ok_or_else(|| PluginError::CredStoreRead {
                    detail: format!("ref {} not found", r.as_ref()),
                }),
        }
    }

    async fn fetch_token(
        &self,
        realm: &str,
        client_id: &str,
        secret: &SecretString,
    ) -> Result<Arc<CachedToken>, PluginError> {
        let token_path = format!("realms/{realm}/protocol/openid-connect/token");
        let token_url = self
            .base_url
            .join(&token_path)
            .map_err(|e| PluginError::Config {
                detail: e.to_string(),
            })?;

        let path_template = "/realms/{realm}/protocol/openid-connect/token";
        let fields = [
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", secret.expose_secret()),
        ];
        let (status, body_text) = self
            .transport
            .form_post(token_url.as_str(), &fields, path_template)
            .await?;

        if !(200..=299).contains(&status) {
            return Err(PluginError::KcRest {
                method: "POST",
                path_template: "/realms/{realm}/protocol/openid-connect/token".into(),
                status: KcStatusKind::Http(status),
                // Token-endpoint error bodies can echo the rejected
                // `client_secret` form field on certain KC misconfigs
                // (and on some upstream proxy 5xx HTML); the
                // defence-in-depth pair MUST run on this seam, not just
                // tenant/user-facade KC sites.
                body_first_2kb: redact_secrets(&truncate_2kb(&body_text)),
            });
        }

        let body: TokenResponse = serde_json::from_str(&body_text).map_err(|e| {
            // `serde_json::Error::Display` reports the byte offset
            // (`column`) into the FULL response body. The error envelope
            // carries at most `BODY_CAP` bytes via `truncate_2kb`, so an
            // offset past that ceiling references bytes the operator
            // cannot actually see in `body_first_2kb`. Annotate when the
            // position is beyond the captured slice so post-mortems
            // aren't misled into hunting for "column 9000" in a 2KB blob.
            const BODY_CAP: usize = 2048;
            let pos_note = if e.column() > BODY_CAP {
                format!(" [position beyond first {BODY_CAP}B of body]")
            } else {
                String::new()
            };
            PluginError::KcRest {
                method: "POST",
                path_template: "/realms/{realm}/protocol/openid-connect/token".into(),
                // Preserve the real 2xx status (typically 200, but KC could
                // return 201/204 on a token-issue path); a hard-coded 200
                // misled post-mortems when the actual response was different.
                status: KcStatusKind::Http(status),
                // `format!("json: {e}")` is bounded by serde's error
                // formatter (a short structural message, no echo of
                // the body), so `truncate_2kb` here is belt-and-braces
                // rather than load-bearing. Apply consistently anyway
                // so every factory body-field path has the same shape.
                body_first_2kb: redact_secrets(&truncate_2kb(&format!("json: {e}{pos_note}"))),
            }
        })?;

        // Pre-built realm_url for downstream callers — DESIGN "Component Model" `CachedToken` table.
        let realm_url = format!("{}admin/realms/{realm}", self.base_url.as_str());
        let safety = Duration::from_millis(self.cfg.admin_token_lifetime_safety_ms);
        let lifetime = Duration::from_secs(body.expires_in);
        // Clamp both ends of the effective lifetime. The floor prevents a
        // tight fetch loop when `expires_in <= safety`; the ceiling treats the
        // provider-controlled value as untrusted and prevents `Instant`
        // overflow while allowing an early, harmless refresh.
        let adjusted_lifetime = lifetime.saturating_sub(safety);
        let effective_lifetime = adjusted_lifetime.clamp(MIN_TOKEN_TTL, MAX_TOKEN_TTL);
        if lifetime <= safety {
            tracing::warn!(
                target: "keycloak_idp_plugin.factory",
                realm,
                client_id,
                kc_expires_in_secs = body.expires_in,
                safety_ms = self.cfg.admin_token_lifetime_safety_ms,
                "kc token expires_in <= safety margin; clamping cache TTL to MIN_TOKEN_TTL"
            );
        } else if adjusted_lifetime > MAX_TOKEN_TTL {
            tracing::warn!(
                target: "keycloak_idp_plugin.factory",
                realm,
                client_id,
                kc_expires_in_secs = body.expires_in,
                max_cache_ttl_secs = MAX_TOKEN_TTL.as_secs(),
                "kc token expires_in exceeds the cache ceiling; clamping to MAX_TOKEN_TTL"
            );
        }
        let now = Instant::now();
        let expires_at = now.checked_add(effective_lifetime).unwrap_or_else(|| {
            // The bounded duration is representable on supported targets, but
            // keep provider input incapable of reaching a panicking `Add`
            // implementation even on an unusual monotonic-clock backend.
            tracing::error!(
                target: "keycloak_idp_plugin.factory",
                realm,
                client_id,
                cache_ttl_secs = effective_lifetime.as_secs(),
                "bounded token cache TTL is not representable by Instant; disabling this cache entry"
            );
            now
        });

        Ok(Arc::new(CachedToken {
            access_token: body.access_token,
            expires_at,
            realm_url: Arc::from(realm_url.as_str()),
        }))
    }
}

#[cfg(test)]
#[path = "factory_tests.rs"]
mod factory_tests;
