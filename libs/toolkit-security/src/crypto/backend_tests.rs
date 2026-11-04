use super::{CryptoError, configured_provider, find_sha256, validate_provider};

#[derive(Debug)]
struct NonFipsRandom;

impl rustls::crypto::SecureRandom for NonFipsRandom {
    fn fill(&self, _: &mut [u8]) -> Result<(), rustls::crypto::GetRandomFailed> {
        Err(rustls::crypto::GetRandomFailed)
    }
}

#[test]
fn missing_witness_is_an_error_without_substitution() {
    let mut provider = configured_provider();
    provider.secure_random = &NonFipsRandom;
    assert_eq!(
        validate_provider(&provider, true),
        Err(CryptoError::FipsUnavailable)
    );
    assert_eq!(validate_provider(&provider, false), Ok(()));
}

#[test]
fn missing_sha256_is_an_error_without_substitution() {
    let mut provider = configured_provider();
    provider.cipher_suites.clear();
    assert!(matches!(
        find_sha256(&provider),
        Err(CryptoError::Sha256Unavailable)
    ));
}
