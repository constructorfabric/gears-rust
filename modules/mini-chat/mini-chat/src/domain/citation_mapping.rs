//! Citation ID mapping: provider `file_id` → internal `attachment_id` UUID.
//!
//! Pure function with no I/O. Applied after the streaming turn completes,
//! before emitting `StreamEvent::Citations`.

use std::collections::HashMap;

use crate::domain::llm::{AttachmentRef, Citation, CitationSource};

/// Map provider `file_ids` in citations to internal attachment UUIDs and filenames.
///
/// - **Web citations**: pass through unchanged.
/// - **File citations** with `attachment_id: None` (malformed): dropped with warning.
/// - **File citations** with unknown `file_id` (not in map): dropped with warning.
/// - **File citations** with known `file_id`: `attachment_id` replaced with UUID string,
///   `title` replaced with the DB filename.
pub fn map_citation_ids<S: ::std::hash::BuildHasher>(
    citations: Vec<Citation>,
    provider_file_id_map: &HashMap<String, AttachmentRef, S>,
) -> Vec<Citation> {
    let total = citations.len();
    let mapped: Vec<Citation> = citations
        .into_iter()
        .filter_map(|mut c| match c.source {
            CitationSource::Web => Some(c),
            CitationSource::File => {
                let file_id = if let Some(id) = &c.attachment_id { id.clone() } else {
                    tracing::warn!(title = %c.title, "malformed file citation: attachment_id is None");
                    return None;
                };
                if let Some(att) = provider_file_id_map.get(&file_id) {
                    c.attachment_id = Some(att.id.to_string());
                    c.title.clone_from(&att.filename);
                    Some(c)
                } else {
                    tracing::warn!(file_id = %file_id, "unmapped file_id in citation (soft-deleted or unknown)");
                    None
                }
            }
        })
        .collect();

    let dropped = total - mapped.len();
    if dropped > 0 {
        tracing::warn!(
            citations_dropped_total = dropped,
            citations_total = total,
            "dropped {dropped}/{total} citations during ID mapping"
        );
    }

    mapped
}
#[cfg(test)]
#[path = "citation_mapping_tests.rs"]
mod tests;
