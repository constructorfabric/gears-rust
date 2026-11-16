#![allow(clippy::expect_used, clippy::panic)]

//! The production HTTP client against an `IdP` on a private CA: custom CA
//! bundles from config and `HTTPS_PROXY` from the environment, end to end
//! through discovery, JWKS and JWT validation with the strict URL policy.

use std::time::Duration;

use oidc_authn_plugin::authenticate::OidcAuthNPlugin;
use oidc_authn_plugin::config::{IssuerTrustConfig, TrustedIssuerEntry, TrustedIssuerInput};
use oidc_authn_plugin::error::AuthNError;
use oidc_authn_plugin::infra::idp_http::{IdpHttpClient, IdpHttpClientError};
use oidc_authn_plugin::infra::runtime::build_oidc_authn_plugin;

pub mod common;

use common::tls_idp::{ConnectProxy, TlsIdp};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Plugin wired as `init` wires it: `IdpHttpClient::from_settings` and the
/// strict (https-only) URL policy.
fn production_plugin(issuer: &str, ca_bundles: &[String]) -> OidcAuthNPlugin {
    let http_client =
        IdpHttpClient::from_settings(REQUEST_TIMEOUT, ca_bundles).expect("client should build");
    plugin_with_client(issuer, http_client)
}

fn plugin_with_client(issuer: &str, http_client: IdpHttpClient) -> OidcAuthNPlugin {
    let issuer_trust = IssuerTrustConfig::from_inputs([TrustedIssuerInput {
        entry: TrustedIssuerEntry::Issuer(issuer.to_owned()),
        discovery_url: None,
        expected_audience: Vec::new(),
        jose_typ: None,
        clock_skew_leeway_secs: None,
    }])
    .expect("https issuer passes the strict policy");
    build_oidc_authn_plugin(
        common::base_jwt_validation_config(),
        issuer_trust,
        common::plugin_config(),
        http_client,
        common::create_test_metrics(),
    )
    .expect("plugin config should be valid")
}

fn token_for(issuer: &str) -> String {
    common::sign_jwt(
        &serde_json::json!({
            "sub": "550e8400-e29b-41d4-a716-446655440000",
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
            "iss": issuer,
            "exp": common::future_exp(),
        }),
        Some(common::TEST_KID),
    )
}

// Every case runs in a child process with the proxy variables cleared: the
// production client reads them, and a proxy set on the machine running the
// tests must not decide the outcome. The workspace forbids `unsafe`, so
// `set_var` is not an option.

#[tokio::test]
async fn private_ca_bundle_from_config_is_trusted() {
    child::run("child_private_ca_bundle_is_trusted", &[])
        .await
        .expect("child should pass");
}

#[tokio::test]
#[ignore = "spawned by private_ca_bundle_from_config_is_trusted without proxy variables"]
async fn child_private_ca_bundle_is_trusted() {
    if !child::is_child() {
        return;
    }
    let idp = TlsIdp::spawn().await;
    let bundle = idp.write_ca_bundle();
    let plugin = production_plugin(&idp.issuer(), &[bundle.to_string_lossy().into_owned()]);

    let result = plugin.authenticate(&token_for(&idp.issuer())).await;
    std::fs::remove_file(&bundle).ok();

    let claims = result.expect("token from an IdP on the configured CA should validate");
    assert_eq!(claims.sub, "550e8400-e29b-41d4-a716-446655440000");
}

#[tokio::test]
async fn idp_on_private_ca_is_rejected_without_bundle() {
    child::run("child_private_ca_is_rejected_without_bundle", &[])
        .await
        .expect("child should pass");
}

#[tokio::test]
#[ignore = "spawned by idp_on_private_ca_is_rejected_without_bundle without proxy variables"]
async fn child_private_ca_is_rejected_without_bundle() {
    if !child::is_child() {
        return;
    }
    let idp = TlsIdp::spawn().await;
    // With no bundle the client has only the OS trust store. If that store is
    // empty the client does not build, so there is no handshake to check.
    let http_client = match IdpHttpClient::from_settings(REQUEST_TIMEOUT, &[]) {
        Ok(client) => client,
        Err(error @ IdpHttpClientError::Build { .. }) => {
            eprintln!("skipped: no OS root certificates ({error})");
            return;
        }
        Err(error) => panic!("unexpected client error: {error}"),
    };
    let plugin = plugin_with_client(&idp.issuer(), http_client);

    let error = plugin
        .authenticate(&token_for(&idp.issuer()))
        .await
        .expect_err("the OS trust store does not know the private CA");

    assert!(matches!(error, AuthNError::IdpUnreachable), "{error:?}");
    assert!(idp.connections() >= 1, "the IdP was never contacted");
}

/// The parent spawns the `IdP` and the proxy; the child gets `HTTPS_PROXY`.
#[tokio::test]
async fn https_proxy_from_env_carries_idp_calls() {
    let idp = TlsIdp::spawn().await;
    let proxy = ConnectProxy::spawn().await;
    let bundle = idp.write_ca_bundle();

    let outcome = child::run(
        "child_authenticates_through_env_proxy",
        &[
            ("HTTPS_PROXY", proxy.url()),
            (child::ISSUER_VAR, idp.issuer()),
            // The signing key is per process: the JWKS the IdP serves holds
            // this process's key, so this process signs the token.
            (child::TOKEN_VAR, token_for(&idp.issuer())),
            (child::CA_BUNDLE_VAR, bundle.to_string_lossy().into_owned()),
        ],
    )
    .await;
    std::fs::remove_file(&bundle).ok();
    outcome.expect("child should authenticate through the proxy");

    let targets = proxy.targets();
    assert!(!targets.is_empty(), "IdP calls bypassed the proxy");
    assert!(
        targets.iter().all(|target| *target == idp.authority()),
        "{targets:?}"
    );
}

#[tokio::test]
#[ignore = "spawned by https_proxy_from_env_carries_idp_calls with HTTPS_PROXY set"]
async fn child_authenticates_through_env_proxy() {
    if !child::is_child() {
        return;
    }
    let issuer = std::env::var(child::ISSUER_VAR).expect("issuer from the parent");
    let bundle = std::env::var(child::CA_BUNDLE_VAR).expect("CA bundle from the parent");
    let token = std::env::var(child::TOKEN_VAR).expect("token from the parent");
    let plugin = production_plugin(&issuer, &[bundle]);

    plugin
        .authenticate(&token)
        .await
        .expect("token should validate through the proxy");
}

mod child {
    /// Set in every spawned child, so a plain `--include-ignored` run skips
    /// the child tests.
    const MARKER_VAR: &str = "OIDC_AUTHN_TEST_CHILD";
    /// Issuer URL for the child.
    pub const ISSUER_VAR: &str = "OIDC_AUTHN_TEST_CHILD_ISSUER";
    /// CA bundle path for the child.
    pub const CA_BUNDLE_VAR: &str = "OIDC_AUTHN_TEST_CHILD_CA_BUNDLE";
    /// JWT signed by the parent, whose key the `IdP`'s JWKS holds.
    pub const TOKEN_VAR: &str = "OIDC_AUTHN_TEST_CHILD_TOKEN";

    /// Proxy-related variables cleared before `run` sets its own.
    const PROXY_VARS: [&str; 9] = [
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
        "REQUEST_METHOD",
    ];

    /// Whether this process was spawned by [`run`].
    pub fn is_child() -> bool {
        std::env::var_os(MARKER_VAR).is_some()
    }

    /// Run the ignored test `name` of this binary in a fresh process whose
    /// only proxy variables are among `vars`.
    pub async fn run(name: &str, vars: &[(&str, String)]) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args([name, "--exact", "--ignored", "--test-threads=1"]);
        for var in PROXY_VARS {
            cmd.env_remove(var);
        }
        cmd.env(MARKER_VAR, "1");
        for (var, value) in vars {
            cmd.env(var, value);
        }
        let out = cmd.output().await.map_err(|error| error.to_string())?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if out.status.success() && stdout.contains("1 passed") {
            Ok(())
        } else {
            Err(format!(
                "child {name} failed:\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }
}
