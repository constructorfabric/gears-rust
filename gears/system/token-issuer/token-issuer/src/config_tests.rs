use super::{MAX_TOKEN_TTL_SECS, TokenIssuerConfig};

fn base() -> TokenIssuerConfig {
    TokenIssuerConfig {
        issuer_base_url: "https://core.example.com".to_owned(),
        ..Default::default()
    }
}

#[test]
fn default_config_values() {
    let c = TokenIssuerConfig::default();
    assert_eq!(c.vendor, "constructorfabric");
    assert_eq!(c.cap_ttl_secs, 300);
    assert_eq!(c.cap_reuse_floor_secs, 150);
    assert_eq!(c.obo_ttl_secs, 60);
    assert_eq!(c.clock_skew_secs, 30);
    assert_eq!(c.cap_key_name, "cap-token-sign");
    assert_eq!(c.obo_key_name, "obo-token-sign");
    assert_eq!(c.obo_audience, "public-api");
    assert_eq!(c.grant_ttl_secs, 300);
    assert_eq!(c.grant_key_name, "grant-token-sign");
    assert!(!c.obo.enabled);
}

#[test]
fn legacy_transit_mount_is_rejected() {
    let result = serde_json::from_value::<TokenIssuerConfig>(serde_json::json!({
        "transit_mount": "transit"
    }));
    assert!(matches!(
        result,
        Err(error) if error.to_string().contains("unknown field `transit_mount`")
    ));
}

#[test]
fn validate_enforces_reuse_floor_invariant() {
    // Valid baseline.
    assert!(base().validate().is_ok());

    // floor < skew → rejected.
    let mut c = base();
    c.cap_reuse_floor_secs = 10;
    c.clock_skew_secs = 30;
    assert!(c.validate().is_err());

    // floor not < ttl → rejected.
    let mut c = base();
    c.cap_reuse_floor_secs = 300;
    c.cap_ttl_secs = 300;
    assert!(c.validate().is_err());

    // empty issuer → rejected.
    let mut c = base();
    c.issuer_base_url = String::new();
    assert!(c.validate().is_err());
}

#[test]
fn validate_bounds_obo_ttl() {
    // obo_ttl_secs = 0 → rejected.
    let mut c = base();
    c.obo_ttl_secs = 0;
    assert!(c.validate().is_err());

    // obo_ttl_secs > 60 → rejected.
    let mut c = base();
    c.obo_ttl_secs = 61;
    assert!(c.validate().is_err());

    // clock_skew_secs >= obo_ttl_secs → rejected.
    let mut c = base();
    c.obo_ttl_secs = 20;
    c.clock_skew_secs = 20;
    assert!(c.validate().is_err());

    // in-bounds → ok.
    let mut c = base();
    c.obo_ttl_secs = 45;
    c.clock_skew_secs = 30;
    assert!(c.validate().is_ok());
}

#[test]
fn issuer_helpers_trim_trailing_slash() {
    let mut c = base();
    c.issuer_base_url = "https://core.example.com/".to_owned();
    assert_eq!(c.cap_issuer(), "https://core.example.com/issuers/cap");
    assert_eq!(c.obo_issuer(), "https://core.example.com/issuers/obo");
    assert_eq!(c.grant_issuer(), "https://core.example.com/issuers/grant");
}

#[test]
fn validate_bounds_cap_and_grant_ttls() {
    let mut c = base();
    c.grant_ttl_secs = 0;
    assert!(c.validate().is_err());

    c.grant_ttl_secs = MAX_TOKEN_TTL_SECS + 1;
    assert!(c.validate().is_err());

    c.grant_ttl_secs = 300;
    c.cap_ttl_secs = MAX_TOKEN_TTL_SECS + 1;
    assert!(c.validate().is_err());

    c.cap_ttl_secs = MAX_TOKEN_TTL_SECS;
    assert!(c.validate().is_ok());
}
