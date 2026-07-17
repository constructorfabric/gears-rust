//! In-memory, TTL-bounded store for transient OAuth authorization state.
//!
//! The PKCE verifier and CSRF `state` produced by `begin` are single-use and
//! short-lived, so they live only in process memory with a TTL — never in
//! durable secret storage. If the callback never arrives, the entry is evicted
//! when its TTL lapses (no orphaned secret material accumulates).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use uuid::Uuid;

/// Transient state captured at `begin` and consumed on the browser callback.
///
/// The callback is unauthenticated, so the acting identity is captured here at
/// `begin` time and recovered via the unguessable CSRF `state`.
#[derive(Clone)]
pub(crate) struct PendingAuthorization {
    /// Subject that initiated the flow.
    pub subject_id: Uuid,
    /// Home tenant of the initiating subject.
    pub subject_tenant_id: Uuid,
    /// Upstream being authorized.
    pub upstream_id: Uuid,
    /// Authorization-server token endpoint for the code exchange.
    pub token_endpoint: String,
    /// Registered client identifier.
    pub client_id: String,
    /// Client secret for confidential clients; absent for public/PKCE clients.
    pub client_secret: Option<String>,
    /// PKCE code verifier bound to the challenge in the authorize URL.
    pub code_verifier: String,
    /// Redirect URI registered and echoed on the token exchange.
    pub redirect_uri: String,
    /// Effective (server-intersected) scopes requested.
    pub scopes: Vec<String>,
    /// Allowlisted URL the browser is redirected to once the callback
    /// completes (success or failure). Captured at `begin` after allowlist
    /// validation; not secret.
    pub return_to: String,
}

impl std::fmt::Debug for PendingAuthorization {
    /// Redacts OAuth secret material (client secret, PKCE code verifier).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("subject_id", &self.subject_id)
            .field("subject_tenant_id", &self.subject_tenant_id)
            .field("upstream_id", &self.upstream_id)
            .field("token_endpoint", &self.token_endpoint)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("code_verifier", &"[REDACTED]")
            .field("redirect_uri", &self.redirect_uri)
            .field("scopes", &self.scopes)
            .field("return_to", &self.return_to)
            .finish()
    }
}

struct Entry {
    inserted: Instant,
    pending: PendingAuthorization,
}

/// In-memory registry of pending authorizations keyed by CSRF `state`.
///
/// Thread-safe (synchronous mutex; critical sections are trivial map ops).
/// Expired entries are purged opportunistically on every `insert`/`take`.
pub(crate) struct PendingAuthorizationStore {
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry>>,
}

impl PendingAuthorizationStore {
    #[must_use]
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Store `pending` under `state`, purging any expired entries first.
    pub(crate) fn insert(&self, state: String, pending: PendingAuthorization) {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        Self::evict_expired(&mut entries, self.ttl, now);
        entries.insert(
            state,
            Entry {
                inserted: now,
                pending,
            },
        );
    }

    /// Remove and return the pending entry for `state`, or `None` if it is
    /// absent or has expired. Also purges other expired entries.
    pub(crate) fn take(&self, state: &str) -> Option<PendingAuthorization> {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        Self::evict_expired(&mut entries, self.ttl, now);
        entries.remove(state).map(|e| e.pending)
    }

    fn evict_expired(entries: &mut HashMap<String, Entry>, ttl: Duration, now: Instant) {
        entries.retain(|_, e| now.duration_since(e.inserted) < ttl);
    }
}
