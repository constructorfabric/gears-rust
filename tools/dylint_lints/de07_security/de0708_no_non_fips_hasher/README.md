# DE0708: No Non-FIPS Hasher Imports

Prohibits imports of non-FIPS-validated hash crates (`sha2`, `sha1`, `md5`) outside an explicit allow-list.

## Why

These crates use pure-Rust RustCrypto implementations that are not FIPS-validated. They are Phase B entries in `deny-fips.toml` (present in the dependency graph via transitives). This lint prevents new *direct* usage from creeping in without review.

## Allow-list

Currently empty — all direct call sites have been replaced. To add an exception, update `is_in_hasher_allow_list()` in `lint_utils/src/lib.rs`.

## Example

```rust
// Bad — triggers DE0708
use sha2::{Digest, Sha256};

// Good — route through the validated crypto provider,
// or add to the allow-list with a SECURITY.md §9 disclaimer.
```

## References

- `docs/security/SECURITY.md` §9 — FIPS dependency policy and non-crypto disclaimers
- `deny-fips.toml` — Phase A/B dependency bans
