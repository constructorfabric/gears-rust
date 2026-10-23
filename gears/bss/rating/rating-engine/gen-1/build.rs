//! Computes `engine_digest` over the generation's frozen tree at build time (rating ADR-0006) and
//! hands it to the crate as `RATING_ENGINE_GEN_1_DIGEST`.

use std::path::Path;

#[path = "build/tree_digest.rs"]
mod tree_digest;

/// Emits `RATING_ENGINE_GEN_1_DIGEST` and reruns when any part of the frozen tree changes.
fn main() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in tree_digest::ROOT_FILES {
        println!("cargo:rerun-if-changed={name}");
    }
    for dir in tree_digest::FROZEN_DIRS {
        println!("cargo:rerun-if-changed={dir}");
    }
    let digest = tree_digest::tree_digest(root)?;
    println!("cargo:rustc-env=RATING_ENGINE_GEN_1_DIGEST={digest}");
    Ok(())
}
