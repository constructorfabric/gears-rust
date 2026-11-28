use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use toolkit_utils::SecretString;
use tracing::warn;

use super::controller::{RateLimitController, Readiness};
use super::registry::{ControllerRegistry, TokenFingerprint};
use crate::domain::ports::github::RepoTokenAccess;

pub struct PooledToken {
    pub token: Option<SecretString>,
    pub controller: Arc<RateLimitController>,
    fingerprint: Option<TokenFingerprint>,
    dropped: AtomicBool,
    scopes: OnceLock<Option<Vec<String>>>,
}

impl PooledToken {
    /// The first 12 hex digits of the token's SHA-256; `None` without a token.
    pub fn label(&self) -> Option<&str> {
        self.fingerprint.as_ref().map(TokenFingerprint::short)
    }

    pub fn is_dropped(&self) -> bool {
        self.dropped.load(Ordering::Acquire)
    }

    /// Keep the scopes from the token's first answer: a classic token lists
    /// them in `X-OAuth-Scopes`, a fine-grained or app token sends no such
    /// header, and its access is only learned from a repository's answers.
    pub fn note_scopes(&self, header: Option<&str>) {
        if self.fingerprint.is_none() {
            return;
        }
        self.scopes.get_or_init(|| {
            header.map(|header| {
                header
                    .split(',')
                    .map(str::trim)
                    .filter(|scope| !scope.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
        });
    }

    /// `false` only for a classic token whose scopes are known and lack
    /// `repo`, the scope a private repository needs.
    pub fn may_read_private(&self) -> bool {
        !matches!(self.scopes.get(), Some(Some(scopes)) if !scopes.iter().any(|scope| scope == "repo"))
    }

    /// Take this token out of the pool after GitHub answered `401` to it, so
    /// the request can be retried with another token. `false` for the entry
    /// without a token, whose `401` stays an error.
    pub fn drop_from_pool(&self) -> bool {
        let Some(fingerprint) = &self.fingerprint else {
            return false;
        };
        if !self.dropped.swap(true, Ordering::AcqRel) {
            warn!(
                token = fingerprint.short(),
                "GitHub answered 401 Unauthorized to a pool token; it stays out of the pool until a restart"
            );
        }
        true
    }
}

pub struct TokenPool {
    tokens: Vec<Arc<PooledToken>>,
}

impl TokenPool {
    #[must_use]
    pub fn new(tokens: Vec<SecretString>, registry: &ControllerRegistry) -> Self {
        if tokens.is_empty() {
            return Self {
                tokens: vec![Arc::new(PooledToken {
                    token: None,
                    controller: Arc::new(RateLimitController::new()),
                    fingerprint: None,
                    dropped: AtomicBool::new(false),
                    scopes: OnceLock::new(),
                })],
            };
        }
        Self {
            tokens: tokens
                .into_iter()
                .map(|token| {
                    let fingerprint = TokenFingerprint::of(token.expose());
                    let controller = registry.get_or_insert(fingerprint.clone());
                    Arc::new(PooledToken {
                        token: Some(token),
                        controller,
                        fingerprint: Some(fingerprint),
                        dropped: AtomicBool::new(false),
                        scopes: OnceLock::new(),
                    })
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn first(&self) -> &PooledToken {
        &self.tokens[0]
    }

    pub fn iter(&self) -> impl Iterator<Item = &PooledToken> {
        self.tokens.iter().map(Arc::as_ref)
    }

    /// The token for the next request on a repository that is `public`
    /// (`None` before the run knows). Tokens GitHub hid the repository from
    /// are left out. On a repository that may be private it prefers tokens
    /// whose scopes allow it, and for GraphQL, which answers a hidden
    /// repository with a partial `null` rather than `404`, tokens already
    /// shown the repository; when no token fits, it falls back to the rest.
    pub async fn pick(
        &self,
        graphql: bool,
        public: Option<bool>,
        access: &RepoTokenAccess,
    ) -> Option<Arc<PooledToken>> {
        let open: Vec<&Arc<PooledToken>> = self
            .tokens
            .iter()
            .filter(|pooled| !pooled.is_dropped())
            .filter(|pooled| {
                pooled
                    .label()
                    .is_none_or(|label| access.sees(label) != Some(false))
            })
            .collect();
        let fitting: Vec<&Arc<PooledToken>> = open
            .iter()
            .copied()
            .filter(|pooled| public != Some(false) || pooled.may_read_private())
            .filter(|pooled| {
                !graphql
                    || public == Some(true)
                    || pooled
                        .label()
                        .is_none_or(|label| access.sees(label) == Some(true))
            })
            .collect();
        let candidates = if fitting.is_empty() { open } else { fitting };
        let mut picked: Option<(&Arc<PooledToken>, Rank)> = None;
        for pooled in candidates {
            let rank = rank(pooled.controller.readiness(graphql).await);
            if picked.is_none_or(|(_, best)| rank < best) {
                picked = Some((pooled, rank));
            }
        }
        picked.map(|(pooled, _)| Arc::clone(pooled))
    }
}

type Rank = (u8, i64);

fn rank(readiness: Readiness) -> Rank {
    match readiness {
        Readiness::Now(spare) => (0, -i64::from(spare)),
        Readiness::Later(Some(at)) => (1, at.timestamp_millis()),
        Readiness::Later(None) => (2, 0),
    }
}
