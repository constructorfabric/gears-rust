use super::*;

#[test]
fn signature_invalid_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::SignatureInvalid.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "signature invalid"
    ));
}

#[test]
fn unsupported_token_format_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::UnsupportedTokenFormat.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "unsupported token format"
    ));
}

#[test]
fn token_expired_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::TokenExpired.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "token expired"
    ));
}

#[test]
fn untrusted_issuer_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::UntrustedIssuer.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "untrusted issuer"
    ));
}

#[test]
fn missing_claim_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::MissingClaim("iss".to_owned()).into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "missing claim"
    ));
}

#[test]
fn invalid_subject_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::InvalidSubject.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "invalid subject id"
    ));
}

#[test]
fn kid_not_found_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::KidNotFound.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "kid not found"
    ));
}

#[test]
fn unsupported_algorithm_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::UnsupportedAlgorithm.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "unsupported algorithm"
    ));
}

#[test]
fn invalid_audience_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::InvalidAudience.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "invalid audience"
    ));
}

#[test]
fn invalid_token_type_maps_to_unauthorized() {
    let mapped: AuthNResolverError = AuthNError::InvalidTokenType.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::Unauthorized(msg) if msg == "invalid token type"
    ));
}

#[test]
fn idp_unreachable_maps_to_service_unavailable() {
    let mapped: AuthNResolverError = AuthNError::IdpUnreachable.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::ServiceUnavailable(msg) if msg == "identity provider unreachable"
    ));
}

#[test]
fn idp_unreachable_is_idp_failure() {
    assert!(AuthNError::IdpUnreachable.is_idp_failure());
}

#[test]
fn non_idp_errors_are_not_idp_failure() {
    assert!(!AuthNError::SignatureInvalid.is_idp_failure());
    assert!(!AuthNError::UnsupportedTokenFormat.is_idp_failure());
    assert!(!AuthNError::TokenExpired.is_idp_failure());
    assert!(!AuthNError::UntrustedIssuer.is_idp_failure());
    assert!(!AuthNError::MissingClaim("sub".to_owned()).is_idp_failure());
    assert!(!AuthNError::InvalidSubject.is_idp_failure());
    assert!(!AuthNError::KidNotFound.is_idp_failure());
    assert!(!AuthNError::UnsupportedAlgorithm.is_idp_failure());
    assert!(!AuthNError::InvalidAudience.is_idp_failure());
    assert!(!AuthNError::InvalidTokenType.is_idp_failure());
    assert!(!AuthNError::TokenEndpointUnsuccessfulStatus(401).is_idp_failure());
    assert!(!AuthNError::TokenResponseParseFailed.is_idp_failure());
    assert!(!AuthNError::TokenEndpointNotConfigured.is_idp_failure());
}

#[test]
fn token_endpoint_unsuccessful_status_maps_to_token_acquisition_failed() {
    let mapped: AuthNResolverError = AuthNError::TokenEndpointUnsuccessfulStatus(401).into();
    assert!(matches!(
        mapped,
        AuthNResolverError::TokenAcquisitionFailed(msg)
            if msg == "token endpoint returned unsuccessful status: 401"
    ));
}

#[test]
fn token_response_parse_failed_maps_to_token_acquisition_failed() {
    let mapped: AuthNResolverError = AuthNError::TokenResponseParseFailed.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::TokenAcquisitionFailed(msg) if msg == "token response parse failed"
    ));
}

#[test]
fn token_endpoint_not_configured_maps_to_token_acquisition_failed() {
    let mapped: AuthNResolverError = AuthNError::TokenEndpointNotConfigured.into();
    assert!(matches!(
        mapped,
        AuthNResolverError::TokenAcquisitionFailed(msg) if msg == "token endpoint not configured"
    ));
}
