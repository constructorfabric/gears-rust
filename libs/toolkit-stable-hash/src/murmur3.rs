/// Computes `MurmurHash3` x86 32-bit over `bytes` with the supplied seed.
///
/// This stable compatibility function is non-cryptographic. Do not use it for
/// passwords, signatures, integrity checks, secrets, or attacker-controlled
/// routing keys.
///
/// # Performance
///
/// The implementation is single-pass, allocation-free, and `O(n)` in the
/// input length. Callers at trust boundaries must bound untrusted input sizes.
#[must_use]
pub fn murmur3_x86_32(bytes: &[u8], seed: u32) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;

    let mut hash = seed;
    let mut blocks = bytes.chunks_exact(4);

    for bytes in &mut blocks {
        let mut block = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        block = block.wrapping_mul(C1);
        block = block.rotate_left(15);
        block = block.wrapping_mul(C2);
        hash ^= block;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe654_6b64);
    }

    let tail = blocks.remainder();
    let mut block = 0_u32;
    match tail.len() {
        3 => {
            block ^= u32::from(tail[2]) << 16;
            block ^= u32::from(tail[1]) << 8;
            block ^= u32::from(tail[0]);
            block = block.wrapping_mul(C1);
            block = block.rotate_left(15);
            block = block.wrapping_mul(C2);
            hash ^= block;
        }
        2 => {
            block ^= u32::from(tail[1]) << 8;
            block ^= u32::from(tail[0]);
            block = block.wrapping_mul(C1);
            block = block.rotate_left(15);
            block = block.wrapping_mul(C2);
            hash ^= block;
        }
        1 => {
            block ^= u32::from(tail[0]);
            block = block.wrapping_mul(C1);
            block = block.rotate_left(15);
            block = block.wrapping_mul(C2);
            hash ^= block;
        }
        _ => {}
    }

    // MurmurHash3 x86-32 defines a 32-bit length field.
    #[allow(clippy::cast_possible_truncation)]
    let length = bytes.len() as u32;
    hash ^= length;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85eb_ca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2_ae35);
    hash ^= hash >> 16;
    hash
}
