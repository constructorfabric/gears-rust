use super::*;
use std::collections::HashSet;

impl SecureRedirectPolicy {
    fn should_strip_headers(&self, original: &Uri, target: &Uri) -> bool {
        self.config.strip_sensitive_headers && !Self::is_same_origin(original, target)
    }
}

fn uri(s: &str) -> Uri {
    s.parse().unwrap()
}

#[test]
fn test_is_same_origin_same() {
    assert!(SecureRedirectPolicy::is_same_origin(
        &uri("https://example.com/foo"),
        &uri("https://example.com/bar")
    ));
}

#[test]
fn test_is_same_origin_different_host() {
    assert!(!SecureRedirectPolicy::is_same_origin(
        &uri("https://example.com/foo"),
        &uri("https://other.com/bar")
    ));
}

#[test]
fn test_is_same_origin_different_scheme() {
    assert!(!SecureRedirectPolicy::is_same_origin(
        &uri("https://example.com/foo"),
        &uri("http://example.com/bar")
    ));
}

#[test]
fn test_is_same_origin_different_port() {
    assert!(!SecureRedirectPolicy::is_same_origin(
        &uri("https://example.com/foo"),
        &uri("https://example.com:8443/bar")
    ));
}

#[test]
fn test_is_same_origin_explicit_default_port() {
    assert!(SecureRedirectPolicy::is_same_origin(
        &uri("https://example.com/foo"),
        &uri("https://example.com:443/bar")
    ));
}

#[test]
fn test_is_https_downgrade() {
    assert!(SecureRedirectPolicy::is_https_downgrade(
        &uri("https://example.com/foo"),
        &uri("http://example.com/bar")
    ));
}

#[test]
fn test_is_not_https_downgrade() {
    assert!(!SecureRedirectPolicy::is_https_downgrade(
        &uri("http://example.com/foo"),
        &uri("https://example.com/bar")
    ));
    assert!(!SecureRedirectPolicy::is_https_downgrade(
        &uri("https://example.com/foo"),
        &uri("https://other.com/bar")
    ));
}

#[test]
fn test_allowed_host() {
    let config = RedirectConfig {
        allowed_redirect_hosts: HashSet::from(["trusted.com".to_owned()]),
        ..Default::default()
    };
    let policy = SecureRedirectPolicy::new(config);

    assert!(policy.is_allowed_host(&uri("https://trusted.com/path")));
    assert!(!policy.is_allowed_host(&uri("https://untrusted.com/path")));
}

#[test]
fn test_redirect_config_default() {
    let config = RedirectConfig::default();
    assert_eq!(config.max_redirects, 10);
    assert!(config.same_origin_only);
    assert!(config.strip_sensitive_headers);
    assert!(!config.allow_https_downgrade);
    assert!(config.allowed_redirect_hosts.is_empty());
}

#[test]
fn test_redirect_config_permissive() {
    let config = RedirectConfig::permissive();
    assert_eq!(config.max_redirects, 10);
    assert!(!config.same_origin_only);
    assert!(config.strip_sensitive_headers);
    assert!(!config.allow_https_downgrade);
}

#[test]
fn test_redirect_config_disabled() {
    let config = RedirectConfig::disabled();
    assert_eq!(config.max_redirects, 0);
}

#[test]
fn test_redirect_config_for_testing() {
    let config = RedirectConfig::for_testing();
    assert!(!config.same_origin_only);
    assert!(config.allow_https_downgrade);
    assert!(config.strip_sensitive_headers);
}

#[test]
fn test_should_strip_headers() {
    let config = RedirectConfig::default();
    let policy = SecureRedirectPolicy::new(config);

    assert!(
        !policy.should_strip_headers(&uri("https://example.com/a"), &uri("https://example.com/b"))
    );
    assert!(
        policy.should_strip_headers(&uri("https://example.com/a"), &uri("https://other.com/b"))
    );
}

#[test]
fn test_should_strip_headers_disabled() {
    let config = RedirectConfig {
        strip_sensitive_headers: false,
        ..Default::default()
    };
    let policy = SecureRedirectPolicy::new(config);

    assert!(
        !policy.should_strip_headers(&uri("https://example.com/a"), &uri("https://other.com/b"))
    );
}

#[test]
fn test_policy_new() {
    let config = RedirectConfig::default();
    let policy = SecureRedirectPolicy::new(config);
    assert_eq!(policy.redirect_count, 0);
    assert!(!policy.cross_origin_detected);
}
