//! The collector survives a release build with fat LTO, where an
//! unreferenced registration could be dropped by the linker.
//!
//! Builds the `declarations` example in its own target directory (a nested
//! cargo on the shared one would wait on its lock), so it is slow and
//! `#[ignore]`d: run with `cargo test -p cf-gears-toolkit-gts-macros
//! --test release_lto -- --ignored`.

use std::process::Command;

#[test]
#[ignore = "builds the workspace deps in release with fat LTO"]
fn declarations_survive_release_lto() {
    let output = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "--release",
            "-p",
            "cf-gears-toolkit-gts-macros",
            "--example",
            "declarations",
        ])
        .env("CARGO_TARGET_DIR", env!("CARGO_TARGET_TMPDIR"))
        .env("CARGO_PROFILE_RELEASE_LTO", "fat")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    let ids: Vec<&str> = stdout.lines().collect();
    assert_eq!(ids, declarations_fixture::DECLARED_IDS);
}
