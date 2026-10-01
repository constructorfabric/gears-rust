#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use async_trait::async_trait;
use uuid::Uuid;

use super::*;
use crate::domain::model::DocumentId;

const WIDGET: &str = "gts.cf.core.example.widget.v1~";
const UNKNOWN: &str = "gts.cf.core.example.unknown.v1~";
const GOOD: &str = "package p\ndeny := true if input.action == \"delete\"";

struct Catalog(Result<HashSet<String>, PortError>);

#[async_trait]
impl TypeCatalogPort for Catalog {
    async fn known_types(&self, ids: &[String]) -> Result<HashSet<String>, PortError> {
        self.0
            .clone()
            .map(|known| ids.iter().filter(|i| known.contains(*i)).cloned().collect())
    }
}

fn validator(catalog: Catalog) -> ContentValidator {
    ContentValidator::new(
        Arc::new(catalog),
        ContentLimits {
            max_documents_per_version: 3,
            max_document_bytes: 200,
        },
    )
}

fn widget_catalog() -> Catalog {
    Catalog(Ok(HashSet::from([WIDGET.to_owned()])))
}

fn doc(name: &str, content: &str, types: &[&str]) -> Document {
    Document {
        id: DocumentId(Uuid::new_v4()),
        name: name.to_owned(),
        content: content.to_owned(),
        resource_types: types.iter().map(|t| (*t).to_owned()).collect(),
        actions: Vec::new(),
    }
}

async fn codes(documents: &[Document]) -> Vec<&'static str> {
    validator(widget_catalog())
        .validate(documents)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.code)
        .collect()
}

#[tokio::test]
async fn valid_content_has_no_findings() {
    let documents = [
        doc("a", GOOD, &[WIDGET]),
        doc("b", GOOD, &["gts.cf.core.example.*"]),
    ];
    assert!(codes(&documents).await.is_empty());
}

#[tokio::test]
async fn content_problems_are_reported_per_document() {
    assert_eq!(
        codes(&[doc("a", "package p\ndeny :=", &[WIDGET])]).await,
        [finding::SYNTAX_ERROR]
    );
    assert_eq!(
        codes(&[doc("a", "package p\nallow := true", &[WIDGET])]).await,
        [finding::ENTRYPOINT_MISSING]
    );
    assert_eq!(
        codes(&[doc(
            "a",
            "package p\ndeny := true if time.now_ns() > 0",
            &[WIDGET]
        )])
        .await,
        [finding::DENYLISTED_BUILTIN]
    );
}

#[tokio::test]
async fn resource_type_problems_are_reported() {
    assert_eq!(
        codes(&[doc("a", GOOD, &[])]).await,
        [finding::RESOURCE_TYPES_EMPTY]
    );
    assert_eq!(
        codes(&[doc("a", GOOD, &[UNKNOWN])]).await,
        [finding::RESOURCE_TYPE_UNKNOWN]
    );
    assert_eq!(
        codes(&[doc("a", GOOD, &["not a pattern"])]).await,
        [finding::INVALID_PATTERN]
    );
}

#[tokio::test]
async fn version_level_problems_are_reported() {
    assert_eq!(
        codes(&[doc("a", GOOD, &[WIDGET]), doc("a", GOOD, &[WIDGET])]).await,
        [finding::DUPLICATE_DOCUMENT_NAME]
    );
    let many: Vec<_> = (0..4)
        .map(|n| doc(&format!("d{n}"), GOOD, &[WIDGET]))
        .collect();
    assert_eq!(codes(&many).await, [finding::LIMIT_EXCEEDED]);
    let big = "x".repeat(300);
    assert_eq!(
        codes(&[doc("a", &big, &[WIDGET])]).await,
        [finding::LIMIT_EXCEEDED],
        "an oversized document is reported, not compiled"
    );
}

#[tokio::test]
async fn a_registry_outage_is_an_error_not_a_finding() {
    let outage = Catalog(Err(PortError::Timeout));
    let result = validator(outage)
        .validate(&[doc("a", GOOD, &[WIDGET])])
        .await;
    assert_eq!(result, Err(PortError::Timeout));
}
