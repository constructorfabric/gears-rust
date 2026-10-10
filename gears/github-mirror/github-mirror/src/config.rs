use std::collections::HashMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use serde::Deserialize;
use uuid::Uuid;

use crate::domain::scope::ScopeConfig;
use crate::infra::github::compression::Compression;

#[derive(Debug, Clone, Deserialize)]
pub struct GithubMirrorConfig {
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    /// Where the GitHub token lives in the credential store; it is read once
    /// at init. Without it the gear syncs unauthenticated, which works for
    /// public repositories at a much lower rate limit.
    #[serde(default)]
    pub github_token_secret: Option<GithubTokenSecret>,
    /// What a sync collects when the request does not say (PRD §5.4): the
    /// type's default, which leaves timeline events off until a deployment or
    /// a request turns them on (PRD §5.2).
    #[serde(default)]
    pub scope: ScopeConfig,
    /// How cached response bodies are stored: `none` or `gzip` (PRD §5.6; the
    /// design's `zstd` is not built into this gear, so it is refused here).
    /// GitHub JSON gzips to roughly a fifth of its size, so the default is on.
    #[serde(default)]
    pub cache_compression: Compression,
    /// How many repositories the gear syncs at the same time
    /// (PRD §6.1 "parallel synchronization of multiple repositories").
    ///
    /// Zero is a config error: a queue with no worker would accept syncs and
    /// never run them.
    #[serde(default = "default_max_concurrent_syncs")]
    pub max_concurrent_syncs: NonZeroUsize,
    /// How many tasks one repository's sync runs at the same time: list
    /// pages being indexed and entities being refined. The reference
    /// implementation's `--max-concurrent`.
    #[serde(default = "default_max_concurrent_tasks")]
    pub max_concurrent_tasks: NonZeroUsize,
    /// Ceiling on GitHub requests in flight across every running sync.
    ///
    /// GitHub's secondary rate limit triggers on concurrency rather than
    /// volume, and the PRD's threshold is "zero bans with parallelism <= 8"
    /// (PRD §6.1), so the default sits at that bound. Without this ceiling
    /// each extra concurrent repository would multiply the request rate.
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: NonZeroUsize,
    /// How long one repository's sync may run before the gear stops it
    /// (PRD §5.2 `fr-sync-deadline`).
    ///
    /// A stopped run is not lost work: its watermarks and fingerprints are
    /// durable, the repository stays `in_progress`, and the next sync or
    /// resume carries on from there. The default is wide enough for a first
    /// sync of a large repository, which spends most of its time waiting out
    /// GitHub's hourly rate limit rather than working.
    #[serde(default = "default_sync_deadline_minutes")]
    pub sync_deadline_minutes: NonZeroU64,
    #[serde(default)]
    pub telemetry_dir: Option<PathBuf>,
    #[serde(default)]
    pub cache_max_age_days: Option<NonZeroU64>,
    #[serde(default)]
    pub tenants: HashMap<Uuid, TenantConfig>,
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantConfig {
    #[serde(default)]
    pub cache_max_age_days: Option<NonZeroU64>,
}

/// Repositories synced at once when the config says nothing: enough to keep
/// the queue moving, low enough that one tenant's backlog is not the whole
/// gear's work.
fn default_max_concurrent_syncs() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN)
}

/// GitHub requests in flight at once when the config says nothing.
fn default_max_concurrent_requests() -> NonZeroUsize {
    NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN)
}

/// Tasks in flight inside one sync when the config says nothing.
fn default_max_concurrent_tasks() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN)
}

/// Six hours: a first sync of a repository the size of `rust-lang/rust` is
/// dominated by rate-limit waits, so the bound is there to end a run that is
/// stuck rather than to cut a healthy one short.
fn default_sync_deadline_minutes() -> NonZeroU64 {
    NonZeroU64::new(360).unwrap_or(NonZeroU64::MIN)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubTokenSecret {
    pub tenant_id: Uuid,
    pub key: String,
}

impl GithubMirrorConfig {
    /// The configured GitHub API base URL, validated.
    ///
    /// Everything the gear fetches is built on this value, and it is echoed
    /// by the health endpoint, so a malformed or non-HTTP value should stop
    /// the gear at startup instead of producing garbage requests later.
    ///
    /// # Errors
    /// `Validation` when the value does not parse as a URL, carries a user
    /// name or a password, its scheme is not `http`/`https`, or it carries a
    /// query or a fragment.
    pub fn resolved_api_base_url(&self) -> Result<String, crate::domain::error::DomainError> {
        let parsed = url::Url::parse(&self.api_base_url).map_err(|e| {
            crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: format!(
                    "`{}` is not a valid URL: {e}",
                    crate::redact::redacted_word(&self.api_base_url)
                ),
            }
        })?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: "must not carry a user name or a password".to_owned(),
            });
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message:
                    "must not carry a query or a fragment: every request path is appended to it"
                        .to_owned(),
            });
        }
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: format!(
                    "`{}` must use http or https, not `{}`",
                    crate::redact::redacted_word(&self.api_base_url),
                    parsed.scheme()
                ),
            });
        }
        // A token would travel in cleartext over plain `http`. Loopback is
        // allowed so a local mock server still works without TLS.
        if parsed.scheme() == "http" && self.github_token_secret.is_some() && !is_loopback(&parsed)
        {
            return Err(crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: format!(
                    concat!(
                        "`{}` uses http, which would send the GitHub token in cleartext; ",
                        "use https (or a loopback host for local testing)"
                    ),
                    crate::redact::redacted_word(&self.api_base_url)
                ),
            });
        }
        Ok(self.api_base_url.clone())
    }
}

/// Whether the URL points at this machine, where plain `http` carries no
/// token over a network.
fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

impl Default for GithubMirrorConfig {
    fn default() -> Self {
        Self {
            api_base_url: default_api_base_url(),
            github_token_secret: None,
            scope: ScopeConfig::default(),
            cache_compression: Compression::default(),
            max_concurrent_syncs: default_max_concurrent_syncs(),
            max_concurrent_tasks: default_max_concurrent_tasks(),
            max_concurrent_requests: default_max_concurrent_requests(),
            sync_deadline_minutes: default_sync_deadline_minutes(),
            telemetry_dir: None,
            cache_max_age_days: None,
            tenants: HashMap::new(),
            cache_dir: None,
        }
    }
}

fn default_api_base_url() -> String {
    "https://api.github.com".to_owned()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn default_points_at_public_github_api() {
        let cfg = GithubMirrorConfig::default();
        assert_eq!(cfg.api_base_url, "https://api.github.com");
    }

    #[test]
    fn deserializes_with_missing_field_using_default() {
        let cfg: GithubMirrorConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.api_base_url, "https://api.github.com");
    }

    #[test]
    fn deserializes_explicit_base_url() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"api_base_url":"https://ghe.local/api/v3"}"#).unwrap();
        assert_eq!(cfg.api_base_url, "https://ghe.local/api/v3");
    }

    #[test]
    fn a_valid_base_url_passes_validation() {
        let cfg = GithubMirrorConfig::default();
        assert_eq!(
            cfg.resolved_api_base_url().unwrap(),
            "https://api.github.com"
        );
    }

    #[test]
    fn a_base_url_that_is_not_a_url_fails_validation() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"api_base_url":"not a url at all"}"#)
                .expect("config must parse");
        let err = cfg.resolved_api_base_url().unwrap_err();
        assert!(
            matches!(err, crate::domain::error::DomainError::Validation { ref field, .. } if field == "api_base_url"),
            "expected a Validation error on api_base_url, got {err:?}"
        );
    }

    #[test]
    fn a_non_http_base_url_scheme_fails_validation() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"api_base_url":"ftp://api.github.com"}"#)
                .expect("config must parse");
        let err = cfg.resolved_api_base_url().unwrap_err();
        assert!(
            matches!(err, crate::domain::error::DomainError::Validation { ref message, .. } if message.contains("http")),
            "expected the error to name the allowed schemes, got {err:?}"
        );
    }

    #[test]
    fn a_wrongly_typed_config_value_fails_to_parse() {
        assert!(
            serde_json::from_str::<GithubMirrorConfig>(r#"{"api_base_url":42}"#).is_err(),
            "a non-string api_base_url must be rejected at parse time"
        );
    }
}
