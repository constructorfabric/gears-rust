// Created: 2026-03-13 by Constructor Tech
// Updated: 2026-03-13 by Constructor Tech
#![allow(dead_code)]

struct SecretKey {
    data: Vec<u8>,
}

// Good - Drop that does NOT manually zero bytes
impl Drop for SecretKey {
    fn drop(&mut self) {
        // No manual zeroing; would use zeroize crate in real code
        let _ = self.data.len();
    }
}

// Good - zeroing outside of Drop is fine (not a security issue)
fn clear_buffer(buf: &mut [u8]) {
    buf.fill(0);
}

// Good - Drop with unrelated mutation (not zeroing)
struct Timer {
    id: u32,
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.id = 999;
    }
}

// Good - non-Drop impl with fill(0) is not flagged
struct Buffer {
    data: Vec<u8>,
}

impl Buffer {
    fn reset(&mut self) {
        self.data.fill(0);
    }
}

// Good - fill(255) is not zeroing; only fill(0) is flagged
struct SentinelBuffer {
    data: Vec<u8>,
}

impl Drop for SentinelBuffer {
    fn drop(&mut self) {
        self.data.fill(255); // non-zero fill — not flagged
    }
}

// Good - Drop calls a helper function (helper may zero, but the Drop body itself doesn't)
struct ManagedSecret {
    data: Vec<u8>,
}

fn secure_erase(buf: &mut Vec<u8>) {
    buf.fill(0); // outside Drop — not flagged
}

impl Drop for ManagedSecret {
    fn drop(&mut self) {
        secure_erase(&mut self.data); // indirect call — not flagged
    }
}

fn main() {}
