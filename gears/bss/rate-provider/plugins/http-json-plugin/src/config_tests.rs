//! Config-deserialization tests — the auth enum's job is to make an unusable
//! credential combination impossible to express, so these assert the parse
//! outcome rather than a downstream runtime check.

use secrecy::ExposeSecret;
use toolkit::var_expand::ExpandVars;

use super::{Auth, HttpJsonPluginConfig};

#[test]
fn omitted_auth_defaults_to_none() {
    let cfg: HttpJsonPluginConfig = serde_saphyr::from_str("id: bank-x").unwrap();
    assert!(matches!(cfg.auth, Auth::None {}));
}

#[test]
fn bearer_carries_its_key() {
    let cfg: HttpJsonPluginConfig =
        serde_saphyr::from_str("auth:\n  kind: bearer\n  api_key: secret-key\n").unwrap();
    match cfg.auth {
        Auth::Bearer { api_key } => assert_eq!(api_key.expose_secret(), "secret-key"),
        other => panic!("expected Bearer, got {other:?}"),
    }
}

#[test]
fn header_key_carries_its_key() {
    let cfg: HttpJsonPluginConfig =
        serde_saphyr::from_str("auth:\n  kind: header-key\n  api_key: secret-key\n").unwrap();
    match cfg.auth {
        Auth::HeaderKey { api_key } => assert_eq!(api_key.expose_secret(), "secret-key"),
        other => panic!("expected HeaderKey, got {other:?}"),
    }
}

#[test]
fn a_kind_that_needs_a_key_cannot_be_configured_without_one() {
    // Asserting rejection (not the message text) keeps the test off the YAML
    // parser's error formatting.
    assert!(
        serde_saphyr::from_str::<HttpJsonPluginConfig>("auth:\n  kind: bearer\n").is_err(),
        "bearer without api_key must be rejected at config load"
    );
}

#[test]
fn a_key_cannot_be_attached_to_the_no_auth_kind() {
    // A key attached to `none` would be silently ignored and every request
    // would go out unauthenticated, so the config must be rejected outright.
    assert!(
        serde_saphyr::from_str::<HttpJsonPluginConfig>("auth:\n  kind: none\n  api_key: k\n")
            .is_err(),
        "a credential attached to the no-auth kind must be rejected"
    );
}

#[test]
fn unknown_auth_kind_is_rejected() {
    assert!(serde_saphyr::from_str::<HttpJsonPluginConfig>("auth:\n  kind: mtls\n").is_err());
}

#[test]
fn stray_top_level_api_key_is_rejected() {
    // `api_key` lives inside `auth`; a stray top-level one must fail loudly
    // rather than silently authenticating with nothing.
    assert!(serde_saphyr::from_str::<HttpJsonPluginConfig>("api_key: leftover\n").is_err());
}

#[test]
fn a_bearer_key_is_expanded_from_the_environment() {
    // A `${VAR}` placeholder keeps the raw key out of the config file, which the
    // effective-config dump serializes verbatim. Without expansion the feed would
    // receive the literal "${...}" string as its credential.
    temp_env::with_vars(
        [("RATE_PROVIDER_TEST_BEARER_KEY", Some("live-bearer"))],
        || {
            let mut cfg: HttpJsonPluginConfig = serde_saphyr::from_str(
                "auth:\n  kind: bearer\n  api_key: \"${RATE_PROVIDER_TEST_BEARER_KEY}\"\n",
            )
            .unwrap();
            cfg.expand_vars().unwrap();
            match cfg.auth {
                Auth::Bearer { api_key } => assert_eq!(api_key.expose_secret(), "live-bearer"),
                other => panic!("expected Bearer, got {other:?}"),
            }
        },
    );
}

#[test]
fn a_header_key_is_expanded_from_the_environment() {
    temp_env::with_vars(
        [("RATE_PROVIDER_TEST_HEADER_KEY", Some("live-header"))],
        || {
            let mut cfg: HttpJsonPluginConfig = serde_saphyr::from_str(
                "auth:\n  kind: header-key\n  api_key: \"${RATE_PROVIDER_TEST_HEADER_KEY}\"\n",
            )
            .unwrap();
            cfg.expand_vars().unwrap();
            match cfg.auth {
                Auth::HeaderKey { api_key } => assert_eq!(api_key.expose_secret(), "live-header"),
                other => panic!("expected HeaderKey, got {other:?}"),
            }
        },
    );
}

#[test]
fn an_unresolvable_placeholder_is_an_error_not_a_literal_credential() {
    // Authenticating with a literal "${MISSING}" would fail against the feed in a
    // way that looks like an upstream problem. `init()` must abort instead.
    temp_env::with_vars([("RATE_PROVIDER_TEST_ABSENT_KEY", None::<&str>)], || {
        let mut cfg: HttpJsonPluginConfig = serde_saphyr::from_str(
            "auth:\n  kind: bearer\n  api_key: \"${RATE_PROVIDER_TEST_ABSENT_KEY}\"\n",
        )
        .unwrap();
        assert!(cfg.expand_vars().is_err());
    });
}

#[test]
fn a_no_credential_config_survives_the_expansion_pass() {
    // The expansion pass runs over every config, not only the ones carrying a
    // credential. A `None` arm that errored (or panicked) would break the ECB-style
    // public-feed case, which is the only configuration v1 actually ships.
    let mut cfg: HttpJsonPluginConfig = serde_saphyr::from_str("id: public-feed").unwrap();
    cfg.expand_vars().unwrap();
    assert!(matches!(cfg.auth, Auth::None {}));
}

#[test]
fn the_auth_default_is_the_no_credential_variant() {
    // Hand-written rather than derived, because `#[derive(Default)]` only accepts
    // a unit variant and the no-credential case is deliberately an empty struct
    // variant. A wrong default here would send a credential-bearing header shape
    // for a feed that has no credential.
    assert!(matches!(Auth::default(), Auth::None {}));
}

#[test]
fn a_literal_key_survives_expansion_unchanged() {
    // A key written straight into the config file (discouraged, but valid) must
    // not be mangled by the expansion pass.
    let mut cfg: HttpJsonPluginConfig =
        serde_saphyr::from_str("auth:\n  kind: bearer\n  api_key: plain-key\n").unwrap();
    cfg.expand_vars().unwrap();
    match cfg.auth {
        Auth::Bearer { api_key } => assert_eq!(api_key.expose_secret(), "plain-key"),
        other => panic!("expected Bearer, got {other:?}"),
    }
}
