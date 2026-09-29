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
fn non_multiple_of_chunk_size_inputs_are_pinned() {
    // Pins the tail-handling path (remainders of 1, 2, and 3 bytes, with and
    // without preceding full 4-byte blocks) across implementation changes.
    assert_eq!(murmur3_x86_32(b"abcde", 0), 0xe89b_9af6);
    assert_eq!(murmur3_x86_32(b"abcdef", 0), 0x6181_c085);
    assert_eq!(murmur3_x86_32(b"abcdefg", 0), 0x883c_9b06);
}

#[test]
fn repeated_hashing_is_deterministic() {
    for _ in 0..100 {
        assert_eq!(murmur3_x86_32(b"repeat-test-subject", 7), 0xf095_aab5);
    }
}
