use std::num::{NonZeroU64, NonZeroUsize};

use serde::Deserialize;
use toolkit_utils::SecretString;
use toolkit_utils::var_expand::ExpandVarsError;

use crate::domain::scope::ScopeConfig;
use crate::infra::github::compression::Compression;

#[derive(Debug, Clone, Deserialize)]
pub struct GithubMirrorConfig {
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    /// Temporary shortcut until credstore integration (gears-rust#4534):
    /// GitHub token used by sync. Unauthenticated requests work for public
    /// repositories at a much lower rate limit.
    ///
    /// Supports `${VAR}` and `${VAR:-default}` so the token can live in the
    /// environment instead of a checked-in config file; call
    /// [`GithubMirrorConfig::resolved_token`] rather than reading the field.
    /// `SecretString` keeps the value out of every `Debug`/`Display` of the
    /// config - it prints as `[REDACTED]` - so a startup config dump cannot
    /// leak a literally-configured PAT.
    #[serde(default)]
    pub github_token: Option<SecretString>,
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

impl GithubMirrorConfig {
    /// The token with any `${VAR}` reference expanded from the environment.
    ///
    /// # Errors
    /// Returns the expansion error when the config names a variable that is
    /// not set and gives no default, so a typo fails loudly at startup
    /// instead of silently syncing unauthenticated.
    pub fn resolved_token(&self) -> Result<Option<String>, ExpandVarsError> {
        self.github_token
            .as_ref()
            .map(|token| toolkit_utils::var_expand::expand_env_vars(token.expose()))
            .transpose()
            .map(|token| token.filter(|t| !t.is_empty()))
    }

    /// The configured GitHub API base URL, validated.
    ///
    /// Everything the gear fetches is built on this value, and it is echoed
    /// by the health endpoint, so a malformed or non-HTTP value should stop
    /// the gear at startup instead of producing garbage requests later.
    ///
    /// # Errors
    /// `Validation` when the value does not parse as a URL or its scheme is
    /// not `http`/`https`.
    pub fn resolved_api_base_url(&self) -> Result<String, crate::domain::error::DomainError> {
        let parsed = url::Url::parse(&self.api_base_url).map_err(|e| {
            crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: format!("`{}` is not a valid URL: {e}", self.api_base_url),
            }
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: format!(
                    "`{}` must use http or https, not `{}`",
                    self.api_base_url,
                    parsed.scheme()
                ),
            });
        }
        // A token would travel in cleartext over plain `http`. Loopback is
        // allowed so a local mock server still works without TLS.
        if parsed.scheme() == "http" && self.github_token.is_some() && !is_loopback(&parsed) {
            return Err(crate::domain::error::DomainError::Validation {
                field: "api_base_url".to_owned(),
                message: format!(
                    concat!(
                        "`{}` uses http, which would send the GitHub token in cleartext; ",
                        "use https (or a loopback host for local testing)"
                    ),
                    self.api_base_url
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
            github_token: None,
            scope: ScopeConfig::default(),
            cache_compression: Compression::default(),
            max_concurrent_syncs: default_max_concurrent_syncs(),
            max_concurrent_tasks: default_max_concurrent_tasks(),
            max_concurrent_requests: default_max_concurrent_requests(),
            sync_deadline_minutes: default_sync_deadline_minutes(),
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
    fn a_literal_token_is_returned_as_is() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"github_token":"ghp_literal"}"#).expect("config must parse");
        assert_eq!(
            cfg.resolved_token().unwrap().as_deref(),
            Some("ghp_literal")
        );
    }

    #[test]
    fn no_token_stays_none() {
        let cfg = GithubMirrorConfig::default();
        assert_eq!(cfg.resolved_token().unwrap(), None);
    }

    #[test]
    fn a_variable_reference_falls_back_to_its_default() {
        let cfg: GithubMirrorConfig = serde_json::from_str(
            r#"{"github_token":"${GH_MIRROR_UNSET_TOKEN:-ghp_from_default}"}"#,
        )
        .expect("config must parse");
        assert_eq!(
            cfg.resolved_token().unwrap().as_deref(),
            Some("ghp_from_default"),
            "the config must read the variable, not the literal text"
        );
    }

    #[test]
    fn an_unset_variable_with_an_empty_default_means_no_token() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"github_token":"${GH_MIRROR_UNSET_TOKEN:-}"}"#)
                .expect("config must parse");
        assert_eq!(
            cfg.resolved_token().unwrap(),
            None,
            "an empty expansion must sync unauthenticated, not send an empty header"
        );
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

    #[test]
    fn an_unset_variable_without_a_default_fails_loudly() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"github_token":"${GH_MIRROR_MISSING_TOKEN}"}"#)
                .expect("config must parse");
        assert!(
            cfg.resolved_token().is_err(),
            "a typo in the variable name must not silently drop the token"
        );
    }
}
