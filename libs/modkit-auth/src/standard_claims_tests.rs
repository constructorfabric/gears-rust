use super::*;

#[test]
fn test_claim_constants() {
    assert_eq!(StandardClaim::ISS, "iss");
    assert_eq!(StandardClaim::SUB, "sub");
    assert_eq!(StandardClaim::AUD, "aud");
    assert_eq!(StandardClaim::EXP, "exp");
    assert_eq!(StandardClaim::NBF, "nbf");
    assert_eq!(StandardClaim::IAT, "iat");
    assert_eq!(StandardClaim::JTI, "jti");
    assert_eq!(StandardClaim::AZP, "azp");
}

#[test]
fn test_all_registered() {
    let all = StandardClaim::all_registered();
    assert_eq!(all.len(), 8);
    assert!(all.contains(&"iss"));
    assert!(all.contains(&"sub"));
    assert!(all.contains(&"aud"));
    assert!(all.contains(&"exp"));
    assert!(all.contains(&"nbf"));
    assert!(all.contains(&"iat"));
    assert!(all.contains(&"jti"));
    assert!(all.contains(&"azp"));
}

#[test]
fn test_is_registered() {
    assert!(StandardClaim::is_registered("iss"));
    assert!(StandardClaim::is_registered("sub"));
    assert!(StandardClaim::is_registered("azp"));
    assert!(!StandardClaim::is_registered("custom_claim"));
    assert!(!StandardClaim::is_registered("tenant_id"));
    assert!(!StandardClaim::is_registered("roles"));
}
