//! Shared bounded read for backends that load a whole local file into memory.

use std::path::Path;

use tokio::io::AsyncReadExt;

use crate::domain::error::DomainError;

/// Reads `path` into memory, refusing files larger than `max_bytes`.
///
/// One bounded read against a single open handle, not a `metadata()` check
/// followed by `fs::read`: that pair is not atomic, so a file growing in between
/// would still be read in full. `parse_local`'s metadata check rejects oversized
/// files early with a better message, but cannot be the memory bound.
///
/// # Errors
///
/// [`DomainError::IoError`] if the file cannot be opened or read;
/// [`DomainError::InvalidRequest`] if it holds more than `max_bytes`.
pub async fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, DomainError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| DomainError::io_error(format!("Failed to read file: {e}")))?;

    // One byte past the limit, so an oversized file is distinguishable from one
    // that exactly fills it.
    let mut content = Vec::new();
    let read = file
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut content)
        .await
        .map_err(|e| DomainError::io_error(format!("Failed to read file: {e}")))?;

    if read as u64 > max_bytes {
        return Err(DomainError::invalid_request(format!(
            "File size exceeds maximum of {max_bytes} bytes"
        )));
    }

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::read_bounded;

    #[tokio::test]
    async fn reads_a_file_that_fits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.txt");
        std::fs::write(&path, b"hello").expect("write");

        let content = read_bounded(&path, 5).await.expect("exactly at the limit");

        assert_eq!(content, b"hello");
    }

    #[tokio::test]
    async fn rejects_a_file_over_the_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.txt");
        std::fs::write(&path, b"hello world").expect("write");

        let err = read_bounded(&path, 5)
            .await
            .expect_err("one byte over the limit must be rejected");

        assert!(
            matches!(
                err,
                crate::domain::error::DomainError::InvalidRequest { .. }
            ),
            "expected InvalidRequest, got {err:?}"
        );
    }

    #[tokio::test]
    async fn missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().expect("tempdir");

        let err = read_bounded(&dir.path().join("nope.txt"), 1024)
            .await
            .expect_err("a missing file must error");

        assert!(
            matches!(err, crate::domain::error::DomainError::IoError { .. }),
            "expected IoError, got {err:?}"
        );
    }
}
