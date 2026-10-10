use crate::domain::error::DomainError;
use crate::domain::model::Endpoint;

/// Maximum length for an upstream alias.
pub(crate) const MAX_ALIAS_LENGTH: usize = 253;

/// Validate an alias: non-empty, max length, safe charset (alphanumeric + `.:-_`),
/// must contain at least one alphanumeric character, and must not be a dot-segment.
pub(in crate::domain::services) fn validate_alias(alias: &str) -> Result<(), DomainError> {
    if alias.is_empty() {
        return Err(DomainError::validation("alias must not be empty"));
    }
    if alias.len() > MAX_ALIAS_LENGTH {
        return Err(DomainError::validation(format!(
            "alias must not exceed {MAX_ALIAS_LENGTH} characters"
        )));
    }
    if !alias
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
    {
        return Err(DomainError::validation(
            "alias contains invalid characters; only alphanumeric, '.', ':', '-', '_' are allowed",
        ));
    }
    // Reject dot-segments and punctuation-only aliases to prevent path traversal
    // and ambiguous URL segments in /proxy/{alias}/{path}.
    if alias == "." || alias == ".." {
        return Err(DomainError::validation(
            "alias must not be a dot-segment ('.' or '..')",
        ));
    }
    if !alias.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err(DomainError::validation(
            "alias must contain at least one alphanumeric character",
        ));
    }
    Ok(())
}

/// Normalize an alias to lowercase. Hostname trailing dots are already
/// handled by `Endpoint::normalized_host()` during derivation; this covers
/// user-provided explicit aliases. All trailing dots are stripped.
pub(in crate::domain::services) fn normalize_alias(alias: &str) -> String {
    alias.to_ascii_lowercase().trim_end_matches('.').to_string()
}

/// Check whether the given endpoints are all IP addresses.
fn endpoints_are_ip(endpoints: &[Endpoint]) -> bool {
    !endpoints.is_empty() && endpoints.iter().all(Endpoint::is_ip)
}

/// Attempt to derive an alias from the endpoint list.
///
/// Returns `Some(alias)` when derivation succeeds (hostname-based), or
/// `None` when an explicit alias is required (IP-based or no common suffix).
///
/// Derivation rules:
/// - Single host, standard port → hostname
/// - Single host, non-standard port → hostname:port
/// - Multiple hosts, all identical → treated as single-host
/// - Multiple hosts, common domain suffix (≥2 labels) → common suffix;
///   non-standard port is appended (e.g., `vendor.com:8443`) to avoid
///   collisions between pools on different ports
/// - Multiple hosts, no common suffix → `None`
/// - IP addresses → `None`
pub(in crate::domain::services) fn compute_derived_alias(endpoints: &[Endpoint]) -> Option<String> {
    if endpoints.is_empty() || endpoints_are_ip(endpoints) {
        return None;
    }

    // Collect unique normalized host contributions.
    let contributions: Vec<String> = endpoints.iter().map(|e| e.alias_contribution()).collect();

    // De-duplicate: if all identical, treat as single-endpoint.
    let unique: Vec<&str> = {
        let mut v: Vec<&str> = contributions.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.dedup();
        v
    };

    if unique.len() == 1 {
        return Some(unique[0].to_string());
    }

    // Multi-host: extract pure hostnames for common suffix computation.
    let hosts: Vec<String> = endpoints.iter().map(|e| e.normalized_host()).collect();
    let suffix = common_domain_suffix(&hosts)?;

    // Append :port when the pool uses a non-standard port so that
    // pools with the same domain suffix but different ports get
    // distinct aliases (e.g., `vendor.com` vs `vendor.com:8443`).
    // validate_endpoints guarantees all endpoints share the same port.
    if endpoints[0].is_standard_port() {
        Some(suffix)
    } else {
        Some(format!("{suffix}:{}", endpoints[0].port))
    }
}

/// Extract the longest common domain suffix from a set of hostnames.
///
/// Returns `Some(suffix)` if the common suffix has ≥2 labels, `None` otherwise.
/// Example: `["us.vendor.com", "eu.vendor.com"]` → `Some("vendor.com")`.
pub(super) fn common_domain_suffix(hosts: &[String]) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }

    // Split each host into labels, reversed (rightmost first).
    let reversed: Vec<Vec<&str>> = hosts
        .iter()
        .map(|h| h.split('.').rev().collect::<Vec<_>>())
        .collect();

    // Find the longest common prefix of the reversed labels.
    let min_len = reversed.iter().map(|r| r.len()).min().unwrap_or(0);
    let mut common_count = 0;
    for i in 0..min_len {
        let label = reversed[0][i];
        if reversed.iter().all(|r| r[i] == label) {
            common_count += 1;
        } else {
            break;
        }
    }

    // Minimum 2 common labels (e.g. `vendor.com`, not just `com`).
    if common_count < 2 {
        return None;
    }

    // Reconstruct the suffix in correct order.
    let suffix: Vec<&str> = reversed[0][..common_count].iter().rev().copied().collect();
    let candidate = suffix.join(".");

    // Reject public suffixes (e.g. "co.uk", "com.au") that are not registrable
    // domains. A registrable domain has at least one label beyond the public
    // suffix (e.g. "vendor.co.uk" is registrable, "co.uk" is not).
    if psl::domain(candidate.as_bytes()).is_none() {
        tracing::debug!(suffix = %candidate, "common suffix is a public suffix (not a registrable domain), alias must be explicit");
        return None;
    }

    Some(candidate)
}

/// Enforce alias rules on upstream **creation** when the user provided an alias.
///
/// - Hostname-derivable endpoints: user-provided alias is rejected unless it
///   exactly matches the derived value.
/// - IP or non-derivable endpoints: the explicit alias is accepted.
pub(in crate::domain::services) fn enforce_alias_create_with(
    user_alias: &str,
    endpoints: &[Endpoint],
) -> Result<String, DomainError> {
    match compute_derived_alias(endpoints) {
        Some(derived) => {
            // Reject user-provided alias when derivation is possible.
            let normalized_user = normalize_alias(user_alias);
            if normalized_user != derived {
                return Err(DomainError::validation(format!(
                    "alias is auto-derived for hostname-based endpoints; \
                     remove the 'alias' field (derived: '{derived}')"
                )));
            }
            // User provided the exact derived value — tolerate silently.
            validate_alias(&derived)?;
            Ok(derived)
        }
        None => {
            let normalized = normalize_alias(user_alias);
            validate_alias(&normalized)?;
            Ok(normalized)
        }
    }
}

/// Enforce alias rules on upstream **creation** when no alias is provided.
///
/// - Hostname-derivable endpoints: alias is auto-derived.
/// - IP or non-derivable endpoints: returns an error (explicit alias required).
pub(in crate::domain::services) fn enforce_alias_create_derived(
    endpoints: &[Endpoint],
) -> Result<String, DomainError> {
    match compute_derived_alias(endpoints) {
        Some(derived) => {
            validate_alias(&derived)?;
            Ok(derived)
        }
        None => Err(DomainError::validation(
            "explicit alias is required for IP-based or heterogeneous-host endpoints",
        )),
    }
}

/// Enforce alias immutability on upstream **update** when endpoints change
/// and the user provided an alias.
///
/// The alias is immutable once set. Any endpoint change that would alter it
/// is rejected — the user must delete and re-create the upstream instead.
/// hostname→IP transitions are always rejected.
pub(in crate::domain::services) fn enforce_alias_update_with(
    user_alias: &str,
    new_endpoints: &[Endpoint],
    existing_alias: &str,
    old_endpoints: &[Endpoint],
) -> Result<String, DomainError> {
    let old_derivable = compute_derived_alias(old_endpoints).is_some();
    let new_derived = compute_derived_alias(new_endpoints);

    match (old_derivable, &new_derived) {
        // New endpoints are hostname-derivable: only allowed when the
        // derived alias matches the existing one exactly.
        (_, Some(derived)) => {
            if *derived != existing_alias {
                return Err(DomainError::validation(format!(
                    "endpoint change would alter the alias from '{existing_alias}' \
                     to '{derived}'; delete and re-create the upstream instead"
                )));
            }
            let normalized_user = normalize_alias(user_alias);
            if normalized_user != *derived {
                return Err(DomainError::validation(format!(
                    "alias is auto-derived for hostname-based endpoints; \
                     remove the 'alias' field (derived: '{derived}')"
                )));
            }
            Ok(derived.clone())
        }
        // hostname → IP: always rejected. The hostname-derived alias would
        // become a lie pointing at arbitrary IP addresses.
        (true, None) => Err(DomainError::validation(
            "cannot change hostname-based endpoints to IP-based; \
                 delete and re-create the upstream instead",
        )),
        // IP → IP: keep existing alias. Reject user-provided alias that
        // would change it — the alias is the routing key and renaming it
        // would break API clients.
        (false, None) => {
            let normalized = normalize_alias(user_alias);
            if normalized != existing_alias {
                return Err(DomainError::validation(format!(
                    "alias cannot be changed from '{existing_alias}' to \
                     '{normalized}'; delete and re-create the upstream instead"
                )));
            }
            Ok(existing_alias.to_string())
        }
    }
}

/// Enforce alias immutability on upstream **update** when endpoints change
/// and no alias is provided by the user.
///
/// The alias is immutable once set. Any endpoint change that would alter it
/// is rejected — the user must delete and re-create the upstream instead.
/// hostname→IP transitions are always rejected.
pub(in crate::domain::services) fn enforce_alias_update_derived(
    new_endpoints: &[Endpoint],
    existing_alias: &str,
    old_endpoints: &[Endpoint],
) -> Result<String, DomainError> {
    let old_derivable = compute_derived_alias(old_endpoints).is_some();
    let new_derived = compute_derived_alias(new_endpoints);

    match (old_derivable, &new_derived) {
        // New endpoints are hostname-derivable: only allowed when the
        // derived alias matches the existing one exactly.
        (_, Some(derived)) => {
            if *derived != existing_alias {
                return Err(DomainError::validation(format!(
                    "endpoint change would alter the alias from '{existing_alias}' \
                     to '{derived}'; delete and re-create the upstream instead"
                )));
            }
            Ok(derived.clone())
        }
        // hostname → IP: always rejected. The hostname-derived alias would
        // become a lie pointing at arbitrary IP addresses.
        (true, None) => Err(DomainError::validation(
            "cannot change hostname-based endpoints to IP-based; \
                 delete and re-create the upstream instead",
        )),
        // IP → IP: keep existing alias.
        (false, None) => Ok(existing_alias.to_string()),
    }
}
