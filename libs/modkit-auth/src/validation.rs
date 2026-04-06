use crate::claims_error::ClaimsError;
use crate::standard_claims::StandardClaim;
use time::OffsetDateTime;
use uuid::Uuid;

/// Configuration for common validation
#[derive(Debug, Clone)]
pub struct ValidationConfig {
    /// Allowed issuers (if empty, any issuer is accepted)
    pub allowed_issuers: Vec<String>,

    /// Allowed audiences (if empty, any audience is accepted)
    pub allowed_audiences: Vec<String>,

    /// Leeway in seconds for time-based validations (exp, nbf)
    pub leeway_seconds: i64,

    /// Whether the `exp` claim is required (default: `true`).
    /// Set to `false` to allow tokens without an expiration claim.
    pub require_exp: bool,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            allowed_issuers: vec![],
            allowed_audiences: vec![],
            leeway_seconds: 60,
            require_exp: true,
        }
    }
}

/// Validate standard JWT claims in raw JSON against the given configuration.
///
/// Checks performed:
/// 1. **Issuer** (`iss`) — must match one of `config.allowed_issuers` (skipped if empty)
/// 2. **Audience** (`aud`) — at least one must match `config.allowed_audiences` (skipped if empty)
/// 3. **Expiration** (`exp`) — required by default; must not be in the past (with leeway).
///    Set `require_exp = false` to accept tokens without an `exp` claim.
/// 4. **Not Before** (`nbf`) — must not be in the future (with leeway)
///
/// # Errors
/// Returns `ClaimsError` if any validation check fails.
pub fn validate_claims(
    raw: &serde_json::Value,
    config: &ValidationConfig,
) -> Result<(), ClaimsError> {
    // 0. Reject non-object payloads early
    if !raw.is_object() {
        return Err(ClaimsError::InvalidClaimFormat {
            field: "claims".to_owned(),
            reason: "must be a JSON object".to_owned(),
        });
    }

    // 1. Validate issuer
    if !config.allowed_issuers.is_empty() {
        if let Some(iss_value) = raw.get(StandardClaim::ISS) {
            let iss = iss_value
                .as_str()
                .ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::ISS.to_owned(),
                    reason: "must be a string".to_owned(),
                })?;
            if !config.allowed_issuers.iter().any(|a| a == iss) {
                return Err(ClaimsError::InvalidIssuer {
                    expected: config.allowed_issuers.clone(),
                    actual: iss.to_owned(),
                });
            }
        } else {
            return Err(ClaimsError::MissingClaim(StandardClaim::ISS.to_owned()));
        }
    }

    // 2. Validate audience (at least one must match)
    if !config.allowed_audiences.is_empty() {
        if let Some(aud_value) = raw.get(StandardClaim::AUD) {
            let audiences = extract_audiences(aud_value)?;
            let has_match = audiences
                .iter()
                .any(|a| config.allowed_audiences.contains(a));
            if !has_match {
                return Err(ClaimsError::InvalidAudience {
                    expected: config.allowed_audiences.clone(),
                    actual: audiences,
                });
            }
        } else {
            return Err(ClaimsError::MissingClaim(StandardClaim::AUD.to_owned()));
        }
    }

    let now = OffsetDateTime::now_utc();
    let leeway = time::Duration::seconds(config.leeway_seconds);

    // 3. Validate expiration with leeway
    if let Some(exp_value) = raw.get(StandardClaim::EXP) {
        let exp = parse_timestamp(exp_value, StandardClaim::EXP)?;
        let exp_with_leeway =
            exp.checked_add(leeway)
                .ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::EXP.to_owned(),
                    reason: "timestamp with leeway is out of range".to_owned(),
                })?;
        if now > exp_with_leeway {
            return Err(ClaimsError::Expired);
        }
    } else if config.require_exp {
        return Err(ClaimsError::MissingClaim(StandardClaim::EXP.to_owned()));
    }

    // 4. Validate not-before with leeway
    if let Some(nbf_value) = raw.get(StandardClaim::NBF) {
        let nbf = parse_timestamp(nbf_value, StandardClaim::NBF)?;
        let nbf_with_leeway =
            nbf.checked_sub(leeway)
                .ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::NBF.to_owned(),
                    reason: "timestamp with leeway is out of range".to_owned(),
                })?;
        if now < nbf_with_leeway {
            return Err(ClaimsError::NotYetValid);
        }
    }

    Ok(())
}

/// Helper to parse a UUID from a JSON value.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a valid UUID string.
pub fn parse_uuid_from_value(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<Uuid, ClaimsError> {
    value
        .as_str()
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be a string".to_owned(),
        })
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| ClaimsError::InvalidClaimFormat {
                field: field_name.to_owned(),
                reason: "must be a valid UUID".to_owned(),
            })
        })
}

/// Helper to parse an array of UUIDs from a JSON value.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not an array of valid UUID strings.
pub fn parse_uuid_array_from_value(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<Vec<Uuid>, ClaimsError> {
    value
        .as_array()
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be an array".to_owned(),
        })?
        .iter()
        .map(|v| parse_uuid_from_value(v, field_name))
        .collect()
}

/// Helper to parse timestamp (seconds since epoch) into `OffsetDateTime`.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a valid unix timestamp.
pub fn parse_timestamp(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<OffsetDateTime, ClaimsError> {
    let ts = value
        .as_i64()
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be a number (unix timestamp)".to_owned(),
        })?;

    OffsetDateTime::from_unix_timestamp(ts).map_err(|_| ClaimsError::InvalidClaimFormat {
        field: field_name.to_owned(),
        reason: "invalid unix timestamp".to_owned(),
    })
}

/// Helper to extract string from JSON value.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a string.
pub fn extract_string(value: &serde_json::Value, field_name: &str) -> Result<String, ClaimsError> {
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be a string".to_owned(),
        })
}

/// Extract audiences from a JSON value.
///
/// Accepts a single string or an array of strings. Rejects non-string entries
/// in arrays and non-string/non-array values.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a string,
/// not an array of strings, or contains non-string entries.
pub fn extract_audiences(value: &serde_json::Value) -> Result<Vec<String>, ClaimsError> {
    match value {
        serde_json::Value::String(s) => Ok(vec![s.clone()]),
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                let s = v.as_str().ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::AUD.to_owned(),
                    reason: "must be a string or array of strings".to_owned(),
                })?;
                out.push(s.to_owned());
            }
            Ok(out)
        }
        _ => Err(ClaimsError::InvalidClaimFormat {
            field: StandardClaim::AUD.to_owned(),
            reason: "must be a string or array of strings".to_owned(),
        }),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "validation_tests.rs"]
mod tests;
