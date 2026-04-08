# Two ways to run the OOP examples

## ALL IN

From the root of the repository, run:

```bash
# Build the calculator-oop binary
cargo build --bin calculator-oop --features oop_module -p calculator

# Run the master with OOP modules enabled
cargo run --bin hyperspot-server --features oop-example -- --config config/oop-example-master+follower.yaml
```

## SEPARATE

```bash
# Run the master with OOP modules enabled
cargo run --bin hyperspot-server --features oop-example -- --config config/oop-example-master.yaml

export MODKIT_DIRECTORY_ENDPOINT=http://127.0.0.1:50051
# Run the follower with OOP modules enabled
cargo run --bin calculator-oop --features oop_module -p calculator -- --config config/oop-example-follower.yaml
```
