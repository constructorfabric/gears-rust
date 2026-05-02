#!/bin/bash -eu
# Copyright 2026 CyberFabric Foundation
# SPDX-License-Identifier: Apache-2.0

# Update Rust toolchain to latest nightly (project requires Rust 1.95+)
# ClusterFuzzLite sets RUSTUP_TOOLCHAIN=nightly-2025-09-05 which is too old
# We must override it to use latest nightly
unset RUSTUP_TOOLCHAIN
rustup toolchain install nightly --force
rustup default nightly
rustup component add rust-src --toolchain nightly
export RUSTUP_TOOLCHAIN=nightly
echo "Rust version: $(rustc --version)"

cd $SRC/cyberfabric

# Build all fuzz targets with optimization
cargo fuzz build -O --fuzz-dir tools/fuzz

# Copy all fuzz target binaries to $OUT
FUZZ_TARGET_OUTPUT_DIR=tools/fuzz/target/x86_64-unknown-linux-gnu/release
for f in tools/fuzz/fuzz_targets/*.rs; do
    FUZZ_TARGET_NAME=$(basename ${f%.*})
    if [ -f "$FUZZ_TARGET_OUTPUT_DIR/$FUZZ_TARGET_NAME" ]; then
        cp "$FUZZ_TARGET_OUTPUT_DIR/$FUZZ_TARGET_NAME" "$OUT/"
    fi
done
