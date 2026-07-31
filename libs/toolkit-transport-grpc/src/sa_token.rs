//! Kubernetes projected `ServiceAccount` token reader with rotation support.
//!
//! In Profile 3 a gear presents a projected `ServiceAccount` JWT as its
//! platform-plane credential on outbound system calls (`InternalCredential::`
//! `KubeServiceAccountToken`). The kubelet **rotates** that token in place
//! (rewriting the projected-volume file) well before expiry, so a long-lived
//! process must re-read the file rather than caching the first value forever.
//!
//! [`ServiceAccountTokenReader`] reads the token once at construction and then
//! refreshes it on a fixed interval (TTL re-read) from a background task. It
//! exposes a [`token_provider`](ServiceAccountTokenReader::token_provider)
//! closure suitable for
//! [`InternalAuthInterceptor::new`](crate::internal_auth::InternalAuthInterceptor::new),
//! which is invoked on every outbound request and therefore always attaches the
//! *current* (post-rotation) token.
//!
//! The default refresh cadence is deliberately short relative to a
//! `ServiceAccount` token's lifetime; a projected token is typically valid for
//! an hour, so a re-read every [`DEFAULT_REFRESH_INTERVAL`] keeps the cached
//! value fresh with negligible cost.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::RwLock;
use secrecy::SecretString;

use crate::internal_auth::InternalAuthInterceptor;

/// Default TTL re-read cadence for the projected token file.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_mins(1);

/// Shared, atomically-updatable current token value.
type SharedToken = Arc<RwLock<Arc<str>>>;

/// Reads a projected `ServiceAccount` token file and keeps a refreshed copy in
/// memory so callers always observe the current (post-rotation) value.
///
/// Dropping the reader stops the background refresh task **once no
/// [`token_provider`](Self::token_provider) closures remain**: the task holds a
/// [`Weak`] handle to the shared state and exits when the last strong reference
/// (the reader and any live providers) is gone.
#[derive(Debug)]
pub struct ServiceAccountTokenReader {
    path: PathBuf,
    current: SharedToken,
}

impl ServiceAccountTokenReader {
    /// Read `path` once and spawn a background task that re-reads it every
    /// [`DEFAULT_REFRESH_INTERVAL`].
    ///
    /// # Errors
    /// Returns an error if the token file cannot be read or is empty on the
    /// initial read.
    pub async fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_refresh_interval(path, DEFAULT_REFRESH_INTERVAL).await
    }

    /// Like [`new`](Self::new) but with a custom re-read `interval` (primarily
    /// for tests).
    ///
    /// # Errors
    /// Returns an error if the token file cannot be read or is empty on the
    /// initial read.
    pub async fn with_refresh_interval(
        path: impl Into<PathBuf>,
        interval: Duration,
    ) -> std::io::Result<Self> {
        let path = path.into();
        let token = read_token(&path).await?;
        let current: SharedToken = Arc::new(RwLock::new(token));

        spawn_refresh_task(path.clone(), Arc::downgrade(&current), interval);

        Ok(Self { path, current })
    }

    /// The projected-token path this reader watches.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Snapshot the current token value.
    #[must_use]
    pub fn current(&self) -> SecretString {
        SecretString::from(self.current.read().to_string())
    }

    /// Build a token-provider closure that yields the *current* token on each
    /// call, for [`InternalAuthInterceptor::new`].
    ///
    /// The returned closure holds a strong reference to the shared token, so
    /// the background refresh task keeps running for as long as any provider
    /// (or interceptor built from one) is alive.
    pub fn token_provider(&self) -> impl Fn() -> Option<SecretString> + Send + Sync + 'static {
        let current = Arc::clone(&self.current);
        move || {
            let value = current.read().to_string();
            if value.is_empty() {
                None
            } else {
                Some(SecretString::from(value))
            }
        }
    }

    /// Convenience: build an [`InternalAuthInterceptor`] that attaches the
    /// current (rotating) token to every outgoing request.
    #[must_use]
    pub fn interceptor(&self) -> InternalAuthInterceptor {
        InternalAuthInterceptor::new(self.token_provider())
    }
}

/// Spawn the TTL re-read loop. Terminates when `current` can no longer be
/// upgraded (the reader and all providers have been dropped).
fn spawn_refresh_task(path: PathBuf, current: Weak<RwLock<Arc<str>>>, interval: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let Some(current) = current.upgrade() else {
                break;
            };
            match read_token(&path).await {
                Ok(token) => {
                    *current.write() = token;
                    tracing::debug!(path = %path.display(), "refreshed service-account token");
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to re-read service-account token; keeping previous value",
                    );
                }
            }
        }
    });
}

/// Read and trim a token file, rejecting an empty result.
async fn read_token(path: &Path) -> std::io::Result<Arc<str>> {
    let raw = tokio::fs::read_to_string(path).await?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("service-account token file is empty: {}", path.display()),
        ));
    }
    Ok(Arc::from(trimmed))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique temp path per test, without pulling in a UUID dependency.
    fn unique_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("sa-{tag}-{}-{n}", std::process::id()))
    }

    async fn write(path: &Path, contents: &str) {
        tokio::fs::write(path, contents).await.expect("write token");
    }

    #[tokio::test]
    async fn reads_initial_token_trimmed() {
        let dir = unique_dir("token");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("token");
        write(&path, "  header.payload.sig\n").await;

        let reader = ServiceAccountTokenReader::new(&path).await.unwrap();
        assert_eq!(reader.current().expose_secret(), "header.payload.sig");

        let provider = reader.token_provider();
        assert_eq!(provider().unwrap().expose_secret(), "header.payload.sig");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn missing_file_is_an_error() {
        let path = unique_dir("missing").join("token");
        assert!(ServiceAccountTokenReader::new(&path).await.is_err());
    }

    #[tokio::test]
    async fn empty_file_is_an_error() {
        let dir = unique_dir("empty");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("token");
        write(&path, "   \n").await;
        assert!(ServiceAccountTokenReader::new(&path).await.is_err());
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn refreshes_on_rotation() {
        let dir = unique_dir("rotate");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("token");
        write(&path, "token-v1").await;

        let reader =
            ServiceAccountTokenReader::with_refresh_interval(&path, Duration::from_millis(20))
                .await
                .unwrap();
        let provider = reader.token_provider();
        assert_eq!(provider().unwrap().expose_secret(), "token-v1");

        // Simulate the kubelet rotating the projected token in place.
        write(&path, "token-v2").await;

        // Wait for at least one refresh cycle.
        let mut observed = String::new();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            observed = provider().unwrap().expose_secret().to_owned();
            if observed == "token-v2" {
                break;
            }
        }
        assert_eq!(
            observed, "token-v2",
            "provider should observe rotated token"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
