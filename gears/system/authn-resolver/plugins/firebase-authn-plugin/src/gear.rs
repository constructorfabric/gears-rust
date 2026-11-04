//! Firebase `AuthN` lifecycle and its scoped SDK implementation.
use crate::config::FirebaseAuthnPluginConfig;
use crate::firebase::{AllowedIdentity, FirebaseVerifier, Refusal};
use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthNResolverPluginSpecV1, AuthenticationResult,
    ClientCredentialsRequest,
};
use secrecy::SecretString;
use std::sync::{Arc, OnceLock};
use toolkit::client_hub::ClientScope;
use toolkit::{Gear, GearCtx};
use toolkit_gts::PluginV1;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use uuid::Uuid;

/// The GTS suffix this plugin registers under the `AuthN` resolver's plugin spec.
pub const PLUGIN_SUFFIX: &str = "cf.builtin.firebase_authn.plugin.v1";

/// The Firebase `AuthN` plugin gear. Registers one scoped
/// [`AuthNResolverPluginClient`] for its configured vendor and priority.
#[toolkit::gear(name = "firebase-authn-plugin", deps = [types_registry])]
#[derive(Default)]
pub struct FirebaseAuthnPlugin {
    service: OnceLock<Arc<FirebaseService>>,
}

#[async_trait]
impl Gear for FirebaseAuthnPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config: FirebaseAuthnPluginConfig = ctx.config_expanded_or_default()?;
        config.validate()?;
        let verifier = FirebaseVerifier::new(
            config.project_id.clone(),
            config
                .identities
                .iter()
                .map(|identity| AllowedIdentity {
                    uid: identity.uid.clone(),
                    email: identity.email.clone(),
                })
                .collect(),
        )?;
        let (id, registration) = PluginV1::<AuthNResolverPluginSpecV1>::build_registration(
            PLUGIN_SUFFIX,
            config.vendor.clone(),
            config.priority,
        )?;
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        RegisterResult::ensure_all_ok(&registry.register(vec![registration]).await?)?;
        let service = Arc::new(FirebaseService { config, verifier });
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("Firebase plugin already initialized"))?;
        ctx.client_hub()
            .register_scoped::<dyn AuthNResolverPluginClient>(ClientScope::gts_id(&id), service);
        Ok(())
    }
}

struct FirebaseService {
    config: FirebaseAuthnPluginConfig,
    verifier: FirebaseVerifier,
}

impl FirebaseService {
    /// The subject id for a Firebase uid: the same uid always maps to the same
    /// id, and the project is part of the name, so two projects never collide.
    fn subject_id(&self, uid: &str) -> Uuid {
        Uuid::new_v5(
            &self.config.subject_namespace,
            format!("firebase://{}/{}", self.config.project_id, uid).as_bytes(),
        )
    }
}

/// One answer per kind of refusal, and the reason only in the log: the caller
/// learns 401 or 503, never how close the token came.
fn refusal_to_error(refusal: &Refusal) -> AuthNResolverError {
    match refusal {
        Refusal::CryptoUnavailable(cause) => {
            warn!(%cause, "Firebase crypto backend unavailable; answering 503");
            AuthNResolverError::ServiceUnavailable("crypto backend unavailable".to_owned())
        }
        Refusal::KeysUnavailable(cause) => {
            warn!(%cause, "Firebase signing keys unavailable; answering 503");
            AuthNResolverError::ServiceUnavailable("Firebase signing keys unavailable".to_owned())
        }
        Refusal::NotAValidToken(_) | Refusal::IdentityNotAllowed { .. } => {
            log_token_refusal(refusal);
            AuthNResolverError::Unauthorized("Firebase identity not permitted".to_owned())
        }
    }
}

/// The log carries the uid at most, never the token or the address.
fn log_token_refusal(refusal: &Refusal) {
    match refusal {
        Refusal::NotAValidToken(reason) => debug!(%reason, "Firebase token refused"),
        Refusal::IdentityNotAllowed { uid, .. } => {
            debug!(%uid, "Firebase identity not on the allowlist");
        }
        Refusal::KeysUnavailable(_) | Refusal::CryptoUnavailable(_) => {}
    }
}

#[async_trait]
impl AuthNResolverPluginClient for FirebaseService {
    async fn authenticate(&self, token: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        let uid = self
            .verifier
            .verify_identity(token)
            .await
            .map_err(|refusal| refusal_to_error(&refusal))?;
        let identity = self
            .config
            .identities
            .iter()
            .find(|identity| identity.uid == uid)
            .ok_or_else(|| {
                AuthNResolverError::Unauthorized("Firebase identity not permitted".to_owned())
            })?;
        let subject = self.subject_id(&uid);
        let security_context = SecurityContext::builder()
            .subject_id(subject)
            .subject_tenant_id(identity.tenant_id)
            .subject_type("user")
            .token_scopes(identity.token_scopes.clone())
            .bearer_token(SecretString::from(token.to_owned()))
            .build()
            .map_err(|error| AuthNResolverError::Internal(error.to_string()))?;
        Ok(AuthenticationResult { security_context })
    }

    async fn exchange_client_credentials(
        &self,
        _: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::TokenAcquisitionFailed(
            "Firebase user sign-in required".to_owned(),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "signed token integration fixtures")]
mod tests {
    use super::*;
    use crate::config::IdentityMapping;
    #[test]
    fn local_crypto_failure_returns_service_unavailable() {
        let refusal = Refusal::CryptoUnavailable("backend failed".to_owned());
        assert!(matches!(
            refusal_to_error(&refusal),
            AuthNResolverError::ServiceUnavailable(_)
        ));
    }

    #[tokio::test]
    async fn signed_identity_gets_only_its_configured_tenant_and_scopes() {
        let config = FirebaseAuthnPluginConfig {
            project_id: "firebase-authn-test".to_owned(),
            identities: (1..=3)
                .map(|id| IdentityMapping {
                    uid: format!("account-{id}"),
                    email: format!("account-{id}@example.test"),
                    tenant_id: Uuid::from_u128(id),
                    token_scopes: vec![format!("scope:{id}")],
                })
                .collect(),
            ..Default::default()
        };
        config.validate().unwrap();
        let verifier = crate::firebase::with_test_keys(
            config.project_id.clone(),
            config
                .identities
                .iter()
                .map(|i| AllowedIdentity {
                    uid: i.uid.clone(),
                    email: i.email.clone(),
                })
                .collect(),
            crate::firebase::tests::signing_keys(),
        );
        let service = FirebaseService { config, verifier };
        for id in 1..=3 {
            let token = crate::firebase::tests::signed_claims(
                serde_json::json!({"sub": format!("account-{id}"), "email": format!("account-{id}@example.test"), "tenant_id": Uuid::nil(), "scopes": ["*"]}),
            );
            let result = service.authenticate(&token).await.unwrap();
            assert_eq!(
                result.security_context.subject_tenant_id(),
                Uuid::from_u128(id)
            );
            assert_eq!(
                result.security_context.token_scopes(),
                &[format!("scope:{id}")]
            );
            assert_eq!(
                result.security_context.subject_id(),
                Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("firebase://firebase-authn-test/account-{id}").as_bytes()
                )
            );
        }
    }

    fn service(verifier: FirebaseVerifier) -> FirebaseService {
        FirebaseService {
            config: FirebaseAuthnPluginConfig {
                project_id: crate::firebase::tests::PROJECT.to_owned(),
                ..Default::default()
            },
            verifier,
        }
    }

    #[test]
    fn the_subject_id_is_stable_per_uid_and_distinct_across_uids() {
        let service = service(FirebaseVerifier::without_http(
            crate::firebase::tests::PROJECT.to_owned(),
            Vec::new(),
        ));
        assert_eq!(service.subject_id("uid-a"), service.subject_id("uid-a"));
        assert_ne!(service.subject_id("uid-a"), service.subject_id("uid-b"));
    }

    /// A key outage is the platform's problem and answers 503; anything about
    /// the token answers 401.
    #[tokio::test]
    async fn a_key_outage_is_503_and_a_refused_token_is_401() {
        let token = crate::firebase::tests::signed_claims(serde_json::json!({}));
        let outage = service(FirebaseVerifier::without_http(
            crate::firebase::tests::PROJECT.to_owned(),
            Vec::new(),
        ));
        assert!(matches!(
            outage.authenticate(&token).await,
            Err(AuthNResolverError::ServiceUnavailable(_))
        ));

        let stranger = service(crate::firebase::with_test_keys(
            crate::firebase::tests::PROJECT.to_owned(),
            Vec::new(),
            crate::firebase::tests::signing_keys(),
        ));
        assert!(matches!(
            stranger.authenticate(&token).await,
            Err(AuthNResolverError::Unauthorized(_))
        ));
        assert!(matches!(
            stranger.authenticate("not-a-token").await,
            Err(AuthNResolverError::Unauthorized(_))
        ));
    }
}
