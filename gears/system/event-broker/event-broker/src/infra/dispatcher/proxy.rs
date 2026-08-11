//! Ingest-routing proxy handler: topic-pattern matching, hetero/sharded
//! modes, specificity tie-break (`DESIGN.md:1216-1391`). Shell only - the
//! routing algorithm lands with #4345.

#[derive(Debug, Default)]
pub struct IngestProxy;

impl IngestProxy {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
