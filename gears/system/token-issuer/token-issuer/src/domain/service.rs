//! Token issuance services.
//!
//! Two cohesive domain services, composed by [`Service`]:
//! - [`CapIssuer`] — mints per-call capability tokens (`cap+jwt`) for verified
//!   caller contexts and publishes the capability JWKS / discovery.
//! - [`OboIssuer`] — gated re-mint of a verified cap token into a down-scoped
//!   OBO token (gated by `obo.enabled`), with peer/down-scope/loop gates and
//!   its own JWKS.
//!
//! The dependency is one-directional: OBO re-mint verifies the inbound cap token
//! against the **capability** JWKS, so [`OboIssuer`] reads [`CapIssuer`]'s JWKS
//! (a shared [`JwksState`]); [`CapIssuer`] has no OBO dependency beyond knowing
//! the OBO issuer string for its loop guard.

use std::sync::Arc;
use std::time::Instant;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use token_issuer_sdk::{
    MintCapabilityRequest, MintGrantRequest, SigningClientV1, SigningKeyRef, TokenIssuerError,
};
use tokio::sync::RwLock;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::warn;

use crate::config::{MAX_TOKEN_TTL_SECS, TokenIssuerConfig};
use crate::domain::cache::{CacheOutcome, CapCache};
use crate::domain::cap_verify::verify_cap;
use crate::domain::claims::{
    build_cap_claims, build_grant_claims, cache_key_for, canonical_scopes, scopes_hash,
};
use crate::domain::downscope::downscope;
use crate::domain::error::DomainError;
use crate::domain::jwks::jwks_document;
use crate::domain::jws::assemble_and_sign;
use crate::domain::loopguard::is_obo_reentry;
use crate::domain::metrics::TokenIssuerMetrics;
use crate::domain::obo::{build_obo_claims, sign_obo};
use crate::domain::obo_cache::{OboCache, OboCacheKey};
use crate::domain::peer_identity::{PeerConnInfo, PeerIdentityResolver};
use crate::domain::rms_registry::RmsAdapterRegistry;

/// Wall-clock source (Unix seconds). Injectable so tests are deterministic.
/// `Arc` (not `Box`) so the cap and OBO issuers can share one clock.
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// System security context for internal public-key reads.
///
/// `public_keys`/`sign` carry a [`SecurityContext`] only for audit/propagation;
/// the signing identity is the plugin's own service account and the keys are
/// platform-scoped, so an anonymous (system) context is correct here.
fn system_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

/// Lazily-warmed public JWKS cache for one Transit signing key.
///
/// Shared (`Arc`) when one service must read another's keys — the OBO issuer
/// verifies inbound cap tokens against the capability key's JWKS.
#[domain_model]
struct JwksState {
    /// The Transit key whose public versions populate the document.
    key: SigningKeyRef,
    /// Cached JWKS document; `None` until `rebuild` succeeds (fail-closed).
    doc: RwLock<Option<serde_json::Value>>,
}

impl JwksState {
    fn new(key: SigningKeyRef) -> Self {
        Self {
            key,
            doc: RwLock::new(None),
        }
    }

    /// Reads the key's public versions and rebuilds the cached JWKS. Fail-closed:
    /// an empty/invalid key set leaves the previous cache untouched and returns
    /// `NotReady` (never caches an empty `{"keys":[]}`).
    async fn rebuild(&self, signer: &dyn SigningClientV1) -> Result<(), DomainError> {
        let versions = signer
            .public_keys(&system_ctx(), &self.key)
            .await
            .map_err(|e| {
                warn!(
                    target: "token_issuer.warm",
                    key = self.key.as_str(),
                    error = %e,
                    "reading public keys failed; staying not-ready"
                );
                DomainError::NotReady
            })?;
        let doc = jwks_document(self.key.as_str(), &versions);
        if jwks_is_empty(&doc) {
            return Err(DomainError::NotReady);
        }
        *self.doc.write().await = Some(doc);
        Ok(())
    }

    /// Rebuilds the JWKS if the cached document lacks `kid` (a Transit key
    /// rotated since the last warm), so a token minted with the new version
    /// stays verifiable. Best-effort: a rebuild failure leaves the last good doc.
    async fn refresh_for_kid(&self, signer: &dyn SigningClientV1, kid: &str) {
        if self.has_kid(kid).await {
            return;
        }
        if let Err(e) = self.rebuild(signer).await {
            warn!(
                target: "token_issuer.jwks",
                error = %e,
                kid,
                "JWKS rebuild for unknown kid failed; keeping last good document"
            );
        }
    }

    /// Whether the cached document carries a key with the given `kid`.
    async fn has_kid(&self, kid: &str) -> bool {
        self.doc
            .read()
            .await
            .as_ref()
            .is_some_and(|doc| jwks_has_kid(doc, kid))
    }

    /// Returns the cached document, or [`DomainError::NotReady`] if unwarmed.
    async fn document(&self) -> Result<serde_json::Value, DomainError> {
        self.doc.read().await.clone().ok_or(DomainError::NotReady)
    }
}

/// Mints capability tokens and serves the public capability JWKS / discovery.
#[domain_model]
pub struct CapIssuer {
    signer: Arc<dyn SigningClientV1>,
    /// Get-or-mint reuse cache for capability tokens.
    cache: CapCache,
    key: SigningKeyRef,
    issuer: String,
    /// OBO issuer identifier — used only by the loop guard to refuse a cap mint
    /// driven by an OBO bearer (no OBO → cap → OBO chain).
    obo_issuer: String,
    ttl_secs: u64,
    /// Capability-key JWKS; shared so the OBO issuer can verify provenance.
    jwks: Arc<JwksState>,
    metrics: Arc<TokenIssuerMetrics>,
    clock: Clock,
}

impl CapIssuer {
    fn new(
        signer: Arc<dyn SigningClientV1>,
        config: &TokenIssuerConfig,
        metrics: Arc<TokenIssuerMetrics>,
        clock: Clock,
    ) -> Result<Self, TokenIssuerError> {
        let key = SigningKeyRef::new(config.cap_key_name.clone())?;
        Ok(Self {
            signer,
            cache: CapCache::new(config.cap_reuse_floor_secs),
            key: key.clone(),
            issuer: config.cap_issuer(),
            obo_issuer: config.obo_issuer(),
            ttl_secs: config.cap_ttl_secs,
            jwks: Arc::new(JwksState::new(key)),
            metrics,
            clock,
        })
    }

    /// The capability issuer identifier.
    fn issuer(&self) -> &str {
        &self.issuer
    }

    /// A shared handle to the capability JWKS (for the OBO issuer's provenance check).
    fn jwks_handle(&self) -> Arc<JwksState> {
        Arc::clone(&self.jwks)
    }

    /// Mints (or reuses) a capability JWT for the verified caller context.
    ///
    /// On a cache miss the JWS is signed twice: once with a provisional header
    /// to learn the Transit key version, then once more with the final header
    /// carrying `kid = {cap_key}-v{version}`. `kid` is inside the signed header
    /// and must match the version Transit actually used, so the version cannot
    /// be guessed. This cost is paid only on a miss (≈ once per key-tuple per
    /// reuse window).
    ///
    /// # Errors
    /// Returns [`TokenIssuerError`] if claim serialization or signing fails.
    async fn mint_capability(
        &self,
        ctx: &SecurityContext,
        req: MintCapabilityRequest,
    ) -> Result<String, TokenIssuerError> {
        // Loop guard (defense in depth): a cap mint is driven by a first-party
        // user token, never by an OBO token. Refuse if the inbound bearer was
        // itself minted by the OBO issuer, so no OBO → cap → OBO chain forms.
        if is_obo_reentry(
            ctx.bearer_token().map(secrecy::ExposeSecret::expose_secret),
            &self.obo_issuer,
        ) {
            return Err(TokenIssuerError::InvalidRequest {
                reason: "capability mint refused under an OBO bearer".to_owned(),
            });
        }

        validate_mint_request(&req)?;
        validate_token_ttl(self.ttl_secs)?;

        let started = Instant::now();
        let now = (self.clock)();
        let claims = build_cap_claims(ctx, &req, &self.issuer, self.ttl_secs, now)?;
        let key = cache_key_for(&claims);
        let signer = Arc::clone(&self.signer);
        let cap_key = &self.key;
        let metrics = &self.metrics;
        let jwks = &self.jwks;
        let result = self
            .cache
            .get_or_mint(&key, now, || async move {
                // Assemble + ES256-sign `cap+jwt` (stable-kid double sign), recording
                // a sign-error metric on any signing failure.
                let jwt =
                    assemble_and_sign(signer.as_ref(), ctx, cap_key, "cap+jwt", &claims, |r| {
                        if r.is_err() {
                            metrics.record_sign_error();
                        }
                    })
                    .await?;
                // Fail closed BEFORE the cache stores this token: a fresh sign may
                // carry an unseen key version (Transit rotated). Rebuild the cap JWKS
                // so the new `kid` is publishable; if the rebuild fails and the JWKS
                // still lacks it, refuse the mint rather than cache a token adapters
                // would fetch the JWKS for, fail to find the kid, and reject.
                if let Some(kid) = kid_of_jwt(&jwt) {
                    jwks.refresh_for_kid(signer.as_ref(), &kid).await;
                    if !jwks.has_kid(&kid).await {
                        return Err(TokenIssuerError::Internal(format!(
                            "minted cap token kid {kid} is not publishable in the JWKS"
                        )));
                    }
                }
                Ok((jwt, claims.exp))
            })
            .await;

        self.metrics.record_mint_duration("cap", started.elapsed());

        match &result {
            Ok((_, CacheOutcome::Hit)) => self.metrics.record_cache_hit(),
            Ok((_, CacheOutcome::Miss)) => {
                self.metrics.record_cache_miss();
                self.metrics.record_sign("cap");
            }
            Err(_) => {}
        }

        result.map(|(jwt, _outcome)| jwt)
    }

    /// Warms the capability JWKS. See [`JwksState::rebuild`].
    async fn warm(&self) -> Result<(), DomainError> {
        self.jwks.rebuild(self.signer.as_ref()).await
    }

    /// Returns the cached capability-key JWKS document.
    ///
    /// # Errors
    /// Returns [`DomainError::NotReady`] if the JWKS has not been warmed.
    async fn cap_jwks(&self) -> Result<serde_json::Value, DomainError> {
        self.jwks.document().await
    }

    /// OIDC-style discovery document for the capability issuer.
    fn cap_discovery(&self) -> serde_json::Value {
        discovery_for(&self.issuer)
    }
}

/// Gated re-mint of a verified capability token into a down-scoped OBO token,
/// plus the OBO issuer's public JWKS / discovery. Inert unless `obo.enabled`.
#[domain_model]
pub struct OboIssuer {
    signer: Arc<dyn SigningClientV1>,
    key: SigningKeyRef,
    issuer: String,
    enabled: bool,
    audience: String,
    ttl_secs: u64,
    clock_skew_secs: u64,
    /// Idempotency cache for minted OBO tokens (keyed by cap jti + scope set).
    cache: OboCache,
    /// Resolves a calling peer (mTLS cert subject) to its adapter GTS ID.
    peer_resolver: Arc<dyn PeerIdentityResolver>,
    /// Reads adapter OBO-grant facts from the RMS registry.
    registry: Arc<dyn RmsAdapterRegistry>,
    /// OBO-key JWKS (this issuer's own published keys).
    jwks: JwksState,
    /// Capability-key JWKS (shared from [`CapIssuer`]) — for Gate 1 provenance.
    cap_jwks: Arc<JwksState>,
    /// Capability issuer identifier — for Gate 1 provenance verification.
    cap_issuer: String,
    metrics: Arc<TokenIssuerMetrics>,
    clock: Clock,
}

impl OboIssuer {
    fn new(
        signer: Arc<dyn SigningClientV1>,
        peer_resolver: Arc<dyn PeerIdentityResolver>,
        registry: Arc<dyn RmsAdapterRegistry>,
        config: &TokenIssuerConfig,
        metrics: Arc<TokenIssuerMetrics>,
        clock: Clock,
        cap: &CapIssuer,
    ) -> Result<Self, TokenIssuerError> {
        let key = SigningKeyRef::new(config.obo_key_name.clone())?;
        Ok(Self {
            signer,
            key: key.clone(),
            issuer: config.obo_issuer(),
            enabled: config.obo.enabled,
            audience: config.obo_audience.clone(),
            ttl_secs: config.obo_ttl_secs,
            clock_skew_secs: config.clock_skew_secs,
            cache: OboCache::new(),
            peer_resolver,
            registry,
            jwks: JwksState::new(key),
            cap_jwks: cap.jwks_handle(),
            cap_issuer: cap.issuer().to_owned(),
            metrics,
            clock,
        })
    }

    /// Whether OBO issuance / OBO issuer routes are enabled (default: `false`).
    fn enabled(&self) -> bool {
        self.enabled
    }

    /// Re-mints a verified capability token into a down-scoped OBO token
    /// (DESIGN.md § 3.6).
    ///
    /// The full gate sequence:
    /// 1. **Gated** — `OboDisabled` (404/503) unless `obo.enabled`.
    /// 2. **Loop guard** — `LoopGuard` (403) if the presented token was minted
    ///    by the OBO issuer (no OBO-on-OBO chain).
    /// 3. **Gate 1 provenance** — the cap token must verify against this issuer's
    ///    cap JWKS (ES256, `typ=cap+jwt`, issuer, non-expired) — `CapInvalid`
    ///    (401) otherwise.
    /// 4. **Gate 1 peer binding** — the verified mTLS peer's GTS ID must equal
    ///    the cap's `aud` — `PeerMismatch` (403). The adapter must be registered
    ///    (`PeerUnknown`, 403), active (`AdapterInactive`, 403), and OBO-granted
    ///    (`OboNotGranted`, 403).
    /// 5. **Gate 2 down-scope** — the grant is the adapter allowlist ∩ cap scopes
    ///    (a cap `*` grants the whole allowlist), optionally narrowed to
    ///    `requested`; a non-subset request or an empty final grant is
    ///    `OboNotGranted` (403). An empty grant is never minted.
    /// 6. **Per-replica idempotency** — keyed by `(cap jti, canonical grant)`;
    ///    a retry routed to this replica with the same cap and grant returns the
    ///    byte-identical OBO token.
    ///
    /// # Errors
    /// Returns the [`DomainError`] for whichever gate fails (see above).
    async fn remint_obo(
        &self,
        peer: &PeerConnInfo,
        cap_jwt: &str,
        requested: Option<Vec<String>>,
    ) -> Result<String, DomainError> {
        if !self.enabled {
            return Err(DomainError::OboDisabled);
        }
        // Loop guard: refuse a presented token that is itself an OBO token.
        if is_obo_reentry(Some(cap_jwt), &self.issuer) {
            return Err(DomainError::LoopGuard);
        }

        let now = (self.clock)();

        // Gate 1 — provenance: the cap token must verify against the cap JWKS.
        let cap = verify_cap(
            cap_jwt,
            &self.cap_jwks.document().await?,
            &self.cap_issuer,
            self.clock_skew_secs,
        )?;

        // Gate 1 — peer binding: the verified mTLS peer must be the cap audience.
        let peer_gts = self.peer_resolver.resolve(peer).await?;
        if cap.aud != peer_gts {
            return Err(DomainError::PeerMismatch);
        }
        let adapter = self
            .registry
            .lookup(&peer_gts)
            .await?
            .ok_or(DomainError::PeerUnknown)?;
        if !adapter.status_active {
            return Err(DomainError::AdapterInactive);
        }
        if !adapter.obo_callback_enabled {
            return Err(DomainError::OboNotGranted);
        }

        // Gate 2 — down-scope against the operator-granted allowlist.
        let granted = downscope(
            &adapter.obo_scope_allowlist,
            &cap.scopes,
            requested.as_deref(),
        )?;
        // Never mint an empty-scope OBO (also guards `requested = Some([])`).
        if granted.is_empty() {
            return Err(DomainError::OboNotGranted);
        }

        // Idempotency: key on the cap jti + the canonical granted scope set.
        let key = OboCacheKey::new(cap.jti, scopes_hash(&canonical_scopes(&granted.join(" "))));
        // Cache the entry up to the cap's Gate-1 acceptance horizon (exp + skew),
        // not bare exp: Gate 1 still accepts the cap during the skew window, so a
        // retry there must reuse the cached OBO rather than mint a fresh one.
        let cap_valid_until = cap
            .exp
            .saturating_add(i64::try_from(self.clock_skew_secs).unwrap_or(i64::MAX));
        let signer = Arc::clone(&self.signer);
        let obo_key = &self.key;
        let obo_jwks = &self.jwks;
        let obo_issuer = &self.issuer;
        let obo_audience = &self.audience;
        let obo_ttl = self.ttl_secs;
        let granted_ref = &granted;
        let peer_ref = &peer_gts;
        let cap_ref = &cap;
        let started = Instant::now();
        let jwt = self
            .cache
            .get_or_mint(&key, cap_valid_until, now, || async move {
                let claims = build_obo_claims(
                    cap_ref,
                    granted_ref,
                    peer_ref,
                    obo_issuer,
                    obo_audience,
                    obo_ttl,
                    now,
                )?;
                let jwt = sign_obo(signer.as_ref(), &system_ctx(), obo_key, &claims).await?;
                // Fail closed before the cache stores the token. A rotation can
                // make the fresh `kid` unknown to the warmed OBO JWKS; return an
                // error rather than cache a credential offline verifiers cannot
                // validate.
                let kid = kid_of_jwt(&jwt)
                    .ok_or_else(|| DomainError::internal("minted OBO token has no kid"))?;
                obo_jwks.refresh_for_kid(signer.as_ref(), &kid).await;
                if !obo_jwks.has_kid(&kid).await {
                    warn!(
                        target: "token_issuer.jwks",
                        kid,
                        "refusing OBO mint because its kid is not publishable"
                    );
                    return Err(DomainError::NotReady);
                }
                Ok((jwt, claims.exp))
            })
            .await?;
        self.metrics.record_mint_duration("obo", started.elapsed());

        Ok(jwt)
    }

    /// Warms the OBO JWKS. See [`JwksState::rebuild`].
    async fn warm(&self) -> Result<(), DomainError> {
        self.jwks.rebuild(self.signer.as_ref()).await
    }

    /// Returns the cached OBO-key JWKS document.
    ///
    /// # Errors
    /// Returns [`DomainError::NotReady`] if the JWKS has not been warmed (OBO is
    /// disabled by default, so this stays not-ready).
    async fn obo_jwks(&self) -> Result<serde_json::Value, DomainError> {
        self.jwks.document().await
    }

    /// OIDC-style discovery document for the OBO issuer (gated by `obo.enabled`).
    fn obo_discovery(&self) -> serde_json::Value {
        discovery_for(&self.issuer)
    }
}

/// Mints data-plane grant tokens (`grant+jwt`) and serves the public grant JWKS /
/// discovery. Its own class, issuer, and Transit key (`grant-token-sign`) — never
/// shared with `cap`/`obo`, so cross-acceptance is cryptographically impossible.
///
/// Unlike [`CapIssuer`], there is no reuse cache: every grant is unique (fresh
/// `jti`, request-specific resource + operation set), so each mint signs fresh.
#[domain_model]
pub struct GrantIssuer {
    signer: Arc<dyn SigningClientV1>,
    key: SigningKeyRef,
    issuer: String,
    /// Default TTL used only when the request supplies `ttl_secs == 0` (the gear
    /// normally passes an already-clamped, non-zero TTL).
    default_ttl_secs: u64,
    /// Grant-key JWKS (this issuer's own published keys).
    jwks: JwksState,
    metrics: Arc<TokenIssuerMetrics>,
    clock: Clock,
}

impl GrantIssuer {
    fn new(
        signer: Arc<dyn SigningClientV1>,
        config: &TokenIssuerConfig,
        metrics: Arc<TokenIssuerMetrics>,
        clock: Clock,
    ) -> Result<Self, TokenIssuerError> {
        let key = SigningKeyRef::new(config.grant_key_name.clone())?;
        Ok(Self {
            signer,
            key: key.clone(),
            issuer: config.grant_issuer(),
            default_ttl_secs: config.grant_ttl_secs,
            jwks: JwksState::new(key),
            metrics,
            clock,
        })
    }

    /// Mints a grant JWT (`grant+jwt`) for the verified caller context.
    ///
    /// Signs the `grant+jwt` (stable-kid double sign) with the `grant-token-sign`
    /// key via the signing port, then — like the cap path — rebuilds the grant
    /// JWKS if the mint carried an unseen key version, refusing the mint rather
    /// than returning a token whose `kid` is not yet publishable.
    ///
    /// # Errors
    /// Returns [`TokenIssuerError`] on an invalid request, claim serialization, or
    /// signing failure.
    async fn mint_grant(
        &self,
        ctx: &SecurityContext,
        mut req: MintGrantRequest,
    ) -> Result<token_issuer_sdk::GrantToken, TokenIssuerError> {
        validate_grant_request(&req)?;
        if req.ttl_secs == 0 {
            req.ttl_secs = self.default_ttl_secs;
        }
        validate_token_ttl(req.ttl_secs)?;

        let started = Instant::now();
        let now = (self.clock)();
        let claims = build_grant_claims(ctx, &req, &self.issuer, now)?;
        let metrics = &self.metrics;
        let jwt = assemble_and_sign(
            self.signer.as_ref(),
            ctx,
            &self.key,
            "grant+jwt",
            &claims,
            |r| {
                if r.is_err() {
                    metrics.record_sign_error();
                }
            },
        )
        .await?;

        // Fail closed before returning: a fresh sign may carry an unseen key
        // version (Transit rotated). Rebuild the grant JWKS so the new `kid` is
        // publishable; if it still is not, refuse rather than return a token an
        // adapter would fetch the JWKS for and then reject.
        if let Some(kid) = kid_of_jwt(&jwt) {
            self.jwks.refresh_for_kid(self.signer.as_ref(), &kid).await;
            if !self.jwks.has_kid(&kid).await {
                return Err(TokenIssuerError::Internal(format!(
                    "minted grant token kid {kid} is not publishable in the JWKS"
                )));
            }
        }

        self.metrics
            .record_mint_duration("grant", started.elapsed());
        self.metrics.record_sign("grant");
        Ok(token_issuer_sdk::GrantToken {
            token: jwt,
            expires_at: claims.exp,
        })
    }

    /// Warms the grant JWKS. See [`JwksState::rebuild`].
    async fn warm(&self) -> Result<(), DomainError> {
        self.jwks.rebuild(self.signer.as_ref()).await
    }

    /// Returns the cached grant-key JWKS document.
    async fn grant_jwks(&self) -> Result<serde_json::Value, DomainError> {
        self.jwks.document().await
    }

    /// OIDC-style discovery document for the grant issuer.
    fn grant_discovery(&self) -> serde_json::Value {
        discovery_for(&self.issuer)
    }
}

/// Composition root: the capability issuer, the (gated) OBO issuer, and the grant
/// issuer, fronted by one façade so the gear/REST/local-client surface stays uniform.
#[domain_model]
pub struct Service {
    cap: CapIssuer,
    obo: OboIssuer,
    grant: GrantIssuer,
}

impl Service {
    /// Builds the service from config and the injected ports: the signing
    /// client, the peer-identity resolver (mTLS cert → adapter GTS ID), and the
    /// RMS adapter registry (OBO-grant facts).
    ///
    /// # Errors
    /// Returns `Err` if `config.cap_key_name` or `config.obo_key_name` is not a
    /// valid signing-key reference.
    pub fn new(
        signer: Arc<dyn SigningClientV1>,
        peer_resolver: Arc<dyn PeerIdentityResolver>,
        registry: Arc<dyn RmsAdapterRegistry>,
        config: &TokenIssuerConfig,
        metrics: Arc<TokenIssuerMetrics>,
    ) -> Result<Self, TokenIssuerError> {
        let clock: Clock = Arc::new(|| chrono::Utc::now().timestamp());
        let cap = CapIssuer::new(
            Arc::clone(&signer),
            config,
            Arc::clone(&metrics),
            Arc::clone(&clock),
        )?;
        let grant = GrantIssuer::new(
            Arc::clone(&signer),
            config,
            Arc::clone(&metrics),
            Arc::clone(&clock),
        )?;
        let obo = OboIssuer::new(
            signer,
            peer_resolver,
            registry,
            config,
            metrics,
            clock,
            &cap,
        )?;
        Ok(Self { cap, obo, grant })
    }

    /// Overrides the clock on all issuers (test seam).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.cap.clock = Arc::clone(&clock);
        self.obo.clock = Arc::clone(&clock);
        self.grant.clock = clock;
        self
    }

    /// Whether OBO issuance / OBO issuer routes are enabled (default: `false`).
    #[must_use]
    pub fn obo_enabled(&self) -> bool {
        self.obo.enabled()
    }

    /// Mints (or reuses) a capability JWT for the verified caller context.
    ///
    /// # Errors
    /// Returns [`TokenIssuerError`] if claim serialization or signing fails.
    pub async fn mint_capability(
        &self,
        ctx: &SecurityContext,
        req: MintCapabilityRequest,
    ) -> Result<String, TokenIssuerError> {
        self.cap.mint_capability(ctx, req).await
    }

    /// Mints a data-plane grant token (`grant+jwt`) for the verified caller
    /// context and returns it with its absolute expiry.
    ///
    /// # Errors
    /// Returns [`TokenIssuerError`] on an invalid request, claim serialization, or
    /// signing failure.
    pub async fn mint_grant(
        &self,
        ctx: &SecurityContext,
        req: MintGrantRequest,
    ) -> Result<token_issuer_sdk::GrantToken, TokenIssuerError> {
        self.grant.mint_grant(ctx, req).await
    }

    /// Re-mints a verified capability token into a down-scoped OBO token.
    ///
    /// # Errors
    /// Returns the [`DomainError`] for whichever gate fails (see [`OboIssuer::remint_obo`]).
    pub async fn remint_obo(
        &self,
        peer: &PeerConnInfo,
        cap_jwt: &str,
        requested: Option<Vec<String>>,
    ) -> Result<String, DomainError> {
        self.obo.remint_obo(peer, cap_jwt, requested).await
    }

    /// Warms the public JWKS caches: the capability key always, and the OBO key
    /// when enabled. The readiness gate calls this before signalling ready, so a
    /// signing-backend or key-availability failure keeps the gear not-ready.
    ///
    /// # Errors
    /// Returns [`DomainError::NotReady`] if any required public key cannot be
    /// read, or if the resulting key set is empty / all PEMs are unparseable
    /// (fail closed — never cache an empty `{"keys":[]}`).
    pub async fn warm_jwks(&self) -> Result<(), DomainError> {
        self.cap.warm().await?;
        // The grant issuer is always active (the `grants` gear depends on it), so
        // its key must be readable for the gear to be ready — fail closed on a
        // missing `grant-token-sign` key exactly as for the cap key.
        self.grant.warm().await?;
        if self.obo.enabled() {
            self.obo.warm().await?;
        }
        Ok(())
    }

    /// Returns the cached capability-key JWKS document.
    ///
    /// # Errors
    /// Returns [`DomainError::NotReady`] if the JWKS has not been warmed.
    pub async fn cap_jwks(&self) -> Result<serde_json::Value, DomainError> {
        self.cap.cap_jwks().await
    }

    /// Returns the cached OBO-key JWKS document.
    ///
    /// # Errors
    /// Returns [`DomainError::NotReady`] if the JWKS has not been warmed.
    pub async fn obo_jwks(&self) -> Result<serde_json::Value, DomainError> {
        self.obo.obo_jwks().await
    }

    /// OIDC-style discovery document for the capability issuer.
    #[must_use]
    pub fn cap_discovery(&self) -> serde_json::Value {
        self.cap.cap_discovery()
    }

    /// OIDC-style discovery document for the OBO issuer (gated by `obo.enabled`).
    #[must_use]
    pub fn obo_discovery(&self) -> serde_json::Value {
        self.obo.obo_discovery()
    }

    /// Returns the cached grant-key JWKS document.
    ///
    /// # Errors
    /// Returns [`DomainError::NotReady`] if the JWKS has not been warmed.
    pub async fn grant_jwks(&self) -> Result<serde_json::Value, DomainError> {
        self.grant.grant_jwks().await
    }

    /// OIDC-style discovery document for the grant issuer.
    #[must_use]
    pub fn grant_discovery(&self) -> serde_json::Value {
        self.grant.grant_discovery()
    }
}

/// Validates a [`MintGrantRequest`] before minting: bounded/charset-safe audience,
/// resource name, and resource type, plus an operation set of at most
/// [`MAX_GRANT_OPERATIONS`] entries with each id bounded/charset-safe. Mirrors
/// [`validate_mint_request`]'s posture.
///
/// # Errors
/// Returns [`TokenIssuerError::InvalidRequest`] on the first violation.
fn validate_grant_request(req: &MintGrantRequest) -> Result<(), TokenIssuerError> {
    let reject = |field: &str| TokenIssuerError::InvalidRequest {
        reason: format!("invalid {field}"),
    };
    if !is_valid_field(&req.audience) {
        return Err(reject("audience"));
    }
    if !is_valid_field(&req.resource_name) {
        return Err(reject("resource_name"));
    }
    if !is_valid_field(&req.resource_type) {
        return Err(reject("resource_type"));
    }
    if req.operations.is_empty() {
        return Err(TokenIssuerError::InvalidRequest {
            reason: "operations must not be empty".to_owned(),
        });
    }
    if req.operations.len() > MAX_GRANT_OPERATIONS {
        return Err(TokenIssuerError::InvalidRequest {
            reason: format!("operations must contain at most {MAX_GRANT_OPERATIONS} entries"),
        });
    }
    if req.operations.iter().any(|op| !is_valid_field(op)) {
        return Err(reject("operation"));
    }
    Ok(())
}

/// Validates the issuer-wide hard lifetime ceiling.
fn validate_token_ttl(ttl_secs: u64) -> Result<(), TokenIssuerError> {
    if !(1..=MAX_TOKEN_TTL_SECS).contains(&ttl_secs) {
        return Err(TokenIssuerError::InvalidRequest {
            reason: format!("ttl_secs must be between 1 and {MAX_TOKEN_TTL_SECS}"),
        });
    }
    Ok(())
}

/// Maximum number of operations accepted in one grant mint request.
const MAX_GRANT_OPERATIONS: usize = 64;

/// Max length for free-form mint-request string fields (cap + grant).
const MAX_FIELD_LEN: usize = 256;

/// Whether `s` is a safe mint-request value (shared by cap- and grant-request
/// validation): non-control ASCII drawn from alphanumerics and a small separator
/// set (covers audiences / GTS IDs / scope and operation identifiers).
fn is_valid_field(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_FIELD_LEN
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/' | '~' | '*')
        })
}

/// Validates a [`MintCapabilityRequest`] before minting: a non-empty audience,
/// and bounded length + charset on `audience` / `operation` / `resource_type`.
///
/// # Errors
/// Returns [`TokenIssuerError::InvalidRequest`] on the first violation.
fn validate_mint_request(req: &MintCapabilityRequest) -> Result<(), TokenIssuerError> {
    let reject = |field: &str| TokenIssuerError::InvalidRequest {
        reason: format!("invalid {field}"),
    };
    if req.audience.trim().is_empty() {
        return Err(TokenIssuerError::InvalidRequest {
            reason: "audience must not be empty".to_owned(),
        });
    }
    if !is_valid_field(&req.audience) {
        return Err(reject("audience"));
    }
    if req
        .operation
        .as_deref()
        .is_some_and(|op| !is_valid_field(op))
    {
        return Err(reject("operation"));
    }
    if req
        .resource_type
        .as_deref()
        .is_some_and(|rt| !is_valid_field(rt))
    {
        return Err(reject("resource_type"));
    }
    Ok(())
}

/// Whether a JWKS document has no usable keys (`keys` absent or empty).
fn jwks_is_empty(doc: &serde_json::Value) -> bool {
    doc.get("keys")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty)
}

/// Whether a JWKS document carries a key with the given `kid`.
fn jwks_has_kid(doc: &serde_json::Value, kid: &str) -> bool {
    doc.get("keys")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|ks| {
            ks.iter()
                .any(|k| k.get("kid").and_then(serde_json::Value::as_str) == Some(kid))
        })
}

/// Extracts the `kid` from a compact JWS header (best-effort).
fn kid_of_jwt(jwt: &str) -> Option<String> {
    let header_b64 = jwt.split('.').next()?;
    let bytes = URL_SAFE_NO_PAD.decode(header_b64).ok()?;
    let header: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    header
        .get("kid")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Builds the minimal discovery document `{ "issuer", "jwks_uri" }` for an
/// issuer identifier.
fn discovery_for(issuer: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": issuer,
        "jwks_uri": format!("{issuer}/jwks.json"),
    })
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
