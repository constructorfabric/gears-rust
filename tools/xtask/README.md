# xtask

Build helper invoked via `cargo xtask <COMMAND>`.

## Commands

| Command | Description |
|---------|-------------|
| `split-debug <NAME>` | Split debug symbols out of `target/release/<NAME>` using platform-native tools and strip the binary. |
| `help` | Show usage. |

### `split-debug`

Detects the platform and runs the appropriate tool chain:

- **Linux** — `objcopy --only-keep-debug` → `objcopy --strip-debug` → `objcopy --add-gnu-debuglink` (produces `.debug` file)
- **macOS** — `dsymutil` → `strip -u -r` (produces `.dSYM` bundle)
- **Windows MSVC** — verifies the `.pdb` already exists next to the binary
- **Windows GNU/MinGW** — falls back to the `objcopy` flow

```sh
cargo build --release --bin cf-server
cargo xtask split-debug cf-server
```
