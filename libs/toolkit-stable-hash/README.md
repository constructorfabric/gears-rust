# ToolKit Stable Hash

Stable non-cryptographic hash implementations used by CF/Gears compatibility
contracts.

## API

```rust
use toolkit_stable_hash::murmur3_x86_32;

assert_eq!(murmur3_x86_32(b"hello", 0), 0x248b_fa47);
```

`murmur3_x86_32` is single-pass, allocation-free, and linear in the input
length. Callers must bound untrusted input sizes.

## Security

MurmurHash3 is not cryptographic. Do not use it for passwords, signatures,
integrity checks, secrets, or attacker-controlled routing keys.

## License

Licensed under Apache-2.0.
