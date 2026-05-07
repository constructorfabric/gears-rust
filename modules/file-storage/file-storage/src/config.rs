//! Static TOML configuration for the FileStorage module (P1).
//!
//! Mirrors the shape declared in DESIGN §3.5. The roster is loaded once
//! at boot — runtime backend registration is P2.

use file_storage_sdk::{CapabilityTag, KNOWN_CAPABILITIES};
use serde::Deserialize;
use uuid::Uuid;

/// Top-level FileStorage config block.
#[derive(Debug, Clone, Deserialize)]
pub struct FileStorageConfig {
    /// Optional UUID of the backend that serves new public files.
    #[serde(default)]
    pub default_public_storage_id: Option<Uuid>,

    /// UUID of the default-private backend used when the SDK caller passes
    /// `backend_id = None` to `create_presigned_upload`.
    #[serde(default)]
    pub default_private_storage_id: Option<Uuid>,

    /// Orphan-delete grace period (seconds).
    #[serde(default = "default_orphan_grace")]
    pub orphan_delete_grace_seconds: u64,

    /// Safety margin added to the orphan-delete grace period.
    #[serde(default = "default_clock_skew_margin")]
    pub signed_url_clock_skew_margin_seconds: u64,

    /// Roster of statically configured backends.
    #[serde(default)]
    pub backends: Vec<BackendConfig>,
}

impl Default for FileStorageConfig {
    fn default() -> Self {
        Self {
            default_public_storage_id: None,
            default_private_storage_id: None,
            orphan_delete_grace_seconds: default_orphan_grace(),
            signed_url_clock_skew_margin_seconds: default_clock_skew_margin(),
            backends: Vec::new(),
        }
    }
}

/// One row of the backend roster.
#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    /// Stable backend identity, assigned once by the deployer.
    pub id: Uuid,

    /// Backend kind. Only `"s3-compatible"` is accepted in P1.
    pub kind: BackendKindCfg,

    #[serde(default)]
    pub default_public: bool,

    #[serde(default)]
    pub default_private: bool,

    /// S3-compatible endpoint URL.
    pub endpoint: String,

    pub region: String,
    pub bucket: String,

    pub access_key: String,
    pub secret_key: String,

    /// Optional per-backend size cap. `None` falls back to the S3 5 TiB
    /// hard cap.
    #[serde(default)]
    pub max_file_size_bytes: Option<u64>,

    /// Optional per-backend metadata budget. `None` falls back to the S3
    /// 2 KiB user-metadata limit.
    #[serde(default)]
    pub max_metadata_bytes: Option<u64>,

    /// Maximum signed-URL TTL the backend will sign (seconds). Falls back
    /// to the AWS SigV4 7-day cap when `None`.
    #[serde(default = "default_max_signed_url_ttl")]
    pub max_signed_url_ttl_seconds: u64,

    /// Versioned capability tags declared by this backend. Validated
    /// against `KNOWN_CAPABILITIES` at module init.
    #[serde(default)]
    pub capabilities: Vec<CapabilityTag>,

    /// Per-backend tenant access list. Empty = visible to every tenant.
    #[serde(default)]
    pub tenant_access: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum BackendKindCfg {
    /// The only kind accepted by P1 config validation.
    #[serde(rename = "s3-compatible")]
    S3Compatible,
}

fn default_orphan_grace() -> u64 {
    86_400
}

fn default_clock_skew_margin() -> u64 {
    60
}

fn default_max_signed_url_ttl() -> u64 {
    86_400
}

/// Validate a fully loaded config at boot.
pub fn validate_config(cfg: &FileStorageConfig) -> Result<(), String> {
    let mut public_default_count = 0usize;
    let mut private_default_count = 0usize;
    let mut max_signed_url_ttl = 0u64;

    for backend in &cfg.backends {
        if backend.default_public {
            public_default_count += 1;
        }
        if backend.default_private {
            private_default_count += 1;
        }
        if backend.max_signed_url_ttl_seconds > max_signed_url_ttl {
            max_signed_url_ttl = backend.max_signed_url_ttl_seconds;
        }
        for cap in &backend.capabilities {
            if !KNOWN_CAPABILITIES.iter().any(|k| k == cap) {
                return Err(format!(
                    "unknown capability \"{cap}\" on backend {id}",
                    id = backend.id
                ));
            }
        }
        // Boot invariant: declaring `download.s3.public.versioned.v1` /
        // `download.s3.sigv4.versioned.v1` is the operator's confirmation
        // that bucket versioning is on (and, for the public-versioned
        // variant, that the bucket policy grants `s3:GetObjectVersion` to
        // anonymous). FileStorage trusts the declaration.
    }

    if public_default_count > 1 {
        return Err("at most one backend may set default_public = true".to_owned());
    }
    if private_default_count > 1 {
        return Err(
            "exactly one backend per tenant view must set default_private (P1: globally one)"
                .to_owned(),
        );
    }
    if !cfg.backends.is_empty() && private_default_count == 0 {
        return Err(
            "at least one backend must set default_private = true when the roster is non-empty"
                .to_owned(),
        );
    }

    let min_grace = max_signed_url_ttl.saturating_add(cfg.signed_url_clock_skew_margin_seconds);
    if cfg.orphan_delete_grace_seconds < min_grace {
        return Err(format!(
            "orphan_delete_grace_seconds ({}) must be >= max_signed_url_ttl_seconds ({}) + clock_skew ({})",
            cfg.orphan_delete_grace_seconds,
            max_signed_url_ttl,
            cfg.signed_url_clock_skew_margin_seconds,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3cfg(default_private: bool) -> BackendConfig {
        BackendConfig {
            id: Uuid::new_v4(),
            kind: BackendKindCfg::S3Compatible,
            default_public: false,
            default_private,
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "bkt".into(),
            access_key: "ak".into(),
            secret_key: "sk".into(),
            max_file_size_bytes: None,
            max_metadata_bytes: None,
            max_signed_url_ttl_seconds: 3600,
            capabilities: vec!["upload.s3.multipart.sigv4.v1".into()],
            tenant_access: vec![],
        }
    }

    #[test]
    fn empty_config_is_valid() {
        let cfg = FileStorageConfig::default();
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn config_with_one_default_private_is_valid() {
        let cfg = FileStorageConfig {
            backends: vec![s3cfg(true)],
            ..Default::default()
        };
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn missing_default_private_with_nonempty_roster_fails() {
        let cfg = FileStorageConfig {
            backends: vec![s3cfg(false)],
            ..Default::default()
        };
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn two_default_private_fails() {
        let cfg = FileStorageConfig {
            backends: vec![s3cfg(true), s3cfg(true)],
            ..Default::default()
        };
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn two_default_public_fails() {
        let mut a = s3cfg(true);
        a.default_public = true;
        let mut b = s3cfg(false);
        b.default_public = true;
        let cfg = FileStorageConfig {
            backends: vec![a, b],
            ..Default::default()
        };
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn unknown_capability_tag_fails() {
        let mut b = s3cfg(true);
        b.capabilities.push("not.a.real.tag.v1".into());
        let cfg = FileStorageConfig {
            backends: vec![b],
            ..Default::default()
        };
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("unknown capability"), "got: {err}");
    }

    #[test]
    fn all_p1_capability_tags_are_accepted() {
        let mut b = s3cfg(true);
        b.capabilities = KNOWN_CAPABILITIES.iter().map(|s| s.to_string()).collect();
        let cfg = FileStorageConfig {
            backends: vec![b],
            ..Default::default()
        };
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn ttl_grace_invariant_enforced() {
        let mut b = s3cfg(true);
        b.max_signed_url_ttl_seconds = 1_000_000; // huge
        let cfg = FileStorageConfig {
            backends: vec![b],
            orphan_delete_grace_seconds: 1, // tiny
            signed_url_clock_skew_margin_seconds: 60,
            ..Default::default()
        };
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("orphan_delete_grace_seconds"), "got: {err}");
    }
}
