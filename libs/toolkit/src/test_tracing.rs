//! Test-only tracing helpers shared by this crate's unit tests.

/// Keeps a second dispatcher registered for the whole binary. With only one, tracing-core
/// computes a callsite's interest from the registering thread's default, so a test without a
/// capture can cache `never` for a callsite another test's capture needs. Call it before
/// installing a thread-local subscriber (`set_default` / `with_default`).
pub fn keep_interest_shared() {
    static SHARED: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}
