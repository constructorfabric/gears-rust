use std::sync::Arc;

use async_trait::async_trait;
use token_issuer_sdk::{
    PublicKeyVersion, SigAlg, SignatureResult, SigningClientV1, SigningError, SigningKeyRef,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use super::GtsSigningPluginSelector;

const VENDOR: &str = "constructorfabric";

/// A valid signing-plugin instance id (the `SigningPluginSpecV1` type chain with
/// a concrete leaf).
const SIGNING_INSTANCE_ID: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.token_issuer.signing_plugin.v1~cf.core.openbao.signing_plugin.v1";

/// Scoped signing client that records nothing and returns a fixed signature.
struct StubSigner;

#[async_trait]
impl SigningClientV1 for StubSigner {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        _signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        Ok(SignatureResult {
            signature: vec![0xAB; 64],
            key_version: 7,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        Ok(vec![PublicKeyVersion {
            version: 7,
            alg: SigAlg::Es256,
            public_key_pem: "PEM".to_owned(),
        }])
    }
}

/// A hub whose types-registry advertises one signing-plugin instance for `vendor`.
fn hub_with_instance(vendor: &str) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    let registry: Arc<dyn TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new().with_instances([make_test_instance(
            SIGNING_INSTANCE_ID,
            serde_json::json!({
                "id": SIGNING_INSTANCE_ID,
                "vendor": vendor,
                "priority": 10,
                "properties": {},
            }),
        )]),
    );
    hub.register::<dyn TypesRegistryClient>(registry);
    hub
}

fn key() -> SigningKeyRef {
    SigningKeyRef::new("cap-token-sign").expect("valid key ref")
}

#[tokio::test]
async fn sign_errors_when_types_registry_absent() {
    // Empty hub: resolving the plugin instance can't reach the types-registry.
    let selector = GtsSigningPluginSelector::new(Arc::new(ClientHub::new()), VENDOR.to_owned());
    let err = selector
        .sign(&SecurityContext::anonymous(), &key(), b"input")
        .await
        .expect_err("must fail when the types-registry is absent");
    assert!(
        matches!(err, SigningError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn sign_delegates_to_resolved_plugin_client() {
    let hub = hub_with_instance(VENDOR);
    hub.register_scoped::<dyn SigningClientV1>(
        ClientScope::gts_id(SIGNING_INSTANCE_ID),
        Arc::new(StubSigner),
    );
    let selector = GtsSigningPluginSelector::new(hub, VENDOR.to_owned());

    let sig = selector
        .sign(&SecurityContext::anonymous(), &key(), b"input")
        .await
        .expect("delegates to the resolved scoped client");
    assert_eq!(sig.key_version, 7);
}

#[tokio::test]
async fn public_keys_delegates_to_resolved_plugin_client() {
    let hub = hub_with_instance(VENDOR);
    hub.register_scoped::<dyn SigningClientV1>(
        ClientScope::gts_id(SIGNING_INSTANCE_ID),
        Arc::new(StubSigner),
    );
    let selector = GtsSigningPluginSelector::new(hub, VENDOR.to_owned());

    let keys = selector
        .public_keys(&SecurityContext::anonymous(), &key())
        .await
        .expect("delegates to the resolved scoped client");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].version, 7);
}

#[tokio::test]
async fn sign_errors_when_scoped_client_not_registered() {
    // The instance resolves, but no scoped SigningClientV1 is registered yet:
    // the selector fails closed and resets its cached instance id.
    let selector = GtsSigningPluginSelector::new(hub_with_instance(VENDOR), VENDOR.to_owned());
    let err = selector
        .sign(&SecurityContext::anonymous(), &key(), b"input")
        .await
        .expect_err("must fail when the scoped client is missing");
    assert!(
        matches!(err, SigningError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn sign_errors_when_no_vendor_matches() {
    let selector =
        GtsSigningPluginSelector::new(hub_with_instance("some-other-vendor"), VENDOR.to_owned());
    let err = selector
        .sign(&SecurityContext::anonymous(), &key(), b"input")
        .await
        .expect_err("must fail when no instance matches the configured vendor");
    assert!(matches!(err, SigningError::NoPluginAvailable), "{err:?}");
}
