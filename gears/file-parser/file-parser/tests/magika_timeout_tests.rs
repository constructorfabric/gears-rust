#![cfg(feature = "magika")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Proves the startup timeout in `gear.rs` bounds the `ort` 2.0.0-rc.12 hang on
//! an unloadable runtime. Needs `ORT_DYLIB_PATH` pointed at an ONNX Runtime
//! older than `api-24` requires (e.g. Firefox's bundled 1.22.x); run manually,
//! since CI has no known-bad runtime to hand:
//!
//! ```text
//! ORT_DYLIB_PATH=/Applications/Firefox.app/Contents/MacOS/libonnxruntime.dylib \
//!   cargo test -p cf-gears-file-parser --features magika --test magika_timeout_tests -- --ignored
//! ```
//!
//! So **the timeout arm is not covered by CI** — `test-magika` provisions a
//! compatible runtime, which cannot trigger the hang. Only the cancellation arm
//! below runs automatically. Fixing that needs a CI fixture pinning a
//! deliberately old runtime alongside the good one.

use file_parser::gear::init_magika_detector;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "needs ORT_DYLIB_PATH pointed at an incompatible ONNX Runtime; see module docs"]
async fn incompatible_runtime_times_out_instead_of_hanging() {
    // Calls the real production startup path, not a re-implementation of
    // its timeout wrapping, so a regression there (e.g. dropping the
    // timeout) is actually caught.
    let result = init_magika_detector(&[], None, &CancellationToken::new()).await;

    let Err(err) = result else {
        panic!("expected the detector init to fail (via timeout) against an incompatible runtime");
    };
    let message = err.to_string();
    assert!(
        message.contains("did not finish loading"),
        "expected a timeout-shaped error message, got: {message}"
    );
}

/// A token already cancelled at entry must return early *without starting init*,
/// so shutdown does not block on a model load whose result is about to be thrown
/// away.
///
/// This covers the pre-spawn check specifically. An earlier version of this test
/// relied on the `select!` arm instead, which meant the init thread had already
/// been spawned: the test passed, then the process aborted on exit with SIGABRT
/// from ONNX Runtime's `cpuinfo` because teardown raced an in-flight init. That
/// is the same hazard `MagikaInitError::leaked_init_thread` describes, so the
/// mid-init cancellation path is deliberately left untested — exercising it means
/// reproducing that abort.
#[tokio::test]
async fn cancellation_during_init_returns_early() {
    let token = CancellationToken::new();
    token.cancel();

    // `let Err(..) else` rather than `expect_err`: the success type is a
    // `dyn ContentTypeDetector`, which is not `Debug`.
    let Err(err) = init_magika_detector(&[], None, &token).await else {
        panic!("a cancelled token must abort init rather than wait for the timeout");
    };

    let message = err.to_string();
    assert!(
        message.contains("cancelled"),
        "expected a cancellation-shaped error message, got: {message}"
    );
}
