//! The auth config keys that hold credential-store references. Writes check
//! these references (format and access); the auth plugins read the same keys.

use std::collections::HashMap;

use credstore_sdk::{CredStoreError, SecretRef};
use toolkit_macros::domain_model;

use crate::domain::gateway_headers::is_gateway_owned_header;
use crate::domain::plugin::PluginError;

#[cfg(test)]
use crate::domain::gts_helpers::{
    APIKEY_AUTH_PLUGIN_ID, NOOP_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
};

/// Auth config key of the `apikey` plugin's secret reference.
pub const SECRET_REF_KEY: &str = "secret_ref";
/// Auth config key of the OAuth2 plugins' client id reference.
pub const CLIENT_ID_REF_KEY: &str = "client_id_ref";
/// Auth config key of the OAuth2 plugins' client secret reference.
pub const CLIENT_SECRET_REF_KEY: &str = "client_secret_ref";

/// An auth config key that holds a credential reference.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct CredentialKey {
    /// The key in `auth.config`.
    pub key: &'static str,
    /// The field a violation about it names (`auth.config.<key>`).
    pub field: &'static str,
}

const SECRET_REF: CredentialKey = CredentialKey {
    key: SECRET_REF_KEY,
    field: oagw_sdk::precondition::SECRET_REF,
};
const CLIENT_ID_REF: CredentialKey = CredentialKey {
    key: CLIENT_ID_REF_KEY,
    field: oagw_sdk::precondition::CLIENT_ID_REF,
};
const CLIENT_SECRET_REF: CredentialKey = CredentialKey {
    key: CLIENT_SECRET_REF_KEY,
    field: oagw_sdk::precondition::CLIENT_SECRET_REF,
};

/// Every key an implemented auth plugin reads a credential reference from,
/// in the order writes check them. A write checks each one present in
/// `auth.config`, whatever the configured plugin: a `secret_ref` was checked
/// on every plugin before the OAuth2 keys were added, and a stored value
/// stays valid if the upstream later switches to a plugin that reads it.
pub const CREDENTIAL_KEYS: &[CredentialKey] = &[SECRET_REF, CLIENT_ID_REF, CLIENT_SECRET_REF];

/// The credential references each implemented auth plugin reads. The
/// OAuth2 plugins read these constants; the `apikey` plugin reads
/// `secret_ref` through [`ApiKeyAuthConfig::parse`]. Tests in `infra::plugin::registry` keep the
/// list complete, check each plugin needs exactly these keys, and that every
/// one is in [`CREDENTIAL_KEYS`].
#[cfg(test)]
pub const AUTH_PLUGIN_CREDENTIAL_KEYS: &[(&str, &[CredentialKey])] = &[
    (NOOP_AUTH_PLUGIN_ID, &[]),
    (APIKEY_AUTH_PLUGIN_ID, &[SECRET_REF]),
    (
        OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
        &[CLIENT_ID_REF, CLIENT_SECRET_REF],
    ),
    (
        OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
        &[CLIENT_ID_REF, CLIENT_SECRET_REF],
    ),
];

/// What kind of failure `e` is. The credential store's own text is not used:
/// it may quote the reference, which never goes to logs or errors (F0008).
pub(crate) fn credstore_error_kind(e: &credstore_sdk::CredStoreError) -> &'static str {
    use credstore_sdk::CredStoreError as E;
    match e {
        E::InvalidSecretRef { .. } => "invalid secret reference",
        E::AccessDenied => "access denied",
        E::NotFound => "secret not found",
        E::Conflict => "secret already exists",
        E::NoPluginAvailable => "no plugin available",
        E::ServiceUnavailable { .. } => "service unavailable",
        E::UnsupportedTransition { .. } => "unsupported sharing transition",
        E::TypeViolation { .. } => "secret type violation",
        E::Internal(_) => "internal error",
    }
}

/// The auth plugin error for a failed credential store read of the secret
/// referenced by `auth.config.<key>`, the same classification `request_ref`
/// mode uses: a missing or refused secret is not found, an outage is
/// unavailable (with the store's retry hint), an error only a write can
/// cause is internal. The error names the key and the kind, never the
/// reference or the store's text (F0008).
pub(crate) fn credstore_read_error(key: &'static str, e: &CredStoreError) -> PluginError {
    match e {
        CredStoreError::NotFound
        | CredStoreError::AccessDenied
        | CredStoreError::InvalidSecretRef { .. } => PluginError::SecretNotFound {
            key,
            kind: credstore_error_kind(e),
        },
        CredStoreError::ServiceUnavailable { retry_after, .. } => {
            PluginError::CredentialStoreUnavailable {
                detail: format!(
                    "credential store unavailable for auth.config.{key}: {}",
                    credstore_error_kind(e)
                ),
                retry_after: *retry_after,
            }
        }
        CredStoreError::NoPluginAvailable | CredStoreError::Internal(_) => {
            PluginError::CredentialStoreUnavailable {
                detail: format!(
                    "credential store unavailable for auth.config.{key}: {}",
                    credstore_error_kind(e)
                ),
                retry_after: None,
            }
        }
        CredStoreError::Conflict
        | CredStoreError::UnsupportedTransition { .. }
        | CredStoreError::TypeViolation { .. } => PluginError::Internal(format!(
            "credential store error for auth.config.{key}: {}",
            credstore_error_kind(e)
        )),
    }
}

/// The auth plugin error for a read that found no secret under the
/// reference in `auth.config.<key>`.
pub(crate) fn secret_missing(key: &'static str) -> PluginError {
    PluginError::SecretNotFound {
        key,
        kind: credstore_error_kind(&CredStoreError::NotFound),
    }
}

/// An auth config entry that references a secret in the credential store.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct CredentialRef<'a> {
    /// The field a violation names: `auth.config.<key>`.
    pub field: &'static str,
    /// The reference as configured.
    pub value: &'a str,
}

// ---------------------------------------------------------------------------
// Credential mode of the API-key plugin (ADR-0022)
// ---------------------------------------------------------------------------

/// Auth config key of the API-key plugin's credential mode.
pub const CREDENTIAL_MODE_KEY: &str = "credential_mode";
/// Auth config key of the secret types a `request_ref` upstream accepts.
pub const REQUEST_SECRET_TYPES_KEY: &str = "request_secret_types";

/// Auth config key of the header the API-key plugin sets.
pub const HEADER_KEY: &str = "header";
/// Auth config key of the prefix the API-key plugin puts before the secret.
pub const PREFIX_KEY: &str = "prefix";

/// The field a missing API-key `secret_ref` names.
pub const SECRET_REF_FIELD: &str = oagw_sdk::precondition::SECRET_REF;
/// The field an auth header name violation names.
pub const HEADER_FIELD: &str = "auth.config.header";
/// The field an API-key `prefix` violation names.
pub const PREFIX_FIELD: &str = "auth.config.prefix";
/// The longest API-key `prefix` a write accepts, in bytes. Real prefixes are
/// a scheme and a space (`Bearer `, `Token `).
pub const MAX_PREFIX_LEN: usize = 64;
/// The field a `credential_mode` violation names.
pub const CREDENTIAL_MODE_FIELD: &str = "auth.config.credential_mode";
/// The field a `request_secret_types` violation names.
pub const REQUEST_SECRET_TYPES_FIELD: &str = "auth.config.request_secret_types";

/// Where the API-key plugin takes the key it injects from.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialMode {
    /// The configured `secret_ref` (the default, and the behavior before
    /// ADR-0022).
    Config,
    /// The request's `X-OAGW-Secret-Ref`, else the configured default.
    RequestRef,
}

impl CredentialMode {
    /// Stored value of [`Self::Config`].
    pub const CONFIG: &'static str = "config";
    /// Stored value of [`Self::RequestRef`].
    pub const REQUEST_REF: &'static str = "request_ref";

    /// The stored value, also the `credential_mode` metric label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => Self::CONFIG,
            Self::RequestRef => Self::REQUEST_REF,
        }
    }

    /// The mode of an effective auth config: [`Self::RequestRef`] only for
    /// the API-key plugin with `credential_mode = request_ref`. Any other
    /// plugin, no auth, or another value reads as [`Self::Config`]; writes
    /// reject unknown values, so a stored row cannot hold one.
    #[must_use]
    pub fn of(auth: Option<&crate::domain::model::AuthConfig>) -> Self {
        let Some(auth) = auth else {
            return Self::Config;
        };
        if auth.plugin_type != crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID {
            return Self::Config;
        }
        match auth
            .config
            .as_ref()
            .and_then(|c| c.get(CREDENTIAL_MODE_KEY))
            .map(String::as_str)
        {
            Some(Self::REQUEST_REF) => Self::RequestRef,
            _ => Self::Config,
        }
    }

    /// Parse a stored value; an absent key is [`Self::Config`]. `None` for
    /// an unknown value.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            None | Some(Self::CONFIG) => Some(Self::Config),
            Some(Self::REQUEST_REF) => Some(Self::RequestRef),
            Some(_) => None,
        }
    }
}

/// Split a stored `request_secret_types` value into its entries: GTS type
/// ids separated by commas, surrounding whitespace ignored. `None` when an
/// entry is empty.
#[must_use]
pub fn split_request_secret_types(value: &str) -> Option<Vec<&str>> {
    value
        .split(',')
        .map(str::trim)
        .map(|entry| (!entry.is_empty()).then_some(entry))
        .collect()
}

// ---------------------------------------------------------------------------
// Parsing: one place for the credential reference and the API-key config
// ---------------------------------------------------------------------------

/// The scheme a credential reference may carry.
pub const CRED_SCHEME: &str = "cred://";

/// Parse a credential reference, with or without the `cred://` scheme,
/// without asking the credential store whether it exists. The write checks,
/// the API-key config and the `X-OAGW-Secret-Ref` header all use it.
///
/// # Errors
///
/// The credential store's syntax error for a malformed reference.
pub fn parse_credential_ref(value: &str) -> Result<SecretRef, CredStoreError> {
    SecretRef::new(value.strip_prefix(CRED_SCHEME).unwrap_or(value))
}

/// Why an auth config cannot be used: the field it names, the violation
/// reason code and a message. Never quotes a credential reference. A write
/// turns it into a 400 field violation; the proxy, reading a row stored
/// before a check existed, into 401 `AUTH_PLUGIN_INTERNAL`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfigError {
    /// The field the violation names (`auth.config.<key>`).
    pub field: &'static str,
    /// The violation reason code (`oagw_sdk::field::*`).
    pub reason: &'static str,
    /// A human-readable explanation.
    pub detail: String,
}

impl AuthConfigError {
    fn new(field: &'static str, reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            field,
            reason,
            detail: detail.into(),
        }
    }
}

/// The credential mode of an auth config: absent is `config`.
///
/// # Errors
///
/// An unknown value, on `auth.config.credential_mode`.
pub fn parse_credential_mode(
    config: Option<&HashMap<String, String>>,
) -> Result<CredentialMode, AuthConfigError> {
    CredentialMode::parse(
        config
            .and_then(|c| c.get(CREDENTIAL_MODE_KEY))
            .map(String::as_str),
    )
    .ok_or_else(|| {
        AuthConfigError::new(
            CREDENTIAL_MODE_FIELD,
            oagw_sdk::field::INVALID_VALUE,
            format!(
                "{CREDENTIAL_MODE_FIELD}: must be '{}' or '{}'",
                CredentialMode::CONFIG,
                CredentialMode::REQUEST_REF
            ),
        )
    })
}

/// The `request_secret_types` allowlist (ADR-0022 guard 4): `None` when
/// absent, else the GTS type ids it lists.
///
/// # Errors
///
/// On `auth.config.request_secret_types`: set outside `request_ref` mode,
/// or not a comma-separated list of well-formed GTS type ids.
pub fn parse_request_secret_types(
    config: Option<&HashMap<String, String>>,
    mode: CredentialMode,
) -> Result<Option<Vec<String>>, AuthConfigError> {
    let Some(types) = config.and_then(|c| c.get(REQUEST_SECRET_TYPES_KEY)) else {
        return Ok(None);
    };
    if mode != CredentialMode::RequestRef {
        return Err(AuthConfigError::new(
            REQUEST_SECRET_TYPES_FIELD,
            oagw_sdk::field::VALIDATION,
            format!(
                "{REQUEST_SECRET_TYPES_FIELD}: allowed only with {CREDENTIAL_MODE_KEY} '{}'",
                CredentialMode::REQUEST_REF
            ),
        ));
    }
    let malformed = || {
        AuthConfigError::new(
            REQUEST_SECRET_TYPES_FIELD,
            oagw_sdk::field::INVALID_GTS_FORMAT,
            format!("{REQUEST_SECRET_TYPES_FIELD}: must be a comma-separated list of GTS type ids"),
        )
    };
    let entries = split_request_secret_types(types).ok_or_else(malformed)?;
    for entry in &entries {
        gts::GtsTypeId::try_new(entry).map_err(|_| malformed())?;
    }
    Ok(Some(entries.into_iter().map(str::to_owned).collect()))
}

/// The API-key auth plugin's config, parsed once. The write check, the
/// plugin (`config` mode) and the proxy's `request_ref` path all read it
/// through [`Self::parse`], so the three cannot drift apart.
#[domain_model]
#[derive(Clone)]
pub struct ApiKeyAuthConfig {
    /// Where the key comes from.
    pub mode: CredentialMode,
    /// The header the key goes in: a valid header name, lowercase.
    pub header: String,
    /// What goes before the key value; empty when unset.
    pub prefix: String,
    /// The configured secret: required in `config` mode, the optional
    /// default in `request_ref` mode.
    pub secret_ref: Option<SecretRef>,
    /// The secret types a `request_ref` upstream accepts; `None` accepts any.
    pub request_secret_types: Option<Vec<String>>,
}

impl ApiKeyAuthConfig {
    /// Parse what the plugin needs to run: a known mode, a `header` that is
    /// a valid header name, a well-formed `secret_ref` (required in `config`
    /// mode), and a well-formed `request_secret_types` only in `request_ref`
    /// mode. Used on every request as well as on writes, so it holds only
    /// rules every stored row was written under; newer rules are in
    /// [`Self::check_write_policy`].
    ///
    /// # Errors
    ///
    /// The first violation, naming its field.
    pub fn parse(config: Option<&HashMap<String, String>>) -> Result<Self, AuthConfigError> {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let mode = parse_credential_mode(config)?;
        let header = get(HEADER_KEY)
            .ok_or_else(|| {
                AuthConfigError::new(
                    HEADER_FIELD,
                    oagw_sdk::field::REQUIRED,
                    format!("{HEADER_FIELD}: required by the API-key auth plugin"),
                )
            })?
            .to_ascii_lowercase();
        if http::HeaderName::from_bytes(header.as_bytes()).is_err() {
            return Err(AuthConfigError::new(
                HEADER_FIELD,
                oagw_sdk::field::INVALID_VALUE,
                format!("{HEADER_FIELD}: not a valid header name"),
            ));
        }
        let prefix = get(PREFIX_KEY).cloned().unwrap_or_default();
        let secret_ref = get(SECRET_REF_KEY)
            .map(|value| {
                parse_credential_ref(value).map_err(|e| {
                    let reason = match &e {
                        CredStoreError::InvalidSecretRef { reason } => reason.clone(),
                        // The store's own text may quote the reference (F0008).
                        other => credstore_error_kind(other).to_owned(),
                    };
                    AuthConfigError::new(
                        SECRET_REF_FIELD,
                        oagw_sdk::field::VALIDATION,
                        format!("{SECRET_REF_FIELD}: invalid credential reference: {reason}"),
                    )
                })
            })
            .transpose()?;
        if mode == CredentialMode::Config && secret_ref.is_none() {
            return Err(AuthConfigError::new(
                SECRET_REF_FIELD,
                oagw_sdk::field::REQUIRED,
                format!(
                    "{SECRET_REF_FIELD}: required with {CREDENTIAL_MODE_KEY} '{}'",
                    CredentialMode::CONFIG
                ),
            ));
        }
        let request_secret_types = parse_request_secret_types(config, mode)?;
        Ok(Self {
            mode,
            header,
            prefix,
            secret_ref,
            request_secret_types,
        })
    }

    /// Rules added after rows may already have been stored, so only writes
    /// apply them: the `header` is not one the gateway sets or strips itself
    /// ([`is_gateway_owned_header`]), and a `prefix` is a valid header value
    /// of at most [`MAX_PREFIX_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// The first violation, naming its field.
    pub fn check_write_policy(&self) -> Result<(), AuthConfigError> {
        if is_gateway_owned_header(&self.header) {
            return Err(AuthConfigError::new(
                HEADER_FIELD,
                oagw_sdk::field::INVALID_VALUE,
                format!(
                    "{HEADER_FIELD}: '{}' is set or stripped by the gateway and cannot carry the key",
                    self.header
                ),
            ));
        }
        if self.prefix.len() > MAX_PREFIX_LEN || http::HeaderValue::from_str(&self.prefix).is_err()
        {
            return Err(AuthConfigError::new(
                PREFIX_FIELD,
                oagw_sdk::field::INVALID_VALUE,
                format!(
                    "{PREFIX_FIELD}: must be a valid header value of at most {MAX_PREFIX_LEN} bytes"
                ),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod api_key_config_tests {
    use super::*;

    fn config(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn api_key_type() -> &'static str {
        credstore_sdk::SECRET_TYPE_CATALOG[1].gts_id
    }

    #[test]
    fn config_mode_needs_header_and_secret_ref() {
        let parsed = ApiKeyAuthConfig::parse(Some(&config(&[
            ("header", "Authorization"),
            ("prefix", "Bearer "),
            ("secret_ref", "cred://openai"),
        ])))
        .unwrap();
        assert_eq!(parsed.mode, CredentialMode::Config);
        assert_eq!(parsed.header, "authorization", "lowercased");
        assert_eq!(parsed.prefix, "Bearer ");
        assert_eq!(
            parsed.secret_ref.unwrap().as_ref(),
            "openai",
            "scheme stripped"
        );
        assert!(parsed.request_secret_types.is_none());

        let err = ApiKeyAuthConfig::parse(Some(&config(&[("header", "x-api-key")])))
            .err()
            .unwrap();
        assert_eq!(
            (err.field, err.reason),
            (SECRET_REF_FIELD, oagw_sdk::field::REQUIRED)
        );
        let err = ApiKeyAuthConfig::parse(None).err().unwrap();
        assert_eq!(
            (err.field, err.reason),
            (HEADER_FIELD, oagw_sdk::field::REQUIRED)
        );
    }

    #[test]
    fn request_ref_mode_makes_secret_ref_optional_and_allows_types() {
        let types = format!("{}, {}", api_key_type(), api_key_type());
        let parsed = ApiKeyAuthConfig::parse(Some(&config(&[
            ("credential_mode", "request_ref"),
            ("header", "x-api-key"),
            ("request_secret_types", &types),
        ])))
        .unwrap();
        assert_eq!(parsed.mode, CredentialMode::RequestRef);
        assert!(parsed.secret_ref.is_none());
        assert_eq!(parsed.prefix, "");
        assert_eq!(parsed.request_secret_types.unwrap().len(), 2);
    }

    #[test]
    fn each_violation_names_its_field_and_reason() {
        /// The config entries, then the field and reason the error names.
        type Case = (
            &'static [(&'static str, &'static str)],
            &'static str,
            &'static str,
        );
        let cases: [Case; 6] = [
            (
                &[("credential_mode", "header_ref"), ("header", "a")],
                CREDENTIAL_MODE_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
            (
                &[("header", "bad header"), ("secret_ref", "k")],
                HEADER_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
            (
                &[("header", "a"), ("secret_ref", "not a ref")],
                SECRET_REF_FIELD,
                oagw_sdk::field::VALIDATION,
            ),
            (
                &[
                    ("header", "a"),
                    ("secret_ref", "k"),
                    ("request_secret_types", "x"),
                ],
                REQUEST_SECRET_TYPES_FIELD,
                oagw_sdk::field::VALIDATION,
            ),
            (
                &[
                    ("credential_mode", "request_ref"),
                    ("header", "a"),
                    ("request_secret_types", "not-gts"),
                ],
                REQUEST_SECRET_TYPES_FIELD,
                oagw_sdk::field::INVALID_GTS_FORMAT,
            ),
            (
                &[
                    ("credential_mode", "request_ref"),
                    ("header", "a"),
                    ("request_secret_types", ","),
                ],
                REQUEST_SECRET_TYPES_FIELD,
                oagw_sdk::field::INVALID_GTS_FORMAT,
            ),
        ];
        for (entries, field, reason) in cases {
            let err = ApiKeyAuthConfig::parse(Some(&config(entries)))
                .err()
                .unwrap();
            assert_eq!((err.field, err.reason), (field, reason), "{entries:?}");
            assert!(
                !err.detail.contains("not a ref"),
                "never quotes a reference"
            );
        }
    }

    /// Rules newer than stored rows are write-only: `parse` (also run on
    /// every request) accepts them, `check_write_policy` refuses them.
    #[test]
    fn write_policy_is_separate_from_parse() {
        for (entries, field) in [
            (&[("header", "Host"), ("secret_ref", "k")][..], HEADER_FIELD),
            (
                &[("header", "X-Forwarded-For"), ("secret_ref", "k")][..],
                HEADER_FIELD,
            ),
            (
                &[("header", "a"), ("secret_ref", "k"), ("prefix", "Bearer\n")][..],
                PREFIX_FIELD,
            ),
        ] {
            let parsed = ApiKeyAuthConfig::parse(Some(&config(entries))).unwrap();
            let err = parsed.check_write_policy().err().unwrap();
            assert_eq!(
                (err.field, err.reason),
                (field, oagw_sdk::field::INVALID_VALUE),
                "{entries:?}"
            );
        }
        let long = "p".repeat(MAX_PREFIX_LEN + 1);
        let parsed = ApiKeyAuthConfig::parse(Some(&config(&[
            ("header", "a"),
            ("secret_ref", "k"),
            ("prefix", &long),
        ])))
        .unwrap();
        assert_eq!(
            parsed.check_write_policy().err().unwrap().field,
            PREFIX_FIELD
        );
        let parsed = ApiKeyAuthConfig::parse(Some(&config(&[
            ("header", "authorization"),
            ("prefix", "Bearer "),
            ("secret_ref", "k"),
        ])))
        .unwrap();
        assert!(parsed.check_write_policy().is_ok());
    }

    #[test]
    fn credential_ref_scheme_is_optional() {
        assert_eq!(parse_credential_ref("cred://k").unwrap().as_ref(), "k");
        assert_eq!(parse_credential_ref("k").unwrap().as_ref(), "k");
        assert!(parse_credential_ref("cred://").is_err());
    }
}
