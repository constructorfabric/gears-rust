use crate::{claims_error::ClaimsError, traits::KeyProvider};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{DecodingKey, Header, decode_header};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    #[serde(rename = "use")]
    #[allow(dead_code)]
    use_: Option<String>,
    n: String,
    e: String,
    #[allow(dead_code)]
    alg: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct JwksResponse {
    keys: Vec<Jwk>,
}

/// Handler for non-string custom JWT header fields; return `Some` to keep as string, or `None` to drop.
type HeaderExtrasHandler = dyn Fn(&str, &Value) -> Option<String> + Send + Sync;

/// Standard JWT header field names from RFC 7515 (JWS), RFC 7516 (JWE),
/// RFC 7518 (JWA), RFC 7797 (b64), and RFC 8555 (ACME).
const STANDARD_HEADER_FIELDS: &[&str] = &[
    "typ", "alg", "cty", "jku", "jwk", "kid", "x5u", "x5c", "x5t", "x5t#S256", "crit", "enc",
    "zip", "url", "nonce", "epk", "apu", "apv", "iv", "tag", "p2s", "p2c", "b64",
];

/// JWKS-based key provider with lock-free reads
///
/// Uses `ArcSwap` for lock-free key lookups and background refresh with exponential backoff.
#[must_use]
pub struct JwksKeyProvider {
    /// JWKS endpoint URL
    jwks_uri: String,

    /// Keys stored in `ArcSwap` for lock-free reads
    keys: Arc<ArcSwap<HashMap<String, DecodingKey>>>,

    /// Last refresh time and error tracking for backoff
    refresh_state: Arc<RwLock<RefreshState>>,

    /// Shared HTTP client for JWKS fetches (pooled connections)
    /// `HttpClient` is `Clone + Send + Sync`, no external locking needed.
    client: modkit_http::HttpClient,

    /// Refresh interval (default: 5 minutes)
    refresh_interval: Duration,

    /// Maximum backoff duration (default: 1 hour)
    max_backoff: Duration,

    /// Cooldown for on-demand refresh (default: 60 seconds)
    on_demand_refresh_cooldown: Duration,

    /// Optional handler for non-string custom JWT header fields.
    /// Called for each non-standard field whose value is not a JSON string.
    /// Return `Some(s)` to keep, `None` to drop.
    header_extras_handler: Option<Arc<HeaderExtrasHandler>>,
}

#[derive(Debug, Default)]
struct RefreshState {
    last_refresh: Option<Instant>,
    last_on_demand_refresh: Option<Instant>,
    consecutive_failures: u32,
    last_error: Option<String>,
    failed_kids: HashSet<String>,
}

impl JwksKeyProvider {
    /// Create a new JWKS key provider
    ///
    /// # Errors
    /// Returns error if HTTP client initialization fails (e.g., TLS setup)
    pub fn new(jwks_uri: impl Into<String>) -> Result<Self, modkit_http::HttpError> {
        Self::with_http_timeout(jwks_uri, Duration::from_secs(10))
    }

    /// Create a new JWKS key provider with custom HTTP timeout
    ///
    /// # Errors
    /// Returns error if HTTP client initialization fails (e.g., TLS setup)
    pub fn with_http_timeout(
        jwks_uri: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, modkit_http::HttpError> {
        let client = modkit_http::HttpClient::builder()
            .timeout(timeout)
            .retry(None) // JWKS provider handles its own retry logic
            .build()?;

        Ok(Self {
            jwks_uri: jwks_uri.into(),
            keys: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            refresh_state: Arc::new(RwLock::new(RefreshState::default())),
            client,
            refresh_interval: Duration::from_secs(300), // 5 minutes
            max_backoff: Duration::from_secs(3600),     // 1 hour
            on_demand_refresh_cooldown: Duration::from_secs(60), // 1 minute
            header_extras_handler: None,
        })
    }

    /// Create a new JWKS key provider (alias for new, kept for compatibility)
    ///
    /// # Errors
    /// Returns error if HTTP client initialization fails (e.g., TLS setup)
    pub fn try_new(jwks_uri: impl Into<String>) -> Result<Self, modkit_http::HttpError> {
        Self::new(jwks_uri)
    }

    /// Create with custom refresh interval
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }

    /// Create with custom max backoff
    pub fn with_max_backoff(mut self, max_backoff: Duration) -> Self {
        self.max_backoff = max_backoff;
        self
    }

    /// Create with custom on-demand refresh cooldown
    pub fn with_on_demand_refresh_cooldown(mut self, cooldown: Duration) -> Self {
        self.on_demand_refresh_cooldown = cooldown;
        self
    }

    /// Stringify all non-string custom JWT header fields.
    ///
    /// Convenience wrapper around [`with_header_extras_handler`](Self::with_header_extras_handler)
    /// that converts every non-string value to its JSON representation
    /// (e.g. `123` → `"123"`, `true` → `"true"`, `[1,2]` → `"[1,2]"`).
    pub fn with_header_extras_stringified(self) -> Self {
        self.with_header_extras_handler(|_, v| Some(v.to_string()))
    }

    /// Set a handler for non-string custom JWT header fields.
    ///
    /// `jsonwebtoken::Header::extras` is `HashMap<String, String>` and rejects
    /// non-string values. This callback is invoked for each such field.
    /// Return `Some(s)` to keep, `None` to drop.
    /// Without a handler, upstream `decode_header` is used as-is.
    pub fn with_header_extras_handler(
        mut self,
        handler: impl Fn(&str, &Value) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.header_extras_handler = Some(Arc::new(handler));
        self
    }

    /// Fetch JWKS from the endpoint
    async fn fetch_jwks(&self) -> Result<HashMap<String, DecodingKey>, ClaimsError> {
        // HttpClient is Clone + Send + Sync, no locking needed
        let jwks: JwksResponse = self
            .client
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| map_http_error(&e))?
            .json()
            .await
            .map_err(|e| map_http_error(&e))?;

        let mut keys = HashMap::new();
        for jwk in jwks.keys {
            if jwk.kty == "RSA" {
                let key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
                    .map_err(|e| ClaimsError::JwksFetchFailed(format!("Invalid RSA key: {e}")))?;
                keys.insert(jwk.kid, key);
            }
        }

        if keys.is_empty() {
            return Err(ClaimsError::JwksFetchFailed(
                "No valid RSA keys found in JWKS".into(),
            ));
        }

        Ok(keys)
    }

    /// Calculate backoff duration based on consecutive failures
    fn calculate_backoff(&self, failures: u32) -> Duration {
        let base = Duration::from_secs(60); // 1 minute base
        let exponential = base * 2u32.pow(failures.min(10)); // Cap at 2^10
        exponential.min(self.max_backoff)
    }

    /// Check if refresh is needed based on interval and backoff
    async fn should_refresh(&self) -> bool {
        let state = self.refresh_state.read().await;

        match state.last_refresh {
            None => true, // Never refreshed
            Some(last) => {
                let elapsed = last.elapsed();
                if state.consecutive_failures == 0 {
                    // Normal refresh interval
                    elapsed >= self.refresh_interval
                } else {
                    // Exponential backoff
                    elapsed >= self.calculate_backoff(state.consecutive_failures)
                }
            }
        }
    }

    /// Perform key refresh with error tracking
    async fn perform_refresh(&self) -> Result<(), ClaimsError> {
        match self.fetch_jwks().await {
            Ok(new_keys) => {
                // Update keys atomically
                self.keys.store(Arc::new(new_keys));

                // Update refresh state
                let mut state = self.refresh_state.write().await;
                state.last_refresh = Some(Instant::now());
                state.consecutive_failures = 0;
                state.last_error = None;

                Ok(())
            }
            Err(e) => {
                // Update failure state
                let mut state = self.refresh_state.write().await;
                state.last_refresh = Some(Instant::now());
                state.consecutive_failures += 1;
                state.last_error = Some(e.to_string());

                Err(e)
            }
        }
    }

    /// Check if a key exists in the cache
    fn key_exists(&self, kid: &str) -> bool {
        let keys = self.keys.load();
        keys.contains_key(kid)
    }

    /// Check if we're in cooldown period and handle throttling logic
    async fn check_refresh_throttle(&self, kid: &str) -> Result<(), ClaimsError> {
        let state = self.refresh_state.read().await;
        if let Some(last_on_demand) = state.last_on_demand_refresh {
            let elapsed = last_on_demand.elapsed();
            if elapsed < self.on_demand_refresh_cooldown {
                let remaining = self.on_demand_refresh_cooldown.saturating_sub(elapsed);
                tracing::debug!(
                    kid = kid,
                    remaining_secs = remaining.as_secs(),
                    "On-demand JWKS refresh throttled (cooldown active)"
                );

                // Check if this kid has failed before
                if state.failed_kids.contains(kid) {
                    tracing::warn!(
                        kid = kid,
                        "Unknown kid repeatedly requested despite recent refresh attempts"
                    );
                }

                return Err(ClaimsError::UnknownKeyId(kid.to_owned()));
            }
        }
        Ok(())
    }

    /// Update state after successful refresh and check if kid is now available
    async fn handle_refresh_success(&self, kid: &str) -> Result<(), ClaimsError> {
        let mut state = self.refresh_state.write().await;
        state.last_on_demand_refresh = Some(Instant::now());

        // Check if the kid now exists
        if self.key_exists(kid) {
            // Kid found - remove from failed list if present
            state.failed_kids.remove(kid);
        } else {
            // Kid still not found after refresh - track it
            state.failed_kids.insert(kid.to_owned());
            tracing::warn!(
                kid = kid,
                "Kid still not found after on-demand JWKS refresh"
            );
        }

        Ok(())
    }

    /// Update state after failed refresh
    async fn handle_refresh_failure(&self, kid: &str, error: ClaimsError) -> ClaimsError {
        let mut state = self.refresh_state.write().await;
        state.last_on_demand_refresh = Some(Instant::now());
        state.failed_kids.insert(kid.to_owned());
        error
    }

    /// Try to refresh keys if unknown kid is encountered
    /// Implements throttling to prevent excessive refreshes
    async fn on_demand_refresh(&self, kid: &str) -> Result<(), ClaimsError> {
        // Check if key exists
        if self.key_exists(kid) {
            return Ok(());
        }

        // Check if we're in cooldown period
        self.check_refresh_throttle(kid).await?;

        // Attempt refresh and track the kid if it fails
        tracing::info!(
            kid = kid,
            "Performing on-demand JWKS refresh for unknown kid"
        );

        match self.perform_refresh().await {
            Ok(()) => self.handle_refresh_success(kid).await,
            Err(e) => Err(self.handle_refresh_failure(kid, e).await),
        }
    }

    /// Get a key by kid (lock-free read)
    fn get_key(&self, kid: &str) -> Option<DecodingKey> {
        let keys = self.keys.load();
        keys.get(kid).cloned()
    }

    /// Validate JWT signature and decode claims without re-parsing the header.
    ///
    /// Uses `jsonwebtoken::crypto::verify` directly instead of `decode()`,
    /// because `decode()` internally calls `decode_header()` which fails
    /// on non-string custom header fields (e.g. `"eap": 1`).
    fn validate_token(
        token: &str,
        key: &DecodingKey,
        header: &Header,
    ) -> Result<Value, ClaimsError> {
        // Enforce exactly three dot-separated segments: header.payload.signature
        let parts: Vec<&str> = token.splitn(4, '.').collect();
        if parts.len() != 3 {
            return Err(ClaimsError::DecodeFailed("Invalid JWT structure".into()));
        }
        let signing_input = &token[..parts[0].len() + 1 + parts[1].len()];
        let payload_b64 = parts[1];
        let signature = parts[2];

        // Verify signature over header.payload (the original signing input)
        let valid =
            jsonwebtoken::crypto::verify(signature, signing_input.as_bytes(), key, header.alg)
                .map_err(|e| {
                    ClaimsError::DecodeFailed(format!("JWT signature verification failed: {e}"))
                })?;
        if !valid {
            return Err(ClaimsError::InvalidSignature);
        }

        // Decode payload
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64.trim_end_matches('='))
            .map_err(|e| ClaimsError::DecodeFailed(format!("JWT payload decode failed: {e}")))?;
        let claims: Value = serde_json::from_slice(&payload_bytes)
            .map_err(|e| ClaimsError::DecodeFailed(format!("JWT claims parse failed: {e}")))?;

        Ok(claims)
    }
}

#[async_trait]
impl KeyProvider for JwksKeyProvider {
    fn name(&self) -> &'static str {
        "jwks"
    }

    async fn validate_and_decode(&self, token: &str) -> Result<(Header, Value), ClaimsError> {
        // Strip "Bearer " prefix if present
        let token = token.trim_start_matches("Bearer ").trim();

        // Decode header to get kid and algorithm
        let header = match &self.header_extras_handler {
            Some(handler) => decode_header_with_handler(token, handler.as_ref()),
            None => decode_header(token),
        }
        .map_err(|e| ClaimsError::DecodeFailed(format!("Invalid JWT header: {e}")))?;

        let kid = header
            .kid
            .as_ref()
            .ok_or_else(|| ClaimsError::DecodeFailed("Missing kid in JWT header".into()))?;

        // Try to get key from cache
        let key = if let Some(k) = self.get_key(kid) {
            k
        } else {
            // Key not in cache, try on-demand refresh
            self.on_demand_refresh(kid).await?;

            // Try again after refresh
            self.get_key(kid)
                .ok_or_else(|| ClaimsError::UnknownKeyId(kid.clone()))?
        };

        // Validate signature and decode claims
        let claims = Self::validate_token(token, &key, &header)?;

        Ok((header, claims))
    }

    async fn refresh_keys(&self) -> Result<(), ClaimsError> {
        if self.should_refresh().await {
            self.perform_refresh().await
        } else {
            Ok(())
        }
    }
}

/// Background task to periodically refresh JWKS
///
/// This task will run until the `cancellation_token` is cancelled, enabling
/// graceful shutdown per `ModKit` patterns. Without cancellation support, this
/// task would run indefinitely and potentially cause process hang on shutdown.
///
/// # Example
///
/// ```ignore
/// use tokio_util::sync::CancellationToken;
/// use std::sync::Arc;
///
/// let provider = Arc::new(JwksKeyProvider::new("https://issuer/.well-known/jwks.json")?);
/// let cancel_token = CancellationToken::new();
///
/// // Spawn the refresh task
/// let task_handle = tokio::spawn(run_jwks_refresh_task(provider.clone(), cancel_token.clone()));
///
/// // On shutdown:
/// cancel_token.cancel();
/// task_handle.await?;
/// ```
pub async fn run_jwks_refresh_task(
    provider: Arc<JwksKeyProvider>,
    cancellation_token: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60)); // Check every minute

    loop {
        tokio::select! {
            () = cancellation_token.cancelled() => {
                tracing::info!("JWKS refresh task shutting down");
                break;
            }
            _ = interval.tick() => {
                if let Err(e) = provider.refresh_keys().await {
                    tracing::warn!("JWKS refresh failed: {}", e);
                }
            }
        }
    }
}

/// Decode a JWT header, routing non-string custom fields through `handler`.
///
/// Returns `Some(s)` to keep the field, `None` to drop it.
fn decode_header_with_handler(
    token: &str,
    handler: &dyn Fn(&str, &Value) -> Option<String>,
) -> Result<Header, jsonwebtoken::errors::Error> {
    let header_b64 = token
        .split('.')
        .next()
        .ok_or(jsonwebtoken::errors::ErrorKind::InvalidToken)?;

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64.trim_end_matches('='))
        .map_err(jsonwebtoken::errors::ErrorKind::Base64)?;

    let mut json: serde_json::Map<String, Value> = serde_json::from_slice(&header_bytes)?;

    json.retain(|key, value| {
        if STANDARD_HEADER_FIELDS.contains(&key.as_str()) || value.is_string() {
            return true;
        }
        match handler(key, value) {
            Some(s) => {
                *value = Value::String(s);
                true
            }
            None => false,
        }
    });

    Ok(serde_json::from_value(Value::Object(json))?)
}

/// Map `HttpError` variants to appropriate `ClaimsError` messages
fn map_http_error(e: &modkit_http::HttpError) -> ClaimsError {
    ClaimsError::JwksFetchFailed(crate::http_error::format_http_error(e, "JWKS"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "jwks_tests.rs"]
mod tests;
