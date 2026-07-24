//! Shared persisted OAuth token-record contract.
//!
//! [`OAuthTokenRecord`] is the durable credstore (JSON) contract written by the
//! enrollment service ([`super::enrollment`]) after a successful
//! authorization-code exchange and read independently by the `oauth2_auth_code`
//! auth plugin on the request path. It lives in this shared module — depended on
//! by both writer and reader — so neither side owns the other's schema.

use serde::{Deserialize, Serialize};

/// Per-user token record persisted (JSON) by the OAGW enrollment service and
/// consumed by the `oauth2_auth_code` auth plugin, both through the
/// [`UserTokenStore`](super::token_store::UserTokenStore) seam.
///
/// Self-contained so the plugin needs no per-upstream storage config: the
/// storage key is derived from `(subject, upstream_id)`, and the
/// authorization-server coordinates travel with the record.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OAuthTokenRecord {
    /// Schema version of this persisted record. Written explicitly so readers
    /// can reject records produced by an incompatible future writer instead of
    /// silently mis-deserializing. Absent on legacy (pre-versioning) records,
    /// which are treated as version 1.
    #[serde(default = "default_record_version")]
    pub version: u32,
    /// Client identifier (from static config or dynamic registration).
    pub client_id: String,
    /// Client secret for confidential clients; absent for public/PKCE clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Authorization-server token endpoint used for refresh.
    pub token_endpoint: String,
    /// Current bearer access token.
    pub access_token: String,
    /// Refresh token, when the server issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Absolute access-token expiry (Unix seconds).
    pub expires_at_unix: i64,
    /// Granted scope, space-separated, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

fn default_record_version() -> u32 {
    OAuthTokenRecord::CURRENT_VERSION
}

impl OAuthTokenRecord {
    /// Current schema version written to credstore.
    pub const CURRENT_VERSION: u32 = 1;

    /// Deserialize a stored record, rejecting one written by a newer,
    /// incompatible schema version rather than silently mis-reading it.
    pub(crate) fn from_slice(bytes: &[u8]) -> Result<Self, String> {
        let record: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("corrupt token record: {e}"))?;
        if record.version > Self::CURRENT_VERSION {
            return Err(format!(
                "unsupported token record version {} (max supported {}); re-authorization required",
                record.version,
                Self::CURRENT_VERSION,
            ));
        }
        Ok(record)
    }
}

impl std::fmt::Debug for OAuthTokenRecord {
    /// Redacts secret material (access token, refresh token, client secret) so a
    /// debug log of this record cannot leak live credentials. Presence of the
    /// optional secrets is preserved without exposing their values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokenRecord")
            .field("version", &self.version)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_endpoint", &self.token_endpoint)
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at_unix", &self.expires_at_unix)
            .field("scope", &self.scope)
            .finish()
    }
}
