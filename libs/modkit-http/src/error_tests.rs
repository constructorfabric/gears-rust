use super::*;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
struct TestError(&'static str);

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for TestError {}

#[test]
fn test_transport_error_preserves_source() {
    let inner = TestError("connection refused");
    let err = HttpError::Transport(Box::new(inner));

    // Verify source() returns the inner error
    let source = err.source();
    assert!(source.is_some(), "Transport error should have a source");

    // Verify we can downcast to the original error type
    let source = source.unwrap();
    let downcast = source.downcast_ref::<TestError>();
    assert!(
        downcast.is_some(),
        "Should be able to downcast to TestError"
    );
    assert_eq!(downcast.unwrap().0, "connection refused");
}

#[test]
fn test_tls_error_preserves_source() {
    let inner = TestError("certificate expired");
    let err = HttpError::Tls(Box::new(inner));

    let source = err.source();
    assert!(source.is_some(), "TLS error should have a source");

    let source = source.unwrap();
    let downcast = source.downcast_ref::<TestError>();
    assert!(downcast.is_some());
    assert_eq!(downcast.unwrap().0, "certificate expired");
}

#[test]
fn test_error_chain_traversal() {
    let inner = TestError("root cause");
    let err = HttpError::Transport(Box::new(inner));

    // Count errors in chain
    let mut count = 0;
    let mut current: Option<&(dyn Error + 'static)> = Some(&err);
    while let Some(e) = current {
        count += 1;
        current = e.source();
    }

    assert_eq!(
        count, 2,
        "Should have 2 errors in chain: HttpError and TestError"
    );
}
