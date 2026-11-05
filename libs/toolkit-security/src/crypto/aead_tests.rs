use super::Aes256Gcm;
use crate::crypto::CryptoError;

// GCM specification test case 16: AES-256, 60-byte plaintext, 20-byte AAD.
// Cross-checked against an independent implementation (python `cryptography`).
const KEY: &str = "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308";

#[cfg(all(feature = "fips", any(target_os = "macos", target_os = "windows")))]
#[test]
fn fips_builds_without_a_route_refuse_at_key_import() {
    assert_eq!(
        Aes256Gcm::new(&hex::decode(KEY).unwrap()).err(),
        Some(CryptoError::UnsupportedAlgorithm)
    );
}

#[cfg(not(all(feature = "fips", any(target_os = "macos", target_os = "windows"))))]
mod routed {
    use super::{Aes256Gcm, CryptoError, KEY};
    use crate::crypto::aead::KEY_LEN;
    use crate::crypto::aead::{NONCE_LEN, TAG_LEN};
    use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
    use proptest::prelude::*;

    const NONCE: &str = "cafebabefacedbaddecaf888";
    const AAD: &str = "feedfacedeadbeeffeedfacedeadbeefabaddad2";
    const PLAINTEXT: &str = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39";
    const SEALED: &str = "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f66276fc6ece0f4e1768cddf8853bb2d551b";
    // Same key and nonce, empty plaintext and AAD: the tag alone.
    const EMPTY_SEALED: &str = "fd2caa16a5832e76aa132c1453eeda7e";

    type Case<'a> = (&'a Aes256Gcm, [u8; NONCE_LEN], &'a [u8], &'a [u8]);

    fn key() -> Aes256Gcm {
        Aes256Gcm::new(&hex::decode(KEY).unwrap()).unwrap()
    }

    fn nonce() -> [u8; NONCE_LEN] {
        hex::decode(NONCE).unwrap().try_into().unwrap()
    }

    // Independent oracle for the sealed layout: AWS-LC's own opener.
    fn oracle_open(nonce: &[u8; NONCE_LEN], aad: &[u8], sealed: &[u8]) -> Vec<u8> {
        let key =
            LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &hex::decode(KEY).unwrap()).unwrap());
        let mut buffer = sealed.to_vec();
        let len = key
            .open_in_place(
                Nonce::assume_unique_for_key(*nonce),
                Aad::from(aad),
                &mut buffer,
            )
            .unwrap()
            .len();
        buffer.truncate(len);
        buffer
    }

    #[test]
    fn opens_the_gcm_known_answer_in_the_aes_gcm_crate_layout() {
        let key = key();
        let aad = hex::decode(AAD).unwrap();
        let plaintext = hex::decode(PLAINTEXT).unwrap();
        let sealed = hex::decode(SEALED).unwrap();
        assert_eq!(sealed.len(), plaintext.len() + TAG_LEN);
        assert_eq!(*key.open(&nonce(), &aad, &sealed).unwrap(), plaintext);
        assert!(
            key.open(&nonce(), &[], &hex::decode(EMPTY_SEALED).unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn seals_with_a_fresh_module_nonce_that_the_oracle_and_open_accept() {
        let key = key();
        let aad = hex::decode(AAD).unwrap();
        let plaintext = hex::decode(PLAINTEXT).unwrap();
        let (first, sealed) = key.seal(&aad, &plaintext).unwrap();
        assert_eq!(sealed.len(), plaintext.len() + TAG_LEN);
        assert_eq!(oracle_open(&first, &aad, &sealed), plaintext);
        assert_eq!(*key.open(&first, &aad, &sealed).unwrap(), plaintext);
        let (second, _) = key.seal(&aad, &plaintext).unwrap();
        assert_ne!(first, second);
        assert!(key.open(&second, &aad, &sealed).is_err());
        let (nonce, empty) = key.seal(&[], &[]).unwrap();
        assert_eq!(empty.len(), TAG_LEN);
        assert!(oracle_open(&nonce, &[], &empty).is_empty());
    }

    #[test]
    fn opening_rejects_every_mismatch() {
        let key = key();
        let aad = b"tenant|person";
        let (nonce, sealed) = key.seal(aad, b"sk-token").unwrap();
        let mut other_nonce = nonce;
        other_nonce[0] ^= 1;
        let mut tampered_body = sealed.clone();
        tampered_body[0] ^= 1;
        let mut tampered_tag = sealed.clone();
        *tampered_tag.last_mut().unwrap() ^= 1;
        let mut other_key = hex::decode(KEY).unwrap();
        other_key[0] ^= 1;
        let other_key = Aes256Gcm::new(&other_key).unwrap();
        let cases: [Case<'_>; 7] = [
            (&key, nonce, b"tenant|other", &sealed),
            (&key, nonce, b"", &sealed),
            (&key, other_nonce, aad, &sealed),
            (&key, nonce, aad, &tampered_body),
            (&key, nonce, aad, &tampered_tag),
            (&key, nonce, aad, &sealed[..sealed.len() - 1]),
            (&other_key, nonce, aad, &sealed),
        ];
        for (key, nonce, aad, ciphertext) in cases {
            assert_eq!(
                key.open(&nonce, aad, ciphertext).err(),
                Some(CryptoError::InvalidTag)
            );
        }
        for short in [&[][..], &[0; TAG_LEN - 1][..]] {
            assert_eq!(
                key.open(&nonce, aad, short).err(),
                Some(CryptoError::InvalidTag)
            );
        }
    }

    #[test]
    fn keys_are_exactly_32_bytes_and_never_printed() {
        for length in [0, 16, 24, KEY_LEN - 1, KEY_LEN + 1, 64] {
            assert_eq!(
                Aes256Gcm::new(&vec![1; length]).err(),
                Some(CryptoError::InvalidKey),
                "{length}"
            );
        }
        let rendered = format!("{:?}", key());
        assert_eq!(rendered, "Aes256Gcm { .. }");
        assert!(!rendered.contains("feffe992"));
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]
        #[test]
        fn any_message_round_trips_and_detects_a_flipped_bit(
            key_bytes in prop::array::uniform32(any::<u8>()),
            aad in prop::collection::vec(any::<u8>(), 0..64),
            plaintext in prop::collection::vec(any::<u8>(), 0..256),
            flip in 0usize..4096,
        ) {
            let key = Aes256Gcm::new(&key_bytes).unwrap();
            let (nonce, sealed) = key.seal(&aad, &plaintext).unwrap();
            prop_assert_eq!(sealed.len(), plaintext.len() + TAG_LEN);
            prop_assert_eq!(&**key.open(&nonce, &aad, &sealed).unwrap(), plaintext.as_slice());
            let mut tampered = sealed;
            let index = flip % tampered.len();
            tampered[index] ^= 1 << (flip % 8);
            prop_assert_eq!(key.open(&nonce, &aad, &tampered).err(), Some(CryptoError::InvalidTag));
        }
    }
}
