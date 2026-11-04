use super::Sha256;

#[test]
fn sha256_matches_published_known_answers() {
    for (input, expected) in [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_slice(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(hex::encode(Sha256::digest(input)), expected);
        for chunk_size in [1, 3, 16, 64] {
            let mut hash = Sha256::new();
            hash.update([]);
            for chunk in input.chunks(chunk_size) {
                hash.update(chunk);
            }
            assert_eq!(hex::encode(hash.finalize()), expected);
        }
    }
}

#[test]
fn sha256_streams_a_million_bytes() {
    let mut hash = Sha256::new();
    for _ in 0..1000 {
        hash.update([b'a'; 1000]);
    }
    assert_eq!(
        hex::encode(hash.finalize()),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn selected_sha256_implementation_matches_the_build_policy() {
    super::super::initialize().expect("configured backend");
    let suite = super::backend::sha256_suite().expect("SHA-256 suite");
    if cfg!(feature = "fips") {
        assert!(suite.common.hash_provider.fips());
    }
}
