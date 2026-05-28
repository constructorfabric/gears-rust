//! Session sharing primitives.
//!
//! The DESIGN `ShareToken` entity is service-local (the SDK doesn't expose
//! it, because plugins never see share tokens). It pairs a cryptographic
//! bearer secret with the session it grants read-only access to.
//!
//! [`ShareToken::token`] is a bearer secret. The custom [`std::fmt::Debug`]
//! impl redacts it so it cannot leak into logs / tracing spans / test
//! fixtures.
//
// @cpt-cf-chat-engine-domain-share-token:p2

use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A share-link token granting read-only access to a session.
#[domain_model]
#[derive(Clone, Serialize, Deserialize)]
pub struct ShareToken {
    /// The token value itself. Bearer secret — redacted from `Debug`.
    pub share_token: String,
    /// The session this token grants access to.
    pub session_id: Uuid,
    /// When the token was issued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Optional expiration. `None` means the token does not auto-expire
    /// (revocation is still possible via `Session.share_token = None`).
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

impl std::fmt::Debug for ShareToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareToken")
            .field("share_token", &"<redacted>")
            .field("session_id", &self.session_id)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_token() {
        let t = ShareToken {
            share_token: "super-secret-token".into(),
            session_id: Uuid::nil(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            expires_at: None,
        };
        let rendered = format!("{t:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("super-secret-token"));
    }
}
