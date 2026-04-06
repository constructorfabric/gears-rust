use super::*;
use zeroize::Zeroize;

#[test]
fn debug_is_redacted() {
    let s = SecretString::new("hunter2");
    assert_eq!(format!("{s:?}"), "[REDACTED]");
}

#[test]
fn display_is_redacted() {
    let s = SecretString::new("hunter2");
    assert_eq!(format!("{s}"), "[REDACTED]");
}

#[test]
fn debug_does_not_contain_secret() {
    let secret = "super-secret-value-12345";
    let s = SecretString::new(secret);
    let dbg = format!("{s:?}");
    assert!(!dbg.contains(secret), "Debug must not contain the secret");
}

#[test]
fn expose_returns_original_value() {
    let s = SecretString::new("hunter2");
    assert_eq!(s.expose(), "hunter2");
}

#[test]
fn clone_preserves_value() {
    let s = SecretString::new("value");
    #[allow(clippy::redundant_clone)]
    let c = s.clone();
    assert_eq!(c.expose(), "value");
}

#[cfg(feature = "serde")]
#[test]
fn deserialize_from_json_string() {
    let s: SecretString = serde_json::from_str("\"hunter2\"").unwrap();
    assert_eq!(s.expose(), "hunter2");
}

#[test]
fn zeroize_clears_buffer() {
    let mut s = SecretString::new("sensitive");
    assert_eq!(s.expose(), "sensitive");

    s.zeroize();
    assert!(s.0.is_empty(), "buffer should be empty after zeroize");
}
