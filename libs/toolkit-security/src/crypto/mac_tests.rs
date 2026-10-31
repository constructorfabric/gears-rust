use super::{CryptoError, HmacSha256};

#[test]
fn hmac_sha256_matches_rfc4231_including_long_keys() {
    // https://www.rfc-editor.org/rfc/rfc4231 §4.2 and §4.7.
    for (key, message, expected) in [
        (
            vec![0x0b; 20],
            b"Hi There".as_slice(),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        ),
        (
            vec![0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First".as_slice(),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        ),
    ] {
        let key = HmacSha256::new(&key).unwrap();
        let tag = key.sign(message).unwrap();
        assert_eq!(hex::encode(tag), expected);
        key.verify(message, &tag).unwrap();
        assert_eq!(key.verify(b"changed", &tag), Err(CryptoError::InvalidTag));
        assert_eq!(
            key.verify(message, &tag[..31]),
            Err(CryptoError::InvalidTag)
        );
    }
}

#[test]
fn hmac_key_policy_preserves_block_boundaries() {
    for len in [0, 1, 13, 14, 32, 63, 64, 65, 131, 1024] {
        let result = HmacSha256::new(&vec![0x42; len]);
        if cfg!(feature = "fips") && len < 14 {
            assert!(matches!(result, Err(CryptoError::HmacKeyTooShort)));
        } else {
            let key = result.unwrap();
            let tag = key.sign(b"boundary").unwrap();
            key.verify(b"boundary", &tag).unwrap();
            assert_eq!(format!("{key:?}"), "HmacSha256 { .. }");
        }
    }
}

#[test]
fn hmac_rejects_oversized_messages_before_calling_cng() {
    assert_eq!(super::validate_message_len(u32::MAX as usize), Ok(()));
    if let Some(too_long) = (u32::MAX as usize).checked_add(1) {
        assert_eq!(
            super::validate_message_len(too_long),
            Err(CryptoError::MessageTooLong)
        );
    }
}

#[test]
fn independent_block_boundary_vectors() {
    // Python stdlib hmac.new(b"B" * length, b"boundary", hashlib.sha256).
    for (length, expected) in [
        (
            63,
            "aad21a111f49a072c37115179a5a1b25ba6dd1dbc910ef91579a0a1956e3bf19",
        ),
        (
            64,
            "530d9d42d5a799c52ff13d9b7dd313db2604204d1f327e0385c0db94b7e29ff6",
        ),
        (
            65,
            "76c506d36076cba6b3ee21d9956fc6fbf3c9d71fe33e2868063af84c08dde198",
        ),
    ] {
        assert_eq!(
            hex::encode(
                HmacSha256::new(&vec![b'B'; length])
                    .unwrap()
                    .sign(b"boundary")
                    .unwrap()
            ),
            expected
        );
    }
}
