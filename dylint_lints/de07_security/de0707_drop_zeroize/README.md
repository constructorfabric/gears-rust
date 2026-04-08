Created: 2026-03-18 by Constructor Tech
Updated: 2026-03-18 by Constructor Tech

# DE0707: No Manual Byte-Zeroing in Drop

## What it does

Detects manual byte-zeroing inside `impl Drop` implementations:

- `*ptr = 0` (deref-assign to zero)
- `slice.fill(0)` or `vec.fill(0)`
- `std::ptr::write_bytes(ptr, 0, len)`

These patterns may be **silently optimized away** by the LLVM dead-store elimination pass.

## Why is this bad?

The LLVM optimizer can legally remove writes to memory that are never read again before the memory is freed. Manual zeroing in `Drop::drop` is almost always a dead store from the optimizer's perspective. Sensitive data (keys, tokens, passwords) may remain in memory after the struct is dropped.

The `zeroize` and `secrecy` crates use compiler memory fences to prevent this optimization.

## Example

### Bad

```rust
struct SecretKey {
    data: Vec<u8>,
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.data.fill(0);  // LLVM may remove this!
    }
}
```

```rust
impl Drop for RawBuffer {
    fn drop(&mut self) {
        unsafe {
            std::ptr::write_bytes(self.data, 0, self.len);  // May be optimized away
        }
    }
}
```

### Good

```rust
use zeroize::Zeroize;

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.data.zeroize();  // Uses compiler fence; won't be optimized away
    }
}
```

```rust
use secrecy::{ExposeSecret, SecretBox};

pub type SecretKey = SecretBox<Vec<u8>>;  // Zeroization built-in
```

## Limitations

- Only inspects the immediate body of `Drop::drop`; zeroing delegated to a helper function is not detected.
- Only flags when the target type is `u8` (byte buffers); `fill(255)` or other values are not flagged.
- Zeroing outside `Drop` (e.g. in a `reset()` method) is allowed.

## Configuration

This lint is configured to **deny** by default.

## See Also

- [zeroize crate](https://crates.io/crates/zeroize)
- [secrecy crate](https://crates.io/crates/secrecy)
