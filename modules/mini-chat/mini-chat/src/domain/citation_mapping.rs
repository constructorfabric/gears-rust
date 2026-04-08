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
mod tests {
    use super::*;
    use crate::domain::llm::TextSpan;
    use uuid::Uuid;

    fn file_citation(file_id: Option<&str>) -> Citation {
        Citation {
            source: CitationSource::File,
            title: "test.pdf".into(),
            url: None,
            attachment_id: file_id.map(String::from),
            snippet: "some text".into(),
            score: None,
            span: Some(TextSpan { start: 0, end: 10 }),
        }
    }

    fn web_citation() -> Citation {
        Citation {
            source: CitationSource::Web,
            title: "Example".into(),
            url: Some("https://example.com".into()),
            attachment_id: None,
            snippet: "web result".into(),
            score: Some(0.9),
            span: None,
        }
    }

    #[test]
    fn known_mapping() {
        let uuid = Uuid::nil();
        let map = HashMap::from([(
            "file-abc".into(),
            AttachmentRef {
                id: uuid,
                filename: "report.pdf".into(),
            },
        )]);
        let result = map_citation_ids(vec![file_citation(Some("file-abc"))], &map);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].attachment_id.as_deref(), Some(&*uuid.to_string()));
        assert_eq!(result[0].title, "report.pdf");
    }

    #[test]
    fn unknown_dropped() {
        let map: HashMap<String, AttachmentRef> = HashMap::new();
        let result = map_citation_ids(vec![file_citation(Some("file-unknown"))], &map);
        assert!(result.is_empty());
    }

    #[test]
    fn soft_deleted_dropped() {
        // If file was soft-deleted, it's not in the map
        let map = HashMap::from([(
            "file-abc".into(),
            AttachmentRef {
                id: Uuid::nil(),
                filename: "test.pdf".into(),
            },
        )]);
        let result = map_citation_ids(vec![file_citation(Some("file-other"))], &map);
        assert!(result.is_empty());
    }

    #[test]
    fn web_passthrough() {
        let map = HashMap::new();
        let result = map_citation_ids(vec![web_citation()], &map);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].source, CitationSource::Web));
    }

    #[test]
    fn empty_map() {
        let map: HashMap<String, AttachmentRef> = HashMap::new();
        let result = map_citation_ids(vec![file_citation(Some("file-x"))], &map);
        assert!(result.is_empty());
    }

    #[test]
    fn malformed_none_file_id() {
        let map = HashMap::from([(
            "file-abc".into(),
            AttachmentRef {
                id: Uuid::nil(),
                filename: "test.pdf".into(),
            },
        )]);
        let result = map_citation_ids(vec![file_citation(None)], &map);
        assert!(result.is_empty());
    }

    #[test]
    fn mixed_citations_partial_mapping() {
        let uuid = Uuid::nil();
        let map = HashMap::from([(
            "file-known".into(),
            AttachmentRef {
                id: uuid,
                filename: "known.pdf".into(),
            },
        )]);
        let citations = vec![
            file_citation(Some("file-known")),
            file_citation(Some("file-unknown")),
            web_citation(),
            file_citation(None),
        ];
        let result = map_citation_ids(citations, &map);
        assert_eq!(result.len(), 2); // known file + web
        assert!(matches!(result[0].source, CitationSource::File));
        assert_eq!(result[0].attachment_id.as_deref(), Some(&*uuid.to_string()));
        assert_eq!(result[0].title, "known.pdf");
        assert!(matches!(result[1].source, CitationSource::Web));
    }
}
