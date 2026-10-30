//! How long a read may wait (feature 0007 §2.3).

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Return what is stored now, possibly nothing.
    No,
    /// Return as soon as anything above `offset` is stored, or empty after
    /// this long. For callers that own a dedicated wait, never for a fetch
    /// holding a shared pool permit.
    For(Duration),
}
