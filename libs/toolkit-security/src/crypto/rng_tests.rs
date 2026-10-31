use rustls::crypto::{GetRandomFailed, SecureRandom};

use super::{CryptoError, fill, fill_with};

#[test]
fn selected_csprng_fills_buffers_and_accepts_empty_input() {
    fill(&mut []).unwrap();
    let mut first = [0; 32];
    let mut second = [0; 32];
    fill(&mut first).unwrap();
    fill(&mut second).unwrap();
    assert_ne!(first, second);
}

#[derive(Debug)]
struct FailingRandom;

impl SecureRandom for FailingRandom {
    fn fill(&self, _: &mut [u8]) -> Result<(), GetRandomFailed> {
        Err(GetRandomFailed)
    }
}

#[test]
fn csprng_failure_is_returned_to_the_caller() {
    assert_eq!(
        fill_with(&FailingRandom, &mut [0; 32]),
        Err(CryptoError::RandomFailed)
    );
}
