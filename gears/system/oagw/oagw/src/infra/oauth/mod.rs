//! Interactive per-user OAuth2 authorization-code enrollment (infra layer).
//!
//! Implements the [`OAuthEnrollmentService`](crate::domain::services::OAuthEnrollmentService)
//! domain port over three collaborators:
//! - [`token_store::UserTokenStore`]: durable per-user token records (credstore-
//!   backed), the single seam shared with the `oauth2_auth_code` auth plugin.
//! - [`pending_store::PendingAuthorizationStore`]: in-memory, TTL-bounded PKCE/
//!   CSRF state bridging `begin` and the unauthenticated callback.
//! - an HTTP client (discovery / dynamic client registration / token exchange).

pub(crate) mod enrollment;
pub(crate) mod pending_store;
pub(crate) mod token_record;
pub(crate) mod token_store;

pub(crate) use enrollment::OAuthEnrollmentServiceImpl;
pub(crate) use token_record::OAuthTokenRecord;
pub(crate) use token_store::{CredStoreUserTokenStore, UserTokenStore};
