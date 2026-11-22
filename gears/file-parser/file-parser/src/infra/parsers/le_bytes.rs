//! Little-endian readers over byte slices for the binary formats (ZIP, CFB,
//! BIFF, BIFF12): `None` when the value runs past the end.

pub fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let b = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

pub fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn f64_at(bytes: &[u8], at: usize) -> Option<f64> {
    let b: [u8; 8] = bytes.get(at..at.checked_add(8)?)?.try_into().ok()?;
    Some(f64::from_le_bytes(b))
}
