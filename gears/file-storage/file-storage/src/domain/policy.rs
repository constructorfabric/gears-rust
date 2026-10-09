//! Policy domain types and the `PolicyResolver`.
//!
//! The resolver computes the effective policy as the most-restrictive combination of
//! tenant and user levels per aspect. Upload enforcement lives in `domain/service/create.rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Identifies whether a policy row applies to the whole tenant or a single user.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyScope {
    Tenant,
    User,
}

impl PolicyScope {
    /// Wire/DB spelling (`"tenant"` / `"user"`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::User => "user",
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// Identifies whether a retention rule applies to the tenant, a user, or a file.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionScope {
    Tenant,
    User,
    File,
}

impl RetentionScope {
    /// Wire/DB spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::User => "user",
            Self::File => "file",
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "user" => Some(Self::User),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// Per-mime-type size limit override.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MimeSizeOverride {
    /// Mime type pattern (e.g. `"video/*"` or `"image/jpeg"`).
    pub mime: String,
    /// Maximum file size in bytes for this mime pattern.
    pub max_bytes: u64,
}

/// Size limits portion of a policy body: a global maximum and optional per-mime overrides.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SizeLimits {
    /// Global maximum file size in bytes (`None` = unlimited at this level).
    pub max_bytes: Option<u64>,
    /// Per-mime overrides; the most specific matching entry is used.
    #[serde(default)]
    pub per_mime: Vec<MimeSizeOverride>,
}

/// Metadata limits portion of a policy body.
#[allow(clippy::struct_field_names)]
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MetadataLimits {
    /// Maximum number of key-value pairs (`None` = unlimited at this level).
    pub max_pairs: Option<u32>,
    /// Maximum length of a single key in bytes (`None` = unlimited).
    pub max_key_len: Option<u32>,
    /// Maximum length of a single value in bytes (`None` = unlimited).
    pub max_value_len: Option<u32>,
    /// Maximum total byte size (sum of all keys + values) (`None` = unlimited).
    pub max_total_bytes: Option<u32>,
}

/// The JSON body stored in the `policies.body` column.
///
/// Allowed mime types, size limits, metadata limits and enabled event types for one scope.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PolicyBody {
    /// Allowed MIME types for upload. An empty list means "all types allowed".
    /// Entries may use `*` wildcard for the subtype (e.g. `"image/*"`).
    #[serde(default)]
    pub allowed_mime_types: Vec<String>,

    /// Size limits (global and per-mime overrides).
    #[serde(default)]
    pub size_limits: SizeLimits,

    /// Metadata limits (max pairs, max key/value lengths, max total size).
    #[serde(default)]
    pub metadata_limits: MetadataLimits,

    /// Enabled event types; an empty list means none at this level.
    #[serde(default)]
    pub enabled_event_types: Vec<String>,
}

/// Criteria for age-based retention (delete files older than `max_age_days`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgeRetention {
    /// Delete files that were created more than this many days ago.
    pub max_age_days: u32,
}

/// Criteria for inactivity-based retention (delete files not accessed for N days).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InactivityRetention {
    /// Delete files not **modified** in this many days (`last_modified_at`, bumped only by
    /// writes: bind/patch/transfer). Downloads do not reset this clock.
    pub inactivity_days: u32,
}

/// Criteria for metadata-based retention (delete when a metadata key/value
/// matches a condition).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MetadataRetention {
    /// The metadata key to inspect.
    pub key: String,
    /// The expected metadata value; deletion fires when the key equals this.
    pub value: String,
}

/// The JSON body stored in the `retention_rules.body` column.
///
/// A rule may specify several criteria; any matching criterion triggers expiry (OR).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RetentionRuleBody {
    /// Age-based expiry criterion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<AgeRetention>,

    /// Inactivity-based expiry criterion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactivity: Option<InactivityRetention>,

    /// Metadata-based expiry criterion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataRetention>,
}

/// A stored policy row, as returned by the `PolicyRepo`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPolicy {
    pub policy_id: Uuid,
    pub tenant_id: Uuid,
    pub scope: PolicyScope,
    /// `None` for `scope = Tenant`; the user's `owner_id` for `scope = User`.
    pub scope_owner_id: Option<Uuid>,
    pub body: PolicyBody,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A stored retention rule row.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRetentionRule {
    pub rule_id: Uuid,
    pub tenant_id: Uuid,
    pub scope: RetentionScope,
    /// `None` for tenant scope; `user_id` for user scope; `file_id` for file scope.
    pub scope_target_id: Option<Uuid>,
    pub body: RetentionRuleBody,
    pub created_at: OffsetDateTime,
}

/// The fully resolved effective policy for a request context, computed by
/// [`PolicyResolver`] as the most-restrictive combination of tenant + user levels.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    /// Intersection of allowed mime types. `None` = all allowed; empty `Vec` = none allowed.
    pub allowed_mime_types: Option<Vec<String>>,

    /// Most-restrictive global size limit in bytes. `None` = unlimited.
    pub max_bytes: Option<u64>,

    /// Per-mime size overrides merged across levels (most restrictive per pattern).
    pub per_mime_max_bytes: Vec<MimeSizeOverride>,

    /// Most-restrictive metadata limits (smallest non-None value per field).
    pub metadata_limits: MetadataLimits,
}

/// Computes the effective policy for a file request context from a tenant-level
/// policy and an optional user-level policy.
///
/// Resolution rule: **most-restrictive wins per aspect**:
/// - `allowed_mime_types`: intersection; if one level is unrestricted, the other
///   level's restriction stands.
/// - `max_bytes` (global): `min(tenant.max_bytes, user.max_bytes)`.
/// - `per_mime` overrides: each mime pattern takes the smallest `max_bytes` across
///   levels (union of patterns, most restrictive value).
/// - metadata limits: smallest non-None value from each limit field.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct PolicyResolver;

impl PolicyResolver {
    /// Compute the effective policy; a `None` level contributes no restrictions.
    #[must_use]
    pub fn resolve(
        tenant_policy: Option<&PolicyBody>,
        user_policy: Option<&PolicyBody>,
    ) -> EffectivePolicy {
        // An empty `allowed_mime_types` means "no restriction at this level", not "nothing".
        let allowed_mime_types = Self::merge_allowed_mimes(
            tenant_policy.map(|p| &p.allowed_mime_types),
            user_policy.map(|p| &p.allowed_mime_types),
        );

        let tenant_max = tenant_policy.and_then(|p| p.size_limits.max_bytes);
        let user_max = user_policy.and_then(|p| p.size_limits.max_bytes);
        let max_bytes = Self::min_option(tenant_max, user_max);

        let empty: &[MimeSizeOverride] = &[];
        let tenant_per_mime = tenant_policy.map_or(empty, |p| p.size_limits.per_mime.as_slice());
        let user_per_mime = user_policy.map_or(empty, |p| p.size_limits.per_mime.as_slice());
        let per_mime_max_bytes = Self::merge_per_mime(tenant_per_mime, user_per_mime);

        let t_meta = tenant_policy.map(|p| &p.metadata_limits);
        let u_meta = user_policy.map(|p| &p.metadata_limits);
        let metadata_limits = Self::merge_metadata_limits(t_meta, u_meta);

        EffectivePolicy {
            allowed_mime_types,
            max_bytes,
            per_mime_max_bytes,
            metadata_limits,
        }
    }

    /// Intersection of allowed mime types; an unrestricted level (empty or `None`) defers
    /// to the other.
    fn merge_allowed_mimes(
        tenant: Option<&Vec<String>>,
        user: Option<&Vec<String>>,
    ) -> Option<Vec<String>> {
        let t_restricted = tenant.filter(|v| !v.is_empty());
        let u_restricted = user.filter(|v| !v.is_empty());

        match (t_restricted, u_restricted) {
            (None, None) => None,
            (Some(t), None) => Some(t.clone()),
            (None, Some(u)) => Some(u.clone()),
            (Some(t), Some(u)) => {
                // Narrower pattern wins: `image/*` ∩ `image/png` is `image/png`, not
                // `image/*` (which would still admit `image/jpeg`).
                let mut intersection: Vec<String> = Vec::new();
                for t_mt in t {
                    for u_mt in u {
                        if let Some(narrow) = Self::intersect_mime(t_mt, u_mt)
                            && !intersection.contains(&narrow)
                        {
                            intersection.push(narrow);
                        }
                    }
                }
                Some(intersection)
            }
        }
    }

    /// Narrower of two overlapping mime patterns, or `None` when disjoint.
    fn intersect_mime(a: &str, b: &str) -> Option<String> {
        if a == b {
            return Some(a.to_owned());
        }
        let (a_type, a_sub) = Self::split_mime(a);
        let (b_type, b_sub) = Self::split_mime(b);
        if a_type != b_type {
            return None;
        }
        match (a_sub, b_sub) {
            ("*", "*") => Some(a.to_owned()),
            ("*", _) => Some(b.to_owned()),
            (_, "*") => Some(a.to_owned()),
            _ => None,
        }
    }

    fn split_mime(mime: &str) -> (&str, &str) {
        let mut parts = mime.splitn(2, '/');
        let base = parts.next().unwrap_or(mime);
        let sub = parts.next().unwrap_or("*");
        (base, sub)
    }

    /// Smallest of two limits (`None` = unlimited).
    fn min_option(a: Option<u64>, b: Option<u64>) -> Option<u64> {
        match (a, b) {
            (None, None) => None,
            (Some(v), None) | (None, Some(v)) => Some(v),
            (Some(x), Some(y)) => Some(x.min(y)),
        }
    }

    /// Union of patterns with the smallest value per pattern, then tightened so a broader
    /// wildcard cap also caps the more-specific entries it covers (consumers pick the
    /// most specific entry, so `image/png = 50MB` would otherwise ignore `image/* = 10MB`).
    fn merge_per_mime(
        tenant: &[MimeSizeOverride],
        user: &[MimeSizeOverride],
    ) -> Vec<MimeSizeOverride> {
        let mut result: Vec<MimeSizeOverride> = tenant.to_vec();

        for u in user {
            if let Some(existing) = result.iter_mut().find(|e| e.mime == u.mime) {
                existing.max_bytes = existing.max_bytes.min(u.max_bytes);
            } else {
                result.push(u.clone());
            }
        }

        let snapshot = result.clone();
        for e in &mut result {
            for o in &snapshot {
                if Self::mime_pattern_covers(&o.mime, &e.mime) {
                    e.max_bytes = e.max_bytes.min(o.max_bytes);
                }
            }
        }

        result
    }

    /// True if every mime matching `other` also matches `pattern` (equal, or `type/*`).
    fn mime_pattern_covers(pattern: &str, other: &str) -> bool {
        if pattern == other {
            return true;
        }
        let (p_type, p_sub) = Self::split_mime(pattern);
        let (o_type, _) = Self::split_mime(other);
        p_type == o_type && p_sub == "*"
    }

    #[allow(clippy::struct_field_names)]
    fn merge_metadata_limits(
        tenant: Option<&MetadataLimits>,
        user: Option<&MetadataLimits>,
    ) -> MetadataLimits {
        MetadataLimits {
            max_pairs: Self::min_option_u32(
                tenant.and_then(|m| m.max_pairs),
                user.and_then(|m| m.max_pairs),
            ),
            max_key_len: Self::min_option_u32(
                tenant.and_then(|m| m.max_key_len),
                user.and_then(|m| m.max_key_len),
            ),
            max_value_len: Self::min_option_u32(
                tenant.and_then(|m| m.max_value_len),
                user.and_then(|m| m.max_value_len),
            ),
            max_total_bytes: Self::min_option_u32(
                tenant.and_then(|m| m.max_total_bytes),
                user.and_then(|m| m.max_total_bytes),
            ),
        }
    }

    fn min_option_u32(a: Option<u32>, b: Option<u32>) -> Option<u32> {
        match (a, b) {
            (None, None) => None,
            (Some(v), None) | (None, Some(v)) => Some(v),
            (Some(x), Some(y)) => Some(x.min(y)),
        }
    }
}

impl PolicyResolver {
    /// True if `mime_type` matches any pattern in `allowed` (exact or `type/*`).
    /// A pattern without a `/` is malformed and never matches.
    #[must_use]
    pub(crate) fn mime_allowed(mime_type: &str, allowed: &[String]) -> bool {
        allowed.iter().any(|pat| {
            if pat == mime_type {
                return true;
            }
            let Some((pt, ps)) = pat.split_once('/') else {
                return false;
            };
            let Some((mt, _)) = mime_type.split_once('/') else {
                return false;
            };
            ps == "*" && pt == mt
        })
    }

    /// Check that `mime_type` is permitted (`None` = all, `Some([])` = nothing).
    pub(crate) fn check_allowed_mime(
        policy: &EffectivePolicy,
        mime_type: &str,
    ) -> Result<(), crate::domain::error::DomainError> {
        let Some(allowed) = &policy.allowed_mime_types else {
            return Ok(());
        };
        if Self::mime_allowed(mime_type, allowed) {
            Ok(())
        } else {
            Err(crate::domain::error::DomainError::policy_mime_not_allowed(
                mime_type,
            ))
        }
    }

    /// Effective maximum blob size: the minimum of the backend ceiling, the policy global
    /// limit and the per-mime override. `None` = unbounded.
    #[must_use]
    pub(crate) fn compute_effective_max_bytes(
        policy: &EffectivePolicy,
        mime_type: &str,
        backend_max: Option<u64>,
    ) -> Option<u64> {
        let policy_global = policy.max_bytes;

        let per_mime_max: Option<u64> = policy
            .per_mime_max_bytes
            .iter()
            .filter(|o| Self::mime_allowed(mime_type, std::slice::from_ref(&o.mime)))
            .map(|o| o.max_bytes)
            .reduce(u64::min);

        [backend_max, policy_global, per_mime_max]
            .into_iter()
            .flatten()
            .reduce(u64::min)
    }

    /// Validate `entries` against the policy's metadata limits.
    pub(crate) fn check_metadata_limits(
        policy: &EffectivePolicy,
        entries: &[(String, String)],
    ) -> Result<(), crate::domain::error::DomainError> {
        let limits = &policy.metadata_limits;

        if let Some(max_pairs) = limits.max_pairs
            && entries.len() > max_pairs as usize
        {
            return Err(crate::domain::error::DomainError::policy_metadata_exceeded(
                format!("too many metadata pairs: {} > {max_pairs}", entries.len()),
            ));
        }

        let mut total_bytes: usize = 0;
        for (key, value) in entries {
            if let Some(max_key_len) = limits.max_key_len
                && key.len() > max_key_len as usize
            {
                return Err(crate::domain::error::DomainError::policy_metadata_exceeded(
                    format!(
                        "metadata key '{key}' length {} exceeds limit of {max_key_len}",
                        key.len()
                    ),
                ));
            }
            if let Some(max_value_len) = limits.max_value_len
                && value.len() > max_value_len as usize
            {
                return Err(crate::domain::error::DomainError::policy_metadata_exceeded(
                    format!(
                        "metadata value for key '{key}' length {} exceeds limit of {max_value_len}",
                        value.len()
                    ),
                ));
            }
            total_bytes += key.len() + value.len();
        }

        if let Some(max_total_bytes) = limits.max_total_bytes
            && total_bytes > max_total_bytes as usize
        {
            return Err(crate::domain::error::DomainError::policy_metadata_exceeded(
                format!(
                    "total metadata size {total_bytes} bytes exceeds limit of {max_total_bytes} bytes"
                ),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod policy_tests;
