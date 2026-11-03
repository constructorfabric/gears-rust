//! Verifying a Firebase ID token, and deciding whether it belongs to an admin.
//!
//! # The thing to get right
//!
//! Firebase social login will issue a perfectly valid ID token to **anybody**
//! with a Google account. Signing in is not a decision this project makes; it
//! is a button. So a token that verifies proves who somebody is and says
//! nothing whatever about whether they may be here.
//!
//! The uid+email check against configured lists is therefore not a detail of
//! this module, it is the whole of the access control. Omitting it — or
//! "temporarily" relaxing it — hands the admin surface to the internet, and
//! everything else in this file would still pass.
//!
//! # Which keys
//!
//! Firebase ID tokens are signed by `securetoken@system.gserviceaccount.com`,
//! and the keys live at the JWK endpoint named below. The nearby
//! `oauth2/v3/certs` is a different key set for Google's *OAuth* ID tokens, and
//! using it here would refuse every genuine token with a signature error —
//! which reads as "Firebase is broken" rather than "wrong endpoint".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode_header};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use toolkit_http::HttpClient;
use tracing::{debug, warn};

/// Google's public keys for Firebase ID tokens, in JWK form.
const JWK_URL: &str =
    "https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com";

/// Used only when the response carries no usable `Cache-Control: max-age`.
/// Google rotates daily and always sends one; this is the floor under a
/// malformed header, not the normal path.
const FALLBACK_KEY_TTL: Duration = Duration::from_hours(1);

/// The least time between two key fetches. A `kid` is attacker-supplied and
/// needs no valid signature to reach the fetch, so an unknown one may cost at
/// most one fetch per interval; so may a key-service outage.
const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(10);

/// How long Google's keys are kept when the response says not to cache them
/// (`no-store`, `no-cache`, `max-age=0`): one fetch interval, so the logins
/// queued behind a fetch share its result instead of each fetching again.
const UNCACHEABLE_KEY_TTL: Duration = MIN_FETCH_INTERVAL;

/// The bound on one key fetch, request and body together. The HTTP client's
/// own timeouts stop once the headers arrive, and every login that needs a key
/// waits on the fetch, so a body that stalls must not stall them for longer.
const FETCH_DEADLINE: Duration = Duration::from_secs(10);

/// Google's key set is a few kilobytes; a body far larger is not it.
const MAX_KEY_SET_BYTES: usize = 64 * 1024;

/// How far `iat` and `auth_time` may lie in the future, in seconds: our clock
/// and Google's drift, and a token minted a moment ago must not be refused for
/// that. `exp` and `nbf` take no leeway, which is the stricter side for them.
const CLOCK_SKEW_SECS: u64 = 60;

/// Tokens are a few kilobytes; one far larger is refused before it is parsed.
const MAX_TOKEN_BYTES: usize = 8192;

/// The claims worth reading. Everything else Firebase sends is either checked
/// by the library or of no interest here.
#[derive(Debug, Deserialize)]
struct Claims {
    /// The Firebase user id. Stable across email changes and sign-in methods,
    /// which is why the allowlist is keyed on it rather than on an address.
    sub: String,
    iat: u64,
    /// When the user signed in. Firebase always sets it and documents it as
    /// required, so a token without it is not one Firebase issued.
    auth_time: u64,
    #[serde(default)]
    email: Option<String>,
    /// Google can put an address on the token before the owner has confirmed
    /// it. An unverified claim is treated as absent.
    #[serde(default)]
    email_verified: bool,
}

struct CachedKeys {
    keys: JwkSet,
    fetched_at: Instant,
    ttl: Duration,
}

impl CachedKeys {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < self.ttl
    }
}

/// Why a token was not accepted.
///
/// Separate variants for the log. What the caller turns them into is one
/// `Unauthorized` with one message, because a person who is not an admin
/// learning *why* only tells them how close they got.
#[derive(Debug)]
pub enum Refusal {
    /// Malformed, wrong algorithm, wrong issuer, expired, or badly signed.
    NotAValidToken(String),
    /// Genuine, and for somebody who is not an admin here.
    IdentityNotAllowed {
        /// The Firebase uid the token carries.
        uid: String,
        /// Its email claim, when present and not blank.
        email: Option<String>,
    },
    /// Google's keys could not be fetched. Not the caller's fault, and not
    /// something to answer 401 to.
    KeysUnavailable(String),
    /// The local crypto backend cannot verify tokens.
    CryptoUnavailable(String),
}

/// One allowlisted account. A token is admitted only when its uid and its
/// verified email both match the same entry.
#[derive(Debug, Clone)]
pub struct AllowedIdentity {
    /// The Firebase user id.
    pub uid: String,
    /// The address, compared case-insensitively.
    pub email: String,
}

/// Verifies Firebase ID tokens for one project against Google's published
/// keys, and admits only the configured identities.
pub struct FirebaseVerifier {
    project_id: String,
    identities: Vec<AllowedIdentity>,
    source: Arc<KeySource>,
    /// Held while deciding whether to fetch and while waiting for the fetch,
    /// so concurrent misses share one fetch; holds when the last one started.
    last_fetch: Mutex<Option<Instant>>,
}

/// Where the keys come from and what is cached of them. Shared with the fetch
/// task, which outlives a request that gives up waiting for it.
struct KeySource {
    /// `None` only in tests, which verify against a key of their own and never
    /// reach Google. A missing client at runtime would be a construction bug,
    /// and it refuses rather than panicking.
    http: Option<HttpClient>,
    jwk_url: String,
    fetch_deadline: Duration,
    keys: RwLock<Option<CachedKeys>>,
    /// Whether the last fetch to finish failed. Written by the fetch task, so it
    /// is right even when the request that started the fetch was cancelled.
    last_failed: AtomicBool,
}

/// Building the verifier failed.
#[derive(Debug, thiserror::Error)]
pub enum FirebaseInitError {
    /// The HTTP client for Google's key endpoint could not be built.
    #[error("building the Firebase key client")]
    Http(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl FirebaseVerifier {
    /// Must be built inside a Tokio runtime: the HTTP client spawns a tower
    /// worker. `Gear::init` is async, which is where this happens.
    ///
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new(
        project_id: String,
        identities: Vec<AllowedIdentity>,
    ) -> Result<Self, FirebaseInitError> {
        Self::fetching_from(project_id, identities, JWK_URL, FETCH_DEADLINE)
    }

    fn fetching_from(
        project_id: String,
        identities: Vec<AllowedIdentity>,
        jwk_url: &str,
        fetch_deadline: Duration,
    ) -> Result<Self, FirebaseInitError> {
        // No retries: one fetch is one request, which is what the fetch
        // interval counts. A failed fetch is retried by the next login after
        // the interval, not by the client within it.
        let http = HttpClient::builder()
            .with_otel()
            .timeout(Duration::from_secs(5))
            .total_timeout(fetch_deadline)
            .retry(None)
            .max_body_size(MAX_KEY_SET_BYTES)
            .build()
            .map_err(|e| FirebaseInitError::Http(Box::new(e)))?;
        Ok(Self::assemble(
            project_id,
            identities,
            Some(http),
            jwk_url,
            fetch_deadline,
            None,
        ))
    }

    fn assemble(
        project_id: String,
        identities: Vec<AllowedIdentity>,
        http: Option<HttpClient>,
        jwk_url: &str,
        fetch_deadline: Duration,
        keys: Option<CachedKeys>,
    ) -> Self {
        Self {
            project_id,
            identities,
            source: Arc::new(KeySource {
                http,
                jwk_url: jwk_url.to_owned(),
                fetch_deadline,
                keys: RwLock::new(keys),
                last_failed: AtomicBool::new(false),
            }),
            last_fetch: Mutex::new(None),
        }
    }

    /// A verifier that cannot fetch keys, for tests that inject a signing key
    /// of their own or pin the outage mapping.
    #[cfg(test)]
    pub(crate) fn without_http(project_id: String, identities: Vec<AllowedIdentity>) -> Self {
        Self::assemble(project_id, identities, None, JWK_URL, FETCH_DEADLINE, None)
    }

    /// The uid behind an admin's ID token.
    ///
    /// # Errors
    ///
    /// [`Refusal`] for every reason the token is not an admin's, including the
    /// one that is not the caller's fault.
    pub async fn verify_identity(&self, token: &str) -> Result<String, Refusal> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(Refusal::NotAValidToken(
                "token exceeds size limit".to_owned(),
            ));
        }
        let header =
            decode_header(token).map_err(|e| Refusal::NotAValidToken(format!("header: {e}")))?;

        // Pinned rather than taken from the token. `alg` is attacker-supplied,
        // and honouring it is how a token signed with `none`, or with the
        // public key as an HMAC secret, gets accepted.
        if header.alg != Algorithm::RS256 {
            return Err(Refusal::NotAValidToken(format!(
                "algorithm {:?}, expected RS256",
                header.alg
            )));
        }
        let kid = header
            .kid
            .ok_or_else(|| Refusal::NotAValidToken("no key id".to_owned()))?;

        if kid.is_empty() || kid.len() > 256 {
            return Err(Refusal::NotAValidToken("invalid key id".to_owned()));
        }
        let key = self.key_for(&kid).await?;
        self.check(token, &key)
    }

    /// Everything after the signing key has been found.
    ///
    /// Separated so it can be tested against a key of our own. Fetching
    /// Google's would make every one of those tests a network call, and the
    /// rules that matter here -- audience, issuer, expiry, and above all the
    /// allowlist -- have nothing to do with where the key came from.
    fn check(&self, token: &str, key: &DecodingKey) -> Result<String, Refusal> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.set_audience(&[&self.project_id]);
        validation.set_issuer(&[format!(
            "https://securetoken.google.com/{}",
            self.project_id
        )]);
        // `sub` is the allowlist key, so a token without one must not reach the
        // comparison with an empty string.
        validation.required_spec_claims = ["exp", "iat", "aud", "iss", "sub"]
            .iter()
            .map(|c| (*c).to_owned())
            .collect();

        let data = toolkit_security::jwt::decode::<Claims>(token, key, &validation).map_err(
            |e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::Provider(cause) => {
                    Refusal::CryptoUnavailable(cause.clone())
                }
                _ => Refusal::NotAValidToken(e.to_string()),
            },
        )?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Refusal::KeysUnavailable(e.to_string()))?
            .as_secs();
        let latest = now.saturating_add(CLOCK_SKEW_SECS);
        if data.claims.iat > latest || data.claims.auth_time > latest {
            return Err(Refusal::NotAValidToken(
                "token was issued in the future".to_owned(),
            ));
        }
        let uid = data.claims.sub;
        if uid.is_empty() {
            return Err(Refusal::NotAValidToken("empty subject".to_owned()));
        }

        let email = data.claims.email.filter(|value| !value.trim().is_empty());

        let matched = email.as_deref().is_some_and(|got| {
            data.claims.email_verified
                && self
                    .identities
                    .iter()
                    .any(|allowed| allowed.uid == uid && allowed.email.eq_ignore_ascii_case(got))
        });
        if !matched {
            return Err(Refusal::IdentityNotAllowed { uid, email });
        }

        Ok(uid)
    }

    /// One signing key, from cache when the cache is still good.
    async fn key_for(&self, kid: &str) -> Result<DecodingKey, Refusal> {
        if let Some(key) = self.source.cached_key(kid).await? {
            return Ok(key);
        }

        {
            let mut last_fetch = self.last_fetch.lock().await;
            // A fetch this request waited behind may have brought the key.
            if let Some(key) = self.source.cached_key(kid).await? {
                return Ok(key);
            }
            if last_fetch.is_some_and(|at| at.elapsed() < MIN_FETCH_INTERVAL) {
                return Err(self.source.refusal_between_fetches(kid).await);
            }
            *last_fetch = Some(Instant::now());
            // cancel-safe: the fetch runs in a task of its own, so a request
            // dropped while waiting (a client gone, a gateway timeout) neither
            // aborts it nor loses its outcome; the task records both. The
            // handle is not kept: the task ends within the fetch deadline.
            let fetch = tokio::spawn(Arc::clone(&self.source).refresh());
            fetch
                .await
                .map_err(|e| Refusal::KeysUnavailable(format!("key fetch task: {e}")))??;
        }

        // A `kid` that is still missing after a fresh fetch is a property of the
        // token, not of the cache -- so this is a refusal rather than a retry.
        self.source.cached_key(kid).await?.ok_or_else(|| {
            Refusal::NotAValidToken(format!("no published key matches key id {kid}"))
        })
    }
}

impl KeySource {
    /// The answer for a key that is not cached while the fetch interval runs.
    ///
    /// A missing `kid` is the token's fault only when a fetch has just
    /// succeeded. After a failed fetch, or while one is still running, it may
    /// be a key Google rotated in that we could not get: 503, not 401.
    async fn refusal_between_fetches(&self, kid: &str) -> Refusal {
        if !self.last_failed.load(Ordering::Acquire) && self.has_fresh_keys().await {
            Refusal::NotAValidToken(format!("no published key matches key id {kid}"))
        } else {
            Refusal::KeysUnavailable("the last key fetch failed or has not finished".to_owned())
        }
    }

    async fn has_fresh_keys(&self) -> bool {
        self.keys
            .read()
            .await
            .as_ref()
            .is_some_and(CachedKeys::is_fresh)
    }

    async fn cached_key(&self, kid: &str) -> Result<Option<DecodingKey>, Refusal> {
        let guard = self.keys.read().await;
        let Some(cached) = guard.as_ref() else {
            return Ok(None);
        };
        if !cached.is_fresh() {
            return Ok(None);
        }
        let Some(jwk) = cached.keys.find(kid) else {
            return Ok(None);
        };
        DecodingKey::from_jwk(jwk).map(Some).map_err(|e| {
            warn!(error = %e, %kid, "firebase jwk could not be decoded");
            Refusal::KeysUnavailable(format!("jwk {kid}: {e}"))
        })
    }

    /// One fetch, bounded by the fetch deadline, with its outcome recorded.
    async fn refresh(self: Arc<Self>) -> Result<(), Refusal> {
        let outcome = tokio::time::timeout(self.fetch_deadline, self.fetch())
            .await
            .unwrap_or_else(|_| {
                Err(Refusal::KeysUnavailable(format!(
                    "no key set within {} ms",
                    self.fetch_deadline.as_millis()
                )))
            });
        self.last_failed.store(outcome.is_err(), Ordering::Release);
        outcome
    }

    async fn fetch(&self) -> Result<(), Refusal> {
        let http = self
            .http
            .as_ref()
            .ok_or_else(|| Refusal::KeysUnavailable("no http client".to_owned()))?;

        let response = http
            .get(&self.jwk_url)
            .send()
            .await
            .map_err(|e| Refusal::KeysUnavailable(e.to_string()))?;

        let ttl = key_ttl(response.headers()).unwrap_or(FALLBACK_KEY_TTL);

        // `checked_bytes` refuses a non-2xx answer, so an outage reads as one
        // rather than as a key set that does not parse.
        let body = response
            .checked_bytes()
            .await
            .map_err(|e| Refusal::KeysUnavailable(e.to_string()))?;

        let keys: JwkSet = serde_json::from_slice(&body)
            .map_err(|e| Refusal::KeysUnavailable(format!("key set: {e}")))?;

        debug!(
            count = keys.keys.len(),
            ttl_secs = ttl.as_secs(),
            "fetched Google signing keys"
        );

        *self.keys.write().await = Some(CachedKeys {
            keys,
            fetched_at: Instant::now(),
            ttl,
        });
        Ok(())
    }
}

/// How long Google says its keys are good for; `None` when it does not say.
///
/// Honoured rather than assumed: the keys rotate daily, and caching past the
/// window means refusing genuine tokens for however long the cache outlives the
/// key -- a failure that looks like everybody being logged out at once.
/// Directive names are case-insensitive, as HTTP has them.
fn key_ttl(headers: &http::HeaderMap) -> Option<Duration> {
    let value = headers.get(http::header::CACHE_CONTROL)?.to_str().ok()?;
    let mut max_age = None;
    for directive in value.split(',') {
        let (name, argument) = match directive.split_once('=') {
            Some((name, argument)) => (name.trim(), Some(argument.trim().trim_matches('"'))),
            None => (directive.trim(), None),
        };
        if name.eq_ignore_ascii_case("no-store") || name.eq_ignore_ascii_case("no-cache") {
            return Some(UNCACHEABLE_KEY_TTL);
        }
        if name.eq_ignore_ascii_case("max-age") {
            max_age = argument.and_then(|seconds| seconds.parse::<u64>().ok());
        }
    }
    match max_age? {
        0 => Some(UNCACHEABLE_KEY_TTL),
        seconds => Some(Duration::from_secs(seconds)),
    }
}

#[cfg(test)]
pub(crate) fn with_test_keys(
    project_id: String,
    identities: Vec<AllowedIdentity>,
    keys: JwkSet,
) -> FirebaseVerifier {
    FirebaseVerifier::assemble(
        project_id,
        identities,
        None,
        JWK_URL,
        FETCH_DEADLINE,
        Some(CachedKeys {
            keys,
            fetched_at: Instant::now(),
            ttl: Duration::from_secs(3600),
        }),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "RSA test fixtures")]
pub(crate) mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rsa::{
        RsaPrivateKey,
        pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding},
    };
    use serde::Serialize;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicUsize;

    pub const PROJECT: &str = "firebase-authn-test";
    const ADMIN_UID: &str = "firebase-uid-of-the-owner";
    const ADMIN_EMAIL: &str = "owner@example.com";

    /// A keypair of our own, so these tests exercise the rules rather than the
    /// network. Google's key is not what any of them is about.
    fn generate_keys() -> (String, String) {
        let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).expect("test RSA key");
        let public = private
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("test public PEM");
        let private = private
            .to_pkcs8_pem(LineEnding::LF)
            .expect("test private PEM");
        (private.to_string(), public)
    }
    pub fn test_keys() -> &'static (String, String) {
        static KEYS: OnceLock<(String, String)> = OnceLock::new();
        KEYS.get_or_init(generate_keys)
    }

    fn admins() -> Vec<AllowedIdentity> {
        vec![AllowedIdentity {
            uid: ADMIN_UID.to_owned(),
            email: ADMIN_EMAIL.to_owned(),
        }]
    }

    #[derive(Serialize)]
    struct TestClaims {
        sub: String,
        aud: String,
        iss: String,
        exp: i64,
        iat: i64,
        auth_time: i64,
        email: String,
        email_verified: bool,
    }

    fn verifier() -> FirebaseVerifier {
        // Not `new`: that builds an HTTP client, which needs a runtime and is
        // no part of what is under test here.
        FirebaseVerifier::without_http(PROJECT.to_owned(), admins())
    }

    fn key() -> DecodingKey {
        DecodingKey::from_rsa_pem(test_keys().1.as_bytes()).expect("test public key")
    }

    fn token_for(uid: &str) -> String {
        token_with(uid, ADMIN_EMAIL, true)
    }

    fn token_with(uid: &str, email: &str, email_verified: bool) -> String {
        signed(
            uid,
            email,
            email_verified,
            PROJECT,
            &format!("https://securetoken.google.com/{PROJECT}"),
            3600,
        )
    }

    fn signed(
        uid: &str,
        email: &str,
        email_verified: bool,
        aud: &str,
        iss: &str,
        expires_in: i64,
    ) -> String {
        let now = chrono::Utc::now().timestamp();
        let claims = TestClaims {
            sub: uid.to_owned(),
            aud: aud.to_owned(),
            iss: iss.to_owned(),
            exp: now + expires_in,
            iat: now - 10,
            auth_time: now - 10,
            email: email.to_owned(),
            email_verified,
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".to_owned());
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(test_keys().0.as_bytes()).expect("test private key"),
        )
        .expect("signed")
    }

    #[test]
    fn the_configured_admin_is_admitted() {
        let uid = verifier()
            .check(&token_for(ADMIN_UID), &key())
            .expect("the admin must get in");
        assert_eq!(uid, ADMIN_UID);
    }

    /// The test this whole module exists for.
    ///
    /// Firebase will issue a genuine, correctly signed, unexpired token for
    /// this project to anybody who clicks "Sign in with Google". Every other
    /// check in `check` passes for that token. The allowlist is the only thing
    /// between them and the admin surface, and dropping it is a one-line edit
    /// that no other test here would notice.
    #[test]
    fn a_google_account_outside_the_admin_list_is_refused() {
        let refusal = verifier()
            .check(&token_for("some-other-google-user"), &key())
            .expect_err("a stranger with a valid token must be refused");

        assert!(
            matches!(&refusal, Refusal::IdentityNotAllowed { uid, .. } if uid == "some-other-google-user"),
            "{refusal:?}"
        );
    }

    /// Right uid, somebody else's address on the same genuine token.
    #[test]
    fn the_right_uid_with_the_wrong_email_is_refused() {
        let refusal = verifier()
            .check(&token_with(ADMIN_UID, "stranger@example.com", true), &key())
            .expect_err("uid alone must not admit a different address");

        assert!(
            matches!(&refusal, Refusal::IdentityNotAllowed { uid, .. } if uid == ADMIN_UID),
            "{refusal:?}"
        );
    }

    /// Both halves are allowlisted, but on different entries: one account's
    /// uid with another account's address is neither of them.
    #[test]
    fn one_entrys_uid_with_another_entrys_email_is_refused() {
        let verifier = FirebaseVerifier::without_http(
            PROJECT.to_owned(),
            vec![
                AllowedIdentity {
                    uid: "uid-a".to_owned(),
                    email: "a@example.com".to_owned(),
                },
                AllowedIdentity {
                    uid: "uid-b".to_owned(),
                    email: "b@example.com".to_owned(),
                },
            ],
        );
        assert!(
            verifier
                .check(&token_with("uid-a", "a@example.com", true), &key())
                .is_ok()
        );
        let refusal = verifier
            .check(&token_with("uid-a", "b@example.com", true), &key())
            .expect_err("halves of two entries must not combine");
        assert!(
            matches!(&refusal, Refusal::IdentityNotAllowed { uid, .. } if uid == "uid-a"),
            "{refusal:?}"
        );
    }

    #[test]
    fn an_unverified_admin_email_is_refused() {
        let refusal = verifier()
            .check(&token_with(ADMIN_UID, ADMIN_EMAIL, false), &key())
            .expect_err("an unverified claim must not count as the allowlisted address");

        assert!(
            matches!(&refusal, Refusal::IdentityNotAllowed { uid, .. } if uid == ADMIN_UID),
            "{refusal:?}"
        );
    }

    /// Signed by the same Google key, minted for a different project.
    #[test]
    fn a_token_for_another_firebase_project_is_refused() {
        let other = signed(
            ADMIN_UID,
            ADMIN_EMAIL,
            true,
            "somebody-elses-project",
            "https://securetoken.google.com/somebody-elses-project",
            3600,
        );
        assert!(matches!(
            verifier().check(&other, &key()),
            Err(Refusal::NotAValidToken(_))
        ));
    }

    /// Our issuer and somebody else's audience: only the audience check
    /// stands between this token and the allowlist.
    #[test]
    fn a_token_for_another_audience_is_refused() {
        let other = signed(
            ADMIN_UID,
            ADMIN_EMAIL,
            true,
            "somebody-elses-project",
            &format!("https://securetoken.google.com/{PROJECT}"),
            3600,
        );
        assert!(matches!(
            verifier().check(&other, &key()),
            Err(Refusal::NotAValidToken(_))
        ));
    }

    /// The right project, the right admin, and the wrong issuer: a token minted
    /// by something that is not Firebase.
    #[test]
    fn a_token_from_the_wrong_issuer_is_refused() {
        let forged = signed(
            ADMIN_UID,
            ADMIN_EMAIL,
            true,
            PROJECT,
            "https://example.com/",
            3600,
        );
        assert!(matches!(
            verifier().check(&forged, &key()),
            Err(Refusal::NotAValidToken(_))
        ));
    }

    #[test]
    fn an_expired_id_token_is_refused() {
        let stale = signed(
            ADMIN_UID,
            ADMIN_EMAIL,
            true,
            PROJECT,
            &format!("https://securetoken.google.com/{PROJECT}"),
            // Expiry takes no leeway; ten minutes past is plainly expired.
            -600,
        );
        assert!(matches!(
            verifier().check(&stale, &key()),
            Err(Refusal::NotAValidToken(_))
        ));
    }

    /// A token signed by a key that is not the one we verify with.
    #[test]
    fn a_token_signed_by_an_unknown_key_is_refused() {
        let wrong_key = DecodingKey::from_rsa_pem(generate_keys().1.as_bytes()).expect("other key");
        assert!(matches!(
            verifier().check(&token_for(ADMIN_UID), &wrong_key),
            Err(Refusal::NotAValidToken(_))
        ));
    }

    #[test]
    fn the_key_url_is_the_firebase_one_not_the_oauth_one() {
        // These differ by one path and fail in opposite directions: the OAuth
        // set verifies nothing Firebase signs, so every genuine admin token
        // would be refused with a signature error.
        assert!(JWK_URL.contains("securetoken@system.gserviceaccount.com"));
        assert!(!JWK_URL.contains("oauth2"));
    }

    fn cache_control(value: &str) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CACHE_CONTROL, value.parse().unwrap());
        headers
    }

    #[test]
    fn a_cache_lifetime_is_read_from_the_response() {
        assert_eq!(
            key_ttl(&cache_control(
                "public, max-age=19008, must-revalidate, no-transform"
            )),
            Some(Duration::from_secs(19_008))
        );
        assert_eq!(
            key_ttl(&cache_control("public, Max-Age=600")),
            Some(Duration::from_secs(600)),
            "directive names are case-insensitive"
        );
    }

    #[test]
    fn a_response_that_says_not_to_cache_is_kept_one_fetch_interval() {
        for value in ["max-age=0", "no-store", "public, No-Cache, max-age=3600"] {
            assert_eq!(
                key_ttl(&cache_control(value)),
                Some(UNCACHEABLE_KEY_TTL),
                "{value}"
            );
        }
    }

    #[test]
    fn a_missing_or_malformed_cache_header_falls_back() {
        assert_eq!(key_ttl(&http::HeaderMap::new()), None);
        for value in [
            "public",
            "max-age=soon",
            "max-age=-1",
            "max-age=99999999999999999999999",
        ] {
            assert_eq!(key_ttl(&cache_control(value)), None, "{value}");
        }
    }

    pub fn signing_keys() -> JwkSet {
        use base64::Engine as _;
        use rsa::pkcs8::DecodePublicKey;
        use rsa::traits::PublicKeyParts;
        let public = rsa::RsaPublicKey::from_public_key_pem(&test_keys().1).unwrap();
        serde_json::from_value(serde_json::json!({"keys": [{"kty": "RSA", "kid": "test-key", "alg": "RS256", "use": "sig",
            "n": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
            "e": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.e().to_bytes_be())}]})).unwrap()
    }

    /// The admin's valid claims, with `extra` overriding or adding fields
    /// (`null` removes one), signed under key id `kid`.
    fn signed_claims_under(kid: Option<&str>, extra: serde_json::Value) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut claims = serde_json::json!({"sub": ADMIN_UID, "aud": PROJECT, "iss": format!("https://securetoken.google.com/{PROJECT}"),
            "iat": now - 10, "auth_time": now - 10, "exp": now + 3600, "email": ADMIN_EMAIL, "email_verified": true});
        let serde_json::Value::Object(extra) = extra else {
            panic!("test claim overrides must be an object")
        };
        let object = claims.as_object_mut().unwrap();
        for (name, value) in extra {
            if value.is_null() {
                object.remove(&name);
            } else {
                object.insert(name, value);
            }
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_owned);
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(test_keys().0.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    pub fn signed_claims(extra: serde_json::Value) -> String {
        signed_claims_under(Some("test-key"), extra)
    }

    /// A token valid in every respect but its key id, so a refusal can only
    /// come from the key lookup.
    fn token_with_kid(kid: &str) -> String {
        signed_claims_under(Some(kid), serde_json::json!({}))
    }

    #[tokio::test]
    async fn cached_jwk_verifies_the_complete_token_path() {
        let verifier = with_test_keys(PROJECT.to_owned(), admins(), signing_keys());
        assert_eq!(
            verifier
                .verify_identity(&token_for(ADMIN_UID))
                .await
                .unwrap(),
            ADMIN_UID
        );
    }

    #[test]
    fn future_issue_and_authentication_times_are_rejected() {
        let future = chrono::Utc::now().timestamp() + 3600;
        for field in ["iat", "auth_time", "nbf"] {
            let token = signed_claims(serde_json::json!({field: future}));
            assert!(
                matches!(
                    verifier().check(&token, &key()),
                    Err(Refusal::NotAValidToken(_))
                ),
                "future {field}"
            );
        }
    }

    /// Up to [`CLOCK_SKEW_SECS`] ahead is clock drift; well past it is not.
    #[test]
    fn issue_times_within_the_clock_skew_are_accepted() {
        let skew = i64::try_from(CLOCK_SKEW_SECS).unwrap();
        let now = chrono::Utc::now().timestamp();
        for field in ["iat", "auth_time"] {
            let near = signed_claims(serde_json::json!({field: now + skew - 15}));
            assert!(
                verifier().check(&near, &key()).is_ok(),
                "{field} within skew"
            );
            let far = signed_claims(serde_json::json!({field: now + skew + 30}));
            assert!(
                matches!(
                    verifier().check(&far, &key()),
                    Err(Refusal::NotAValidToken(_))
                ),
                "{field} past skew"
            );
        }
    }

    #[test]
    fn missing_issue_or_authentication_time_and_just_expired_tokens_are_rejected() {
        for field in ["iat", "auth_time"] {
            let token = signed_claims(serde_json::json!({field: null}));
            assert!(
                matches!(
                    verifier().check(&token, &key()),
                    Err(Refusal::NotAValidToken(_))
                ),
                "missing {field}"
            );
        }
        let now = chrono::Utc::now().timestamp();
        let token = signed_claims(serde_json::json!({"iat": now - 100, "exp": now - 1}));
        assert!(matches!(
            verifier().check(&token, &key()),
            Err(Refusal::NotAValidToken(_))
        ));
    }

    #[tokio::test]
    async fn missing_or_unknown_kid_and_key_outage_fail_closed() {
        // No keys and no way to fetch them: an outage, not the token's fault.
        assert!(matches!(
            verifier().verify_identity(&token_for(ADMIN_UID)).await,
            Err(Refusal::KeysUnavailable(_))
        ));

        let cached = with_test_keys(PROJECT.to_owned(), admins(), signing_keys());
        // An unknown key id sends a fetch, which cannot happen here.
        assert!(matches!(
            cached.verify_identity(&token_with_kid("unknown-key")).await,
            Err(Refusal::KeysUnavailable(_))
        ));
        assert!(matches!(
            cached
                .verify_identity(&signed_claims_under(None, serde_json::json!({})))
                .await,
            Err(Refusal::NotAValidToken(_))
        ));
    }

    /// What the stub key server answers.
    #[derive(Clone, Copy)]
    enum Stub {
        /// [`signing_keys`] with `max-age=3600`.
        Keys,
        /// [`signing_keys`] after a delay.
        SlowKeys(Duration),
        /// This status and no key set.
        Status(u16),
        /// A 200 whose body is past [`MAX_KEY_SET_BYTES`].
        Oversized,
        /// The headers, then nothing.
        Stall,
    }

    /// Serves `stub` over plain HTTP and counts the requests.
    async fn key_server(stub: Stub) -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/keys", listener.local_addr().unwrap());
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fetches);
        let keys = serde_json::to_vec(&signing_keys()).unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let keys = keys.clone();
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let Ok(read) = stream.read(&mut request).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = match stub {
                        Stub::Keys | Stub::Stall => (200, keys),
                        Stub::SlowKeys(delay) => {
                            tokio::time::sleep(delay).await;
                            (200, keys)
                        }
                        Stub::Status(status) => (status, b"unavailable".to_vec()),
                        Stub::Oversized => (200, vec![b' '; MAX_KEY_SET_BYTES + 1]),
                    };
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                         cache-control: max-age=3600\r\ncontent-length: {}\r\n\
                         connection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).await.ok();
                    if matches!(stub, Stub::Stall) {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        return;
                    }
                    stream.write_all(&body).await.ok();
                });
            }
        });
        (url, fetches)
    }

    fn fetching(url: &str, deadline: Duration) -> FirebaseVerifier {
        FirebaseVerifier::fetching_from(PROJECT.to_owned(), admins(), url, deadline).unwrap()
    }

    /// An invented `kid` needs no valid signature to reach the fetch, so it
    /// must not buy one per request: concurrent misses share one fetch, and a
    /// fresh key set is not fetched again within the interval.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unknown_key_ids_cost_one_fetch_per_interval() {
        let (url, fetches) = key_server(Stub::Keys).await;
        let verifier = Arc::new(fetching(&url, FETCH_DEADLINE));

        let misses: Vec<_> = (0..16)
            .map(|i| {
                let verifier = Arc::clone(&verifier);
                tokio::spawn(async move {
                    verifier
                        .verify_identity(&token_with_kid(&format!("invented-{i}")))
                        .await
                })
            })
            .collect();
        for miss in misses {
            assert!(matches!(
                miss.await.unwrap(),
                Err(Refusal::NotAValidToken(_))
            ));
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "one fetch for 16 misses");

        assert!(matches!(
            verifier
                .verify_identity(&token_with_kid("invented-again"))
                .await,
            Err(Refusal::NotAValidToken(_))
        ));
        assert_eq!(
            verifier
                .verify_identity(&token_for(ADMIN_UID))
                .await
                .unwrap(),
            ADMIN_UID
        );
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "a fresh set is not refetched"
        );
    }

    /// A failed fetch is not retried, by the client or by the next request,
    /// within the interval; and a key that may have rotated in during the
    /// outage is answered 503, not 401, for the rest of it.
    #[tokio::test]
    async fn a_failed_fetch_is_not_retried_within_the_interval() {
        let (url, fetches) = key_server(Stub::Status(503)).await;
        let verifier = fetching(&url, FETCH_DEADLINE);
        // A cached set that is still good, as during an outage that began after
        // the last successful fetch.
        *verifier.source.keys.write().await = Some(CachedKeys {
            keys: signing_keys(),
            fetched_at: Instant::now(),
            ttl: Duration::from_secs(3600),
        });

        for attempt in 0..2 {
            assert!(
                matches!(
                    verifier
                        .verify_identity(&token_with_kid("rotated-in"))
                        .await,
                    Err(Refusal::KeysUnavailable(_))
                ),
                "attempt {attempt}"
            );
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "one request, no retries");
        assert_eq!(
            verifier
                .verify_identity(&token_for(ADMIN_UID))
                .await
                .unwrap(),
            ADMIN_UID,
            "the cached key still serves"
        );
    }

    #[tokio::test]
    async fn expired_cached_keys_are_fetched_again() {
        let (url, fetches) = key_server(Stub::Keys).await;
        let verifier = fetching(&url, FETCH_DEADLINE);
        *verifier.source.keys.write().await = Some(CachedKeys {
            keys: signing_keys(),
            fetched_at: Instant::now(),
            ttl: Duration::ZERO,
        });
        assert_eq!(
            verifier
                .verify_identity(&token_for(ADMIN_UID))
                .await
                .unwrap(),
            ADMIN_UID
        );
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    /// The HTTP client's timeouts end at the headers; the fetch deadline
    /// covers the body too.
    #[tokio::test]
    async fn a_stalled_key_download_ends_at_the_deadline() {
        let (url, _) = key_server(Stub::Stall).await;
        let verifier = fetching(&url, Duration::from_millis(300));
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            verifier.verify_identity(&token_for(ADMIN_UID)),
        )
        .await
        .expect("the fetch deadline must end a stalled download");
        assert!(matches!(outcome, Err(Refusal::KeysUnavailable(_))));
    }

    #[tokio::test]
    async fn an_error_status_or_an_oversized_body_is_an_outage() {
        for stub in [Stub::Status(500), Stub::Oversized] {
            let (url, _) = key_server(stub).await;
            assert!(matches!(
                fetching(&url, FETCH_DEADLINE)
                    .verify_identity(&token_for(ADMIN_UID))
                    .await,
                Err(Refusal::KeysUnavailable(_))
            ));
        }
    }

    /// A request that stops waiting does not take the fetch with it: the key
    /// set still lands, and the next login uses it without another fetch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_request_does_not_cancel_its_fetch() {
        let (url, fetches) = key_server(Stub::SlowKeys(Duration::from_millis(300))).await;
        let verifier = fetching(&url, FETCH_DEADLINE);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                verifier.verify_identity(&token_for(ADMIN_UID)),
            )
            .await
            .is_err(),
            "the first request gives up while the key server is still answering"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !verifier.source.has_fresh_keys().await {
            assert!(Instant::now() < deadline, "the detached fetch never landed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            verifier
                .verify_identity(&token_for(ADMIN_UID))
                .await
                .unwrap(),
            ADMIN_UID
        );
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn symmetric_algorithms_are_rejected_before_key_fetch() {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("test-key".to_owned());
        let token = encode(
            &header,
            &serde_json::json!({}),
            &EncodingKey::from_secret(b"not-a-firebase-key"),
        )
        .unwrap();
        assert!(matches!(
            verifier().verify_identity(&token).await,
            Err(Refusal::NotAValidToken(_))
        ));
    }

    /// Padded past the limit, the admin's own genuine token is refused: the
    /// size guard, not a parse error, is what refuses it.
    #[tokio::test]
    async fn an_oversized_token_is_refused_even_when_genuine() {
        let verifier = with_test_keys(PROJECT.to_owned(), admins(), signing_keys());
        let padded = signed_claims(serde_json::json!({"padding": "x".repeat(MAX_TOKEN_BYTES)}));
        assert!(padded.len() > MAX_TOKEN_BYTES);
        assert!(
            verifier
                .check(&padded, &key())
                .is_ok_and(|uid| uid == ADMIN_UID),
            "without the guard this token verifies"
        );
        assert!(matches!(
            verifier.verify_identity(&padded).await,
            Err(Refusal::NotAValidToken(reason)) if reason.contains("size")
        ));
    }
}
