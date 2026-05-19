//! Infra-layer wrapper that boxes a `sqlx::Error` while exposing its
//! SQLSTATE in `Display`, and forwards `source()` to the original error so
//! the chain remains walkable from the domain layer.

use crate::domain::error::SourceError;

/// Wrapper around `sqlx::Error` that surfaces the SQLSTATE in its `Display`.
///
/// Domain errors hold a [`SourceError`] (`Box<dyn Error + Send + Sync>`) so
/// they remain free of `sqlx` types; this wrapper bridges the layers so the
/// generic chain walker in `domain::error::render_source_chain` still emits
/// the SQLSTATE that operators need for triage.
#[derive(Debug)]
pub struct DbError(sqlx::Error);

impl DbError {
    #[must_use]
    pub fn boxed(e: sqlx::Error) -> SourceError {
        Box::new(Self(e))
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let sqlx::Error::Database(db) = &self.0
            && let Some(code) = db.code()
        {
            return write!(f, "{} (sqlstate={code})", self.0);
        }
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
