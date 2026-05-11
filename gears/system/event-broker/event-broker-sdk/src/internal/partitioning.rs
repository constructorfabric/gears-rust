/// MurmurHash3 x86 32-bit (seed 0). Must match the broker's implementation
/// byte-for-byte - validated against ADR-0002's 10-vector fixture in tests/contract.rs.
pub fn murmur3_32(data: &[u8], seed: u32) -> u32 {
    const C1: u32 = 0x_cc9e_2d51;
    const C2: u32 = 0x_1b87_3593;

    let mut h = seed;
    let nblocks = data.len() / 4;

    for i in 0..nblocks {
        let mut k = u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap_or([0; 4]));
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        h ^= k;
        h = h.rotate_left(13);
        h = h.wrapping_mul(5).wrapping_add(0xe6546b64);
    }

    let tail = &data[nblocks * 4..];
    let mut k: u32 = 0;
    match tail.len() {
        3 => {
            k ^= u32::from(tail[2]) << 16;
            k ^= u32::from(tail[1]) << 8;
            k ^= u32::from(tail[0]);
            k = k.wrapping_mul(C1);
            k = k.rotate_left(15);
            k = k.wrapping_mul(C2);
            h ^= k;
        }
        2 => {
            k ^= u32::from(tail[1]) << 8;
            k ^= u32::from(tail[0]);
            k = k.wrapping_mul(C1);
            k = k.rotate_left(15);
            k = k.wrapping_mul(C2);
            h ^= k;
        }
        1 => {
            k ^= u32::from(tail[0]);
            k = k.wrapping_mul(C1);
            k = k.rotate_left(15);
            k = k.wrapping_mul(C2);
            h ^= k;
        }
        _ => {}
    }

    h ^= data.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x_85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0x_c2b2_ae35);
    h ^= h >> 16;
    h
}

/// Canonical partition assignment per ADR-0002.
///
/// `key` is `event.partition_key` if present, else `event.subject`. Must be ASCII.
/// Formula: `(murmur3_32(key.as_bytes(), 0) & 0x7FFFFFFF) % partition_count`.
pub fn partition_for(key: &str, partition_count: u32) -> u32 {
    let h = murmur3_32(key.as_bytes(), 0) & 0x7FFF_FFFF;
    h % partition_count
}
