//! Policy domain types and the `PolicyResolver`.
//!
//! M1 stores and resolves policy; it does NOT yet enforce it on uploads (that is
//! M2). The resolver computes the **effective policy = most-restrictive across
//! tenant + user** per aspect, as required by the PRD:
//!
//! @cpt-cf-file-storage-fr-allowed-types-policy
//! @cpt-cf-file-storage-fr-size-limits-policy
//! @cpt-cf-file-storage-fr-metadata-limits
//! @cpt-cf-file-storage-fr-retention-policies

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

// ── Policy scope / owner ───────────────────────────────────────────────────────

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

// ── Policy body ───────────────────────────────────────────────────────────────

/// Per-mime-type size limit override.
///
/// Part of `cpt-cf-file-storage-fr-size-limits-policy`:
/// "optional per-mime-type overrides (e.g., 100 MB general, 1 GB for `video/*`)".
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MimeSizeOverride {
    /// Mime type pattern (e.g. `"video/*"` or `"image/jpeg"`).
    pub mime: String,
    /// Maximum file size in bytes for this mime pattern.
    pub max_bytes: u64,
}

/// Size limits portion of a policy body.
///
/// `cpt-cf-file-storage-fr-size-limits-policy`: tenants and users define a
/// global maximum size and optional per-mime-type overrides. The most-restrictive
/// value wins across tenant and user levels.
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
///
/// `cpt-cf-file-storage-fr-metadata-limits`: maximum number of key-value pairs,
/// maximum key length, maximum value length, maximum total metadata size.
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
/// Holds the allowed mime types, size limits, metadata limits, and enabled event
/// types for a single scope (tenant or user).
///
/// @cpt-cf-file-storage-fr-allowed-types-policy
/// @cpt-cf-file-storage-fr-size-limits-policy
/// @cpt-cf-file-storage-fr-metadata-limits
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PolicyBody {
    /// Allowed MIME types for upload. An empty list means "all types allowed".
    /// Entries may use `*` wildcard for the subtype (e.g. `"image/*"`).
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    #[serde(default)]
    pub allowed_mime_types: Vec<String>,

    /// Size limits (global and per-mime overrides).
    ///
    /// @cpt-cf-file-storage-fr-size-limits-policy
    #[serde(default)]
    pub size_limits: SizeLimits,

    /// Metadata limits (max pairs, max key/value lengths, max total size).
    ///
    /// @cpt-cf-file-storage-fr-metadata-limits
    #[serde(default)]
    pub metadata_limits: MetadataLimits,

    /// Enabled event types for the `EventBroker` (M2/M0 will use this).
    /// An empty list means no events are enabled at this level.
    #[serde(default)]
    pub enabled_event_types: Vec<String>,
}

// ── Retention rule body ───────────────────────────────────────────────────────

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
    /// Delete files not accessed (read or written) in this many days.
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
/// A rule may specify one or more criteria; any matching criterion triggers
/// expiry (OR semantics). `cpt-cf-file-storage-fr-retention-policies`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RetentionRuleBody {
    /// Age-based expiry criterion.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<AgeRetention>,

    /// Inactivity-based expiry criterion.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactivity: Option<InactivityRetention>,

    /// Metadata-based expiry criterion.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataRetention>,
}

// ── Policy row (domain view) ──────────────────────────────────────────────────

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

// ── Effective policy (resolved) ───────────────────────────────────────────────

/// The fully resolved effective policy for a request context, computed by
/// [`PolicyResolver`] as the most-restrictive combination of tenant + user levels.
///
/// @cpt-cf-file-storage-fr-allowed-types-policy
/// @cpt-cf-file-storage-fr-size-limits-policy
/// @cpt-cf-file-storage-fr-metadata-limits
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    /// Intersection of allowed mime types from tenant and user policies.
    /// `None` means "all types allowed" (no restriction from any level).
    /// An empty `Vec` means "no types allowed" (total restriction).
    pub allowed_mime_types: Option<Vec<String>>,

    /// Most-restrictive global size limit in bytes. `None` = unlimited.
    pub max_bytes: Option<u64>,

    /// Per-mime size overrides, merged from all levels (most restrictive wins
    /// per mime pattern).
    pub per_mime_max_bytes: Vec<MimeSizeOverride>,

    /// Most-restrictive metadata limits (smallest non-None value from each level).
    pub metadata_limits: MetadataLimits,
}

// ── PolicyResolver ─────────────────────────────────────────────────────────────

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
///
/// @cpt-cf-file-storage-fr-allowed-types-policy
/// @cpt-cf-file-storage-fr-size-limits-policy
/// @cpt-cf-file-storage-fr-metadata-limits
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct PolicyResolver;

impl PolicyResolver {
    /// Compute the effective policy from an optional tenant-level policy body
    /// and an optional user-level policy body.
    ///
    /// Either argument may be `None` (meaning "no policy defined at that level",
    /// which contributes no restrictions). When both are `None`, the returned
    /// `EffectivePolicy` is fully permissive (no restrictions).
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    #[must_use]
    pub fn resolve(
        tenant_policy: Option<&PolicyBody>,
        user_policy: Option<&PolicyBody>,
    ) -> EffectivePolicy {
        // ── Allowed mime types ────────────────────────────────────────────────
        // Most-restrictive-wins: intersection across levels.
        // Empty allowed_mime_types in a PolicyBody means "no restriction at this
        // level" — not "nothing allowed". A level is "restricted" only when its
        // allowed_mime_types is non-empty.
        let allowed_mime_types = Self::merge_allowed_mimes(
            tenant_policy.map(|p| &p.allowed_mime_types),
            user_policy.map(|p| &p.allowed_mime_types),
        );

        // ── Global size limit ─────────────────────────────────────────────────
        // Most-restrictive = smallest non-None value.
        let tenant_max = tenant_policy.and_then(|p| p.size_limits.max_bytes);
        let user_max = user_policy.and_then(|p| p.size_limits.max_bytes);
        let max_bytes = Self::min_option(tenant_max, user_max);

        // ── Per-mime size overrides ───────────────────────────────────────────
        let empty: &[MimeSizeOverride] = &[];
        let tenant_per_mime = tenant_policy.map_or(empty, |p| p.size_limits.per_mime.as_slice());
        let user_per_mime = user_policy.map_or(empty, |p| p.size_limits.per_mime.as_slice());
        let per_mime_max_bytes = Self::merge_per_mime(tenant_per_mime, user_per_mime);

        // ── Metadata limits ───────────────────────────────────────────────────
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

    /// Intersection of allowed mime types.
    ///
    /// - Both unrestricted (empty list or None) => None (no restriction).
    /// - One unrestricted + one restricted => the restricted set wins.
    /// - Both restricted => intersection of the two sets.
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
                // Intersection, resolved to the NARROWER pattern for asymmetric
                // wildcard overlaps: tenant `image/*` ∩ user `image/png` yields
                // `image/png`, not `image/*` (which would wrongly keep admitting
                // `image/jpeg` once the list is enforced as an allow-list).
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

    /// Intersect two mime patterns: return the **narrower** pattern when they
    /// overlap, or `None` when they are disjoint. `image/*` ∩ `image/png` =
    /// `image/png`; `image/png` ∩ `image/jpeg` = `None`.
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
            // Two different concrete subtypes under the same base type: disjoint.
            _ => None,
        }
    }

    fn split_mime(mime: &str) -> (&str, &str) {
        let mut parts = mime.splitn(2, '/');
        let base = parts.next().unwrap_or(mime);
        let sub = parts.next().unwrap_or("*");
        (base, sub)
    }

    /// Return the smallest of two `Option<u64>` values (`None` = unlimited).
    fn min_option(a: Option<u64>, b: Option<u64>) -> Option<u64> {
        match (a, b) {
            (None, None) => None,
            (Some(v), None) | (None, Some(v)) => Some(v),
            (Some(x), Some(y)) => Some(x.min(y)),
        }
    }

    /// Merge per-mime overrides: union of patterns, most-restrictive value per
    /// pattern, and — critically — collapse *overlapping* patterns so a broader
    /// wildcard cap always tightens the more-specific entries it covers.
    ///
    /// Consumers pick the most-specific matching entry, so without the final
    /// tightening pass `image/png = 50MB` alongside `image/* = 10MB` would let a
    /// PNG upload use 50MB and silently ignore the stricter 10MB wildcard cap.
    fn merge_per_mime(
        tenant: &[MimeSizeOverride],
        user: &[MimeSizeOverride],
    ) -> Vec<MimeSizeOverride> {
        let mut result: Vec<MimeSizeOverride> = tenant.to_vec();

        // 1. Union: for an identical pattern take the smaller value, else add.
        for u in user {
            if let Some(existing) = result.iter_mut().find(|e| e.mime == u.mime) {
                existing.max_bytes = existing.max_bytes.min(u.max_bytes);
            } else {
                result.push(u.clone());
            }
        }

        // 2. Tighten each entry by every *broader* pattern that also covers it,
        //    so a specific entry can never be looser than a matching wildcard.
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

    /// Does `pattern` cover `other` — i.e. does every concrete mime matching
    /// `other` also match `pattern`? True for exact equality or a `type/*`
    /// wildcard over the same base type.
    fn mime_pattern_covers(pattern: &str, other: &str) -> bool {
        if pattern == other {
            return true;
        }
        let (p_type, p_sub) = Self::split_mime(pattern);
        let (o_type, _) = Self::split_mime(other);
        p_type == o_type && p_sub == "*"
    }

    /// Merge metadata limits: smallest non-None value from each field.
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

// ── Pure policy-enforcement helpers ───────────────────────────────────────────

impl PolicyResolver {
    /// Returns `true` if `mime_type` matches any pattern in `allowed`.
    /// Supports exact match and `*` wildcard for subtype (e.g. `"image/*"`).
    /// A pattern without a `/` (e.g. `"image"`) is malformed and never matches.
    #[must_use]
    pub(crate) fn mime_allowed(mime_type: &str, allowed: &[String]) -> bool {
        allowed.iter().any(|pat| {
            if pat == mime_type {
                return true;
            }
            // wildcard subtype: "image/*" matches "image/jpeg".
            // A pattern without a `/` is malformed and must not act as a wildcard.
            let Some((pt, ps)) = pat.split_once('/') else {
                return false;
            };
            let Some((mt, _)) = mime_type.split_once('/') else {
                return false;
            };
            ps == "*" && pt == mt
        })
    }

    /// Check that `mime_type` is permitted by the effective policy.
    ///
    /// - `None` `allowed_mime_types` → all types permitted (no restriction).
    /// - `Some([])` → nothing permitted.
    /// - `Some(list)` → must match a pattern in the list.
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    pub(crate) fn check_allowed_mime(
        policy: &EffectivePolicy,
        mime_type: &str,
    ) -> Result<(), crate::domain::error::DomainError> {
        let Some(allowed) = &policy.allowed_mime_types else {
            return Ok(()); // no restriction
        };
        if Self::mime_allowed(mime_type, allowed) {
            Ok(())
        } else {
            Err(crate::domain::error::DomainError::policy_mime_not_allowed(
                mime_type,
            ))
        }
    }

    /// Compute the effective maximum blob size for `mime_type`, taking the most
    /// restrictive of:
    ///   1. Backend hardware ceiling (`backend_max`).
    ///   2. Policy global limit (`EffectivePolicy.max_bytes`).
    ///   3. Policy per-mime override (`EffectivePolicy.per_mime_max_bytes`).
    ///
    /// `None` means unbounded from all sources.
    ///
    /// @cpt-cf-file-storage-fr-size-limits-policy
    #[must_use]
    pub(crate) fn compute_effective_max_bytes(
        policy: &EffectivePolicy,
        mime_type: &str,
        backend_max: Option<u64>,
    ) -> Option<u64> {
        let policy_global = policy.max_bytes;

        // Find the most specific per-mime override that matches.
        let per_mime_max: Option<u64> = policy
            .per_mime_max_bytes
            .iter()
            .filter(|o| Self::mime_allowed(mime_type, std::slice::from_ref(&o.mime)))
            .map(|o| o.max_bytes)
            .reduce(u64::min);

        // Most restrictive = minimum of all non-None ceilings.
        [backend_max, policy_global, per_mime_max]
            .into_iter()
            .flatten()
            .reduce(u64::min)
    }

    /// Validate `entries` against the metadata limits in `policy`.
    ///
    /// @cpt-cf-file-storage-fr-metadata-limits
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
