//! Domain models for the `AuthN` resolver gear.

use std::fmt;
use std::sync::Arc;

use secrecy::SecretString;
use time::OffsetDateTime;

use toolkit_security::SecurityContext;

use crate::scopes::ScopeSet;

/// Opaque identifier a plugin assigns to a result it may serve again.
///
/// Callers compare tags for equality and return them in
/// [`RequestCacheControl::Refresh`]. Tags need not be token hashes and must
/// contain no secrets: they may be logged.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CacheTag(Arc<str>);

impl CacheTag {
    /// Wrap a plugin-defined tag.
    #[must_use]
    pub fn new(tag: impl Into<String>) -> Self {
        Self(Arc::from(tag.into()))
    }

    /// The plugin-defined tag value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CacheTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CacheTag").field(&&*self.0).finish()
    }
}

/// How long the producer allows an [`AuthenticationResult`] to be reused.
///
/// Defaults to **no-store** (`cache_until == None`). Plugins opt in with
/// [`CacheControl::until`]; reuse ends at the earlier of that deadline and
/// known [`SecurityContext::expires_at`]. Unknown expiry still requires a
/// finite `cache_until`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheControl {
    /// Plugin-defined tag of this result, for targeted refresh. Optional.
    pub tag: Option<CacheTag>,
    /// Latest instant at which the result may still be reused. `None` means
    /// no-store.
    pub cache_until: Option<OffsetDateTime>,
}

impl CacheControl {
    /// The result must not be reused (the default).
    #[must_use]
    pub const fn no_store() -> Self {
        Self {
            tag: None,
            cache_until: None,
        }
    }

    /// The result may be reused until `cache_until`.
    #[must_use]
    pub const fn until(cache_until: OffsetDateTime) -> Self {
        Self {
            tag: None,
            cache_until: Some(cache_until),
        }
    }

    /// Attach a plugin-defined tag.
    #[must_use]
    pub fn with_tag(mut self, tag: CacheTag) -> Self {
        self.tag = Some(tag);
        self
    }

    /// Whether the result must not be reused.
    #[must_use]
    pub const fn is_no_store(&self) -> bool {
        self.cache_until.is_none()
    }
}

/// What the caller allows the plugin to serve from its own cache.
///
/// [`AuthenticationRequest::new`] requires fresh validation.
/// [`ClientCredentialsRequest::new`] allows plugin-cache hits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestCacheControl {
    /// The plugin may return a result it cached earlier.
    AllowCached,
    /// Re-acquire unless the plugin can prove its cached result is newer than the
    /// supplied tag. A missing, equal, unknown or incomparable tag requires fresh
    /// acquisition, including when replicas cannot compare their generations.
    Refresh {
        /// Tag of the result to replace, or `None` for unconditional refresh.
        replacing: Option<CacheTag>,
    },
}

impl RequestCacheControl {
    /// Re-acquire unconditionally.
    #[must_use]
    pub const fn refresh() -> Self {
        Self::Refresh { replacing: None }
    }

    /// Refresh conditionally under the freshness rules of [`Self::Refresh`].
    #[must_use]
    pub const fn refresh_unless_newer_than(tag: CacheTag) -> Self {
        Self::Refresh {
            replacing: Some(tag),
        }
    }
}

/// Process-local identity shared by clones of one published result.
/// Never serialized. Cache, slot and publication together prevent stale invalidations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CacheOrigin {
    cache: u64,
    slot: u64,
    generation: u64,
}

impl CacheOrigin {
    /// Cache ids are unique per process; slot ids are never reused within a
    /// cache.
    pub(crate) const fn new(cache: u64, slot: u64, generation: u64) -> Self {
        Self {
            cache,
            slot,
            generation,
        }
    }

    /// Id of the cache instance that published the result.
    pub(crate) const fn cache(self) -> u64 {
        self.cache
    }

    /// Slot (scope set or publication) within that cache; never reused.
    pub(crate) const fn slot(self) -> u64 {
        self.slot
    }

    /// Publication number within the slot.
    #[cfg(test)]
    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }
}

/// Validated identity and the producer's reuse policy.
#[derive(Debug, Clone)]
pub struct AuthenticationResult {
    /// Validated identity, scopes, optional bearer token and credential expiry.
    pub security_context: SecurityContext,

    /// Whether and until when the result may be reused. Defaults to no-store.
    pub cache_control: CacheControl,

    /// Set only on results returned by a local cache such as
    /// [`crate::ClientCredentialsProvider`].
    origin: Option<CacheOrigin>,
}

impl AuthenticationResult {
    /// A result that must not be reused ([`CacheControl::no_store`]).
    #[must_use]
    pub fn new(security_context: SecurityContext) -> Self {
        Self {
            security_context,
            cache_control: CacheControl::no_store(),
            origin: None,
        }
    }

    /// Replace the reuse policy.
    #[must_use]
    pub fn with_cache_control(mut self, cache_control: CacheControl) -> Self {
        self.cache_control = cache_control;
        self
    }

    /// The local cache publication that returned this result, if any.
    pub(crate) fn origin(&self) -> Option<CacheOrigin> {
        self.origin
    }

    /// Attach the publishing cache's local invalidation identity.
    pub(crate) fn with_origin(mut self, origin: CacheOrigin) -> Self {
        self.origin = Some(origin);
        self
    }
}

/// Request to validate a bearer token.
///
/// Construct with [`AuthenticationRequest::new`]. `Debug` omits the token.
#[derive(Clone)]
#[non_exhaustive]
pub struct AuthenticationRequest {
    /// The raw bearer token, without the `Bearer ` prefix.
    pub bearer_token: SecretString,

    /// Controls reuse by plugins that cache validations.
    pub cache_control: RequestCacheControl,
}

impl AuthenticationRequest {
    /// Require fresh validation. Set [`RequestCacheControl::AllowCached`] to allow a cache hit.
    #[must_use]
    pub fn new(bearer_token: impl Into<SecretString>) -> Self {
        Self {
            bearer_token: bearer_token.into(),
            cache_control: RequestCacheControl::refresh(),
        }
    }

    /// Set what the plugin may serve from its own cache.
    #[must_use]
    pub fn with_cache_control(mut self, cache_control: RequestCacheControl) -> Self {
        self.cache_control = cache_control;
        self
    }
}

impl fmt::Debug for AuthenticationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthenticationRequest")
            .field("cache_control", &self.cache_control)
            .finish_non_exhaustive()
    }
}

/// Request to exchange `OAuth2` client credentials for a `SecurityContext`.
///
/// The caller provides its credentials; the `AuthN` plugin knows the token
/// endpoint / issuer URL from its own configuration. Construct with
/// [`ClientCredentialsRequest::new`].
#[derive(Clone)]
#[non_exhaustive]
pub struct ClientCredentialsRequest {
    /// `OAuth2` client identifier.
    pub client_id: String,

    /// `OAuth2` client secret.
    pub client_secret: SecretString,

    /// Scopes to request from the `IdP`, validated and in canonical form.
    /// Passed as the `scope` parameter of the `OAuth2` `client_credentials`
    /// grant. `None` requests the `IdP` default scopes.
    pub scopes: Option<ScopeSet>,

    /// Whether the plugin may serve a result from its own cache.
    pub cache_control: RequestCacheControl,
}

impl ClientCredentialsRequest {
    /// A request for the `IdP` default scopes that allows cached results.
    #[must_use]
    pub fn new(client_id: impl Into<String>, client_secret: SecretString) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret,
            scopes: None,
            cache_control: RequestCacheControl::AllowCached,
        }
    }

    /// Request exactly `scopes` instead of the `IdP` defaults.
    #[must_use]
    pub fn with_scopes(mut self, scopes: ScopeSet) -> Self {
        self.scopes = Some(scopes);
        self
    }

    /// Set what the plugin may serve from its own cache.
    #[must_use]
    pub fn with_cache_control(mut self, cache_control: RequestCacheControl) -> Self {
        self.cache_control = cache_control;
        self
    }
}
