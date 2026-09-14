#![cfg(feature = "magika")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Runtime smoke tests for the real `MagikaDetector`. Only compiled with
//! `--features magika`; needs an ONNX Runtime shared library at run time,
//! pointed to via `ORT_DYLIB_PATH` (minor version >= 24 — see the workspace
//! `ort` dependency comment in Cargo.toml).
//!
//! `MagikaDetector::new` is called through [`new_detector`], not directly:
//! `ort` 2.0.0-rc.12 deadlocks instead of erroring when the runtime it loads
//! is missing/incompatible (confirmed via stack sampling), so a bare call
//! here would hang `cargo test` forever for anyone running this without
//! `ORT_DYLIB_PATH` set to a compatible library. See the matching mitigation
//! in `gear.rs`.

use std::time::Duration;

use file_parser::domain::detector::ContentTypeDetector;
use file_parser::infra::MagikaDetector;

const INIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Bounded wrapper around `MagikaDetector::new`. Panics with a clear message
/// on timeout or load failure instead of ever hanging the test binary.
async fn new_detector(
    extensions: impl IntoIterator<Item = impl Into<String>> + Send + 'static,
) -> MagikaDetector {
    let extensions: Vec<String> = extensions.into_iter().map(Into::into).collect();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        drop(tx.send(MagikaDetector::new(extensions)));
    });

    tokio::time::timeout(INIT_TIMEOUT, rx)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Magika detector init did not finish within {}s — set ORT_DYLIB_PATH to a \
                 compatible (minor version >= 24) ONNX Runtime shared library",
                INIT_TIMEOUT.as_secs()
            )
        })
        .expect("init thread died unexpectedly")
        .expect("Magika session should load")
}

#[tokio::test]
async fn identifies_pdf_content_with_high_confidence() {
    let detector = new_detector(["txt", "log", "md", "pdf", "html", "docx", "png", "jpg"]).await;

    // A structurally complete single-page PDF (catalog, page tree, content
    // stream, and xref table) — Magika is a content classifier, not a
    // magic-byte matcher, so a too-minimal fixture risks lower/unstable
    // confidence on model updates. Larger and closer to a real document
    // keeps this assertion stable.
    let pdf_bytes: &[u8] = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n\
4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n\
5 0 obj\n<< /Length 68 >>\nstream\nBT /F1 24 Tf 72 712 Td (Hello, Magika detection test!) Tj ET\nendstream\nendobj\n\
xref\n0 6\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000114 00000 n \n\
0000000265 00000 n \n\
0000000336 00000 n \n\
trailer\n<< /Size 6 /Root 1 0 R >>\n\
startxref\n456\n\
%%EOF";

    let result = detector
        .detect(bytes::Bytes::from_static(pdf_bytes))
        .await
        .expect("should detect a PDF-like byte stream");

    assert_eq!(result.extension, "pdf");
    assert!(
        result.confidence.get() > 0.5,
        "expected reasonably confident PDF detection, got {}",
        result.confidence
    );
}

/// The detector must survive callers that give up mid-inference.
///
/// An earlier design moved the session out of a pool and returned it by hand
/// after the `.await`, so a dropped future skipped the return and destroyed it;
/// enough cancellations drained the pool and later requests hung forever.
/// Holding the mutex guard inside the blocking task releases it via `Drop`.
///
/// Asserts observable behaviour, not internals, and is bounded by a timeout so a
/// regression fails an assertion instead of hanging the binary.
#[tokio::test]
async fn detector_survives_cancelled_detections() {
    let detector = new_detector(["txt", "pdf", "html"]).await;
    let html = bytes::Bytes::from_static(
        b"<!DOCTYPE html><html><head><title>t</title></head><body><p>hi</p></body></html>",
    );

    // Drop enough pre-poll detections that any per-cancellation session loss
    // would exhaust a pool of any plausible size.
    for _ in 0..8 {
        let pending = detector.detect(html.clone());
        drop(pending);
    }

    // Then cancel one that has actually begun, so the drop lands after
    // `spawn_blocking` rather than before the first poll.
    let started = detector.detect(html.clone());
    let timed_out = tokio::time::timeout(Duration::from_millis(1), started).await;
    drop(timed_out);

    let result = tokio::time::timeout(INIT_TIMEOUT, detector.detect(html))
        .await
        .expect("detector must still respond after cancelled detections, not hang");

    assert_eq!(
        result.map(|d| d.extension),
        Some("html".to_owned()),
        "a cancelled detection must not consume the session permanently"
    );
}

#[tokio::test]
async fn unmapped_detection_returns_none() {
    // Constructed with no registered extensions at all, so nothing this
    // detector identifies can ever have a matching entry.
    let detector = new_detector(Vec::<String>::new()).await;

    let pdf_bytes =
        b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF";

    assert!(
        detector
            .detect(bytes::Bytes::from_static(pdf_bytes))
            .await
            .is_none()
    );
}
