// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Where the Vault token comes from, and how it is re-read.
//!
//! The plugin authenticates with a Vault token and nothing else: no login
//! method, no renewal, no background task. The token is configured through
//! exactly one source:
//!
//! * `token`: inline in the configuration (after `${VAR}` expansion);
//! * `token_env`: the name of an environment variable, read when the plugin
//!   starts;
//! * `token_file`: a file, for example the sink of a Vault Agent sidecar that
//!   keeps it fresh. It is read on first use (so the plugin can start before
//!   the sidecar has written it) and again whenever Vault answers `403`.
//!
//! The current token is cached; a `403` makes the transport call
//! [`TokenStore::refreshed_after_rejection`], which re-reads the source once
//! and reports a token only if it differs from the rejected one. The token is
//! never logged and never part of an error message.
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context as _, bail};
use tracing::{info, warn};

use crate::config::{VaultCredStorePluginConfig, VaultToken};

/// Why a token could not be obtained. Carries no part of the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// The environment variable is not set (or not valid Unicode).
    NotSet,
    /// The file could not be read.
    Unreadable(std::io::ErrorKind),
    /// The source is empty (or only whitespace).
    Empty,
    /// The token holds characters that cannot be sent in an HTTP header.
    Malformed,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSet => f.write_str("the environment variable is not set"),
            Self::Unreadable(kind) => write!(f, "the token file could not be read ({kind})"),
            Self::Empty => f.write_str("the token is empty"),
            Self::Malformed => f.write_str("the token contains characters not allowed in a header"),
        }
    }
}

impl std::error::Error for TokenError {}

/// The configured token source.
#[derive(Debug, Clone)]
pub enum TokenSource {
    /// `token`: the value itself.
    Inline(VaultToken),
    /// `token_env`: the name of the variable.
    Env(String),
    /// `token_file`: the path of the file.
    File(PathBuf),
}

impl TokenSource {
    /// Picks the source out of `cfg`.
    ///
    /// # Errors
    /// Fails unless exactly one of `token`, `token_file`, `token_env` is set.
    pub fn from_config(cfg: &VaultCredStorePluginConfig) -> anyhow::Result<Self> {
        match (&cfg.token, &cfg.token_file, &cfg.token_env) {
            (Some(token), None, None) => Ok(Self::Inline(token.clone())),
            (None, Some(path), None) => Ok(Self::File(PathBuf::from(path))),
            (None, None, Some(name)) => Ok(Self::Env(name.clone())),
            _ => bail!("exactly one of `token`, `token_file`, `token_env` must be set"),
        }
    }

    /// Name of the configuration key that selected this source.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Inline(_) => "token",
            Self::Env(_) => "token_env",
            Self::File(_) => "token_file",
        }
    }

    /// Reads the token from the source, right now.
    async fn read(&self) -> Result<Arc<str>, TokenError> {
        match self {
            Self::Inline(token) => normalize(token.expose()),
            Self::Env(name) => read_env(name),
            Self::File(path) => {
                let raw = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| TokenError::Unreadable(e.kind()))?;
                normalize(&raw)
            }
        }
    }
}

fn read_env(name: &str) -> Result<Arc<str>, TokenError> {
    normalize(&std::env::var(name).map_err(|_| TokenError::NotSet)?)
}

/// Trims surrounding whitespace (a token file usually ends with a newline)
/// and rejects an empty token or one that cannot be an HTTP header value.
fn normalize(raw: &str) -> Result<Arc<str>, TokenError> {
    let token = raw.trim();
    if token.is_empty() {
        return Err(TokenError::Empty);
    }
    if !token.chars().all(|c| c.is_ascii_graphic()) {
        return Err(TokenError::Malformed);
    }
    Ok(Arc::from(token))
}

/// The current token plus the means to re-read it.
pub struct TokenStore {
    source: TokenSource,
    cached: Mutex<Option<Arc<str>>>,
}

impl TokenStore {
    /// Creates the store. An inline token and a `token_env` variable are
    /// resolved now, so a missing or malformed one fails startup; a
    /// `token_file` is only read on first use, because a Vault Agent sidecar
    /// may not have written it yet when the plugin starts.
    ///
    /// # Errors
    /// The inline token or the environment variable is missing, empty or
    /// malformed. The message never contains the token.
    pub fn new(source: TokenSource) -> anyhow::Result<Self> {
        let initial = match &source {
            TokenSource::Inline(token) => {
                Some(normalize(token.expose()).context("`token` is not a usable Vault token")?)
            }
            TokenSource::Env(name) => Some(
                read_env(name)
                    .with_context(|| format!("`token_env` ({name}) is not a usable Vault token"))?,
            ),
            TokenSource::File(_) => None,
        };
        Ok(Self {
            source,
            cached: Mutex::new(initial),
        })
    }

    /// The configured source.
    #[must_use]
    pub fn source(&self) -> &TokenSource {
        &self.source
    }

    fn cached(&self) -> Option<Arc<str>> {
        self.cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn remember(&self, token: Arc<str>) {
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = Some(token);
    }

    /// The token to send: the cached one, or, while none is cached yet (a
    /// `token_file` not read so far), the one just read from the source.
    ///
    /// # Errors
    /// [`TokenError`] when the source cannot be read.
    pub async fn current(&self) -> Result<Arc<str>, TokenError> {
        if let Some(token) = self.cached() {
            return Ok(token);
        }
        let token = self.source.read().await?;
        self.remember(Arc::clone(&token));
        Ok(token)
    }

    /// Called after Vault rejected `rejected` with `403`: re-reads the source
    /// once and, if it now yields a different token (a sidecar rotated the
    /// file), caches it and returns it for one retry of the request.
    ///
    /// Returns `None` when there is nothing new to try: an inline token
    /// cannot change, the source still holds the rejected token, or it could
    /// not be read (logged, without the token).
    pub async fn refreshed_after_rejection(&self, rejected: &str) -> Option<Arc<str>> {
        if matches!(self.source, TokenSource::Inline(_)) {
            return None;
        }
        match self.source.read().await {
            Ok(fresh) => {
                self.remember(Arc::clone(&fresh));
                if fresh.as_ref() == rejected {
                    None
                } else {
                    info!(
                        source = self.source.kind(),
                        "vault credstore plugin: token source holds a new token after a 403; \
                         retrying the request once"
                    );
                    Some(fresh)
                }
            }
            Err(error) => {
                warn!(
                    source = self.source.kind(),
                    %error,
                    "vault credstore plugin: token source could not be re-read after a 403"
                );
                None
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "token_tests.rs"]
mod token_tests;
