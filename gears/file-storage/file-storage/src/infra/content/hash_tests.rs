use super::*;

const EMPTY_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn hasher_streams_to_the_sha256_digest_and_counts_bytes() {
    assert_eq!(sha256_hex(b""), EMPTY_HEX);
    assert_eq!(sha256_hex(b"abc"), ABC_HEX);

    let mut h = Hasher::new();
    assert!(h.is_empty());
    h.update(b"ab");
    h.update(b"c");
    assert_eq!(h.len(), 3);
    assert_eq!(hex::encode(h.finalize()), ABC_HEX);
}
