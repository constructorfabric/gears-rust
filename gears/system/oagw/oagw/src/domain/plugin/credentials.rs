//! The auth config keys that hold credential-store references. Writes check
//! these references (format and access); the auth plugins read the same keys.

use toolkit_macros::domain_model;

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
/// `secret_ref` as a serde field. Tests in `infra::plugin::registry` keep the
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

/// An auth config entry that references a secret in the credential store.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct CredentialRef<'a> {
    /// The field a violation names: `auth.config.<key>`.
    pub field: &'static str,
    /// The reference as configured.
    pub value: &'a str,
}
