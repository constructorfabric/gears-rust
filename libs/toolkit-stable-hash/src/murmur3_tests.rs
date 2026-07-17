use super::murmur3_x86_32;

#[test]
fn compatibility_vectors_are_pinned() {
    assert_eq!(murmur3_x86_32(b"", 0), 0x0000_0000);
    assert_eq!(murmur3_x86_32(b"a", 0), 0x3c25_69b2);
    assert_eq!(murmur3_x86_32(b"ab", 0), 0x9bbf_d75f);
    assert_eq!(murmur3_x86_32(b"abc", 0), 0xb3dd_93fa);
    assert_eq!(murmur3_x86_32(b"abcd", 0), 0x43ed_676a);
    assert_eq!(murmur3_x86_32(b"hello", 0), 0x248b_fa47);
    assert_eq!(murmur3_x86_32(b"hello", 42), 0xe2db_d2e1);
}

#[test]
fn repeated_hashing_is_deterministic() {
    for _ in 0..100 {
        assert_eq!(murmur3_x86_32(b"repeat-test-subject", 7), 0xf095_aab5);
    }
}
