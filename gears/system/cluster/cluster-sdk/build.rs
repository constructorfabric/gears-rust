// Created: 2026-08-12 by Constructor Tech
//! Compiles the committed `cluster.*.v1` `.proto` files into prost messages plus
//! the `*_client` and `*_server` traits.
//!
//! The `.proto` files are **generated artefacts, committed to the repo** — see
//! `examples/gen_grpc_proto.rs`, which regenerates them from the contract IR and
//! the gRPC binding IR with `proto.lock.toml` pinning every field number. This
//! script only compiles them; it never generates them, so a normal build needs no
//! contract machinery and produces no surprises when a contract changes without
//! the `.proto` being regenerated (`gen_grpc_proto` plus review is what catches
//! that).
//!
//! Both halves of the codegen matter: the client backs the SDK's
//! `Remote*Backend` handles, and the `*_server` traits are what the cluster gear
//! implements by hand — gRPC server codegen is out of scope platform-wide, so those
//! four impls are the sanctioned permanent pattern rather than interim glue
//! (DESIGN-DEPLOYABLE-GEAR §6.1).
//!
//! Everything here is behind `grpc-client`, so a Profile 1 build runs no `protoc`
//! and needs none installed.

fn main() {
    #[cfg(feature = "grpc-client")]
    compile_protos();
}

#[cfg(feature = "grpc-client")]
fn compile_protos() {
    const PROTOS: &[&str] = &[
        "proto/cluster/cache/v1/cache.proto",
        "proto/cluster/lock/v1/lock.proto",
        "proto/cluster/leader/v1/leader.proto",
        "proto/cluster/profile/v1/profile.proto",
    ];

    for proto in PROTOS {
        println!("cargo:rerun-if-changed={proto}");
    }
    // The lockfile is not an input to compilation, but a change to it means field
    // numbers moved, which is exactly when a stale build would be misleading.
    println!("cargo:rerun-if-changed=proto.lock.toml");

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(PROTOS, &["proto"])
        .unwrap_or_else(|e| panic!("compiling cluster.v1 protos: {e}"));
}
