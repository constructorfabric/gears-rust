use super::{IdentityConfig, S2sCredentialMapping, StaticAuthNPluginConfig, TokenMapping};
use secrecy::{ExposeSecret, SecretString};
use toolkit::var_expand::ExpandVars;

fn configured(token: &str) -> StaticAuthNPluginConfig {
    StaticAuthNPluginConfig {
        tokens: vec![TokenMapping {
            token: SecretString::from(token.to_owned()),
            identity: IdentityConfig::default(),
        }],
        s2s_credentials: vec![S2sCredentialMapping {
            client_id: "demo-worker".to_owned(),
            client_secret: SecretString::from("${STATIC_AUTHN_TEST_S2S_SECRET}"),
            identity: IdentityConfig::default(),
            bearer_token: Some(SecretString::from("${STATIC_AUTHN_TEST_BEARER}")),
        }],
        ..Default::default()
    }
}

#[test]
fn token_and_s2s_credentials_expand_from_environment() {
    temp_env::with_vars(
        [
            ("STATIC_AUTHN_TEST_BEARER", Some("expanded-bearer")),
            (
                "STATIC_AUTHN_TEST_S2S_SECRET",
                Some("expanded-client-secret"),
            ),
        ],
        || {
            let mut config = configured("${STATIC_AUTHN_TEST_BEARER}");
            config.expand_vars().unwrap();
            assert_eq!(config.tokens[0].token.expose_secret(), "expanded-bearer");
            assert_eq!(
                config.s2s_credentials[0].client_secret.expose_secret(),
                "expanded-client-secret"
            );
            assert_eq!(
                config.s2s_credentials[0]
                    .bearer_token
                    .as_ref()
                    .unwrap()
                    .expose_secret(),
                "expanded-bearer"
            );
            let debug = format!("{config:?}");
            assert!(!debug.contains("expanded-bearer"));
            assert!(!debug.contains("expanded-client-secret"));
        },
    );
}

#[test]
fn missing_credential_variable_is_an_error() {
    temp_env::with_vars([("STATIC_AUTHN_TEST_MISSING", None::<&str>)], || {
        let mut config = configured("${STATIC_AUTHN_TEST_MISSING}");
        let error = config.expand_vars().unwrap_err();
        assert!(error.to_string().contains("STATIC_AUTHN_TEST_MISSING"));
    });
}

#[test]
fn literal_tokens_remain_compatible() {
    let mut config = configured("literal-test-token");
    config.s2s_credentials.clear();
    config.expand_vars().unwrap();
    assert_eq!(config.tokens[0].token.expose_secret(), "literal-test-token");
}
