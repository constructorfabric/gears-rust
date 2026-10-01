//! Content validation shared by `validate` and `activate`, so a draft that
//! validates is a draft that activates.
//!
//! Version level: content limits and unique document names. Per document: the
//! content compiles under the Rego backend (source, AST and rule-dependency
//! guards) and defines the `deny` entrypoint, references no denylisted
//! builtin, lists at least one resource type, every resource type is valid GTS
//! pattern syntax and every concrete one is known to the types registry.
//! Every failure is reported against its document; no screen stops the
//! others, except that an oversized document is never handed to the backend.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gts::GtsIdPattern;
use policy_engine_sdk::POLICY_ENTRYPOINT;
use policy_engine_sdk::management::{ValidationFinding, finding};
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::{CompileError, EvaluationBackend, RegoBackend, screen_denylist};

use crate::domain::model::{ContentLimits, Document, LimitKind, LimitViolation};
use crate::domain::ports::{PortError, TypeCatalogPort};

/// One validation problem.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The document concerned; `None` for version-level findings.
    pub document_name: Option<String>,
    /// Stable code from [`finding`].
    pub code: &'static str,
    /// Human-readable detail.
    pub message: String,
}

impl Finding {
    fn document(document: &Document, code: &'static str, message: String) -> Self {
        Self {
            document_name: Some(document.name.clone()),
            code,
            message,
        }
    }

    /// The SDK view of the finding.
    #[must_use]
    pub fn to_sdk(&self) -> ValidationFinding {
        ValidationFinding {
            document_name: self.document_name.clone(),
            code: self.code.to_owned(),
            message: self.message.clone(),
        }
    }
}

/// Validates version content against the limits, the Rego backend and the
/// types registry.
#[domain_model]
pub struct ContentValidator {
    backend: Arc<dyn EvaluationBackend>,
    types: Arc<dyn TypeCatalogPort>,
    limits: ContentLimits,
}

impl std::fmt::Debug for ContentValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentValidator")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl ContentValidator {
    /// A validator over the Rego backend.
    #[must_use]
    pub fn new(types: Arc<dyn TypeCatalogPort>, limits: ContentLimits) -> Self {
        Self {
            backend: Arc::new(RegoBackend::new()),
            types,
            limits,
        }
    }

    /// The content limits.
    #[must_use]
    pub const fn limits(&self) -> &ContentLimits {
        &self.limits
    }

    /// Every finding for `documents`; empty when the content may be activated.
    ///
    /// # Errors
    ///
    /// [`PortError`] when the types registry cannot answer; an outage is never
    /// a content finding.
    pub async fn validate(&self, documents: &[Document]) -> Result<Vec<Finding>, PortError> {
        let known = self
            .types
            .known_types(&concrete_type_identifiers(documents))
            .await?;
        Ok(self.screen(documents, &known))
    }

    fn screen(&self, documents: &[Document], known: &HashSet<String>) -> Vec<Finding> {
        let mut findings: Vec<Finding> = self
            .limits
            .check(documents)
            .iter()
            .filter(|v| v.document.is_none())
            .map(limit_finding)
            .collect();
        findings.extend(duplicate_names(documents));
        for document in documents {
            let oversized = self.limits.check_document(document);
            findings.extend(oversized.iter().map(limit_finding));
            if oversized.is_none() {
                self.screen_content(document, &mut findings);
            }
            screen_resource_types(document, known, &mut findings);
        }
        findings
    }

    fn screen_content(&self, document: &Document, findings: &mut Vec<Finding>) {
        let compiled = self
            .backend
            .validate_syntax(&document.content)
            .and_then(|()| {
                self.backend
                    .compile(&document.name, &document.content, POLICY_ENTRYPOINT)
            });
        match compiled {
            Err(err) => findings.push(compile_finding(document, &err)),
            Ok(compiled) => {
                let denied = screen_denylist(compiled.as_ref());
                if !denied.is_empty() {
                    findings.push(Finding::document(
                        document,
                        finding::DENYLISTED_BUILTIN,
                        format!(
                            "content references denylisted builtin(s): {}",
                            denied.into_iter().collect::<Vec<_>>().join(", ")
                        ),
                    ));
                }
            }
        }
    }
}

fn concrete_type_identifiers(documents: &[Document]) -> Vec<String> {
    documents
        .iter()
        .flat_map(|d| d.resource_types.iter())
        .map(|t| t.trim())
        .filter(|t| !t.ends_with('*') && GtsIdPattern::try_new(t).is_ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn limit_finding(violation: &LimitViolation) -> Finding {
    let what = match violation.kind {
        LimitKind::DocumentsPerVersion => "documents in the version",
        LimitKind::DocumentBytes => "content bytes in the document",
    };
    Finding {
        document_name: violation.document.clone(),
        code: finding::LIMIT_EXCEEDED,
        message: format!(
            "{} {what} exceeds the limit of {}",
            violation.actual, violation.limit
        ),
    }
}

fn duplicate_names(documents: &[Document]) -> Vec<Finding> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for document in documents {
        *seen.entry(document.name.as_str()).or_default() += 1;
    }
    let mut repeated: Vec<(&str, usize)> = seen.into_iter().filter(|(_, n)| *n > 1).collect();
    repeated.sort_unstable();
    repeated
        .into_iter()
        .map(|(name, count)| Finding {
            document_name: Some(name.to_owned()),
            code: finding::DUPLICATE_DOCUMENT_NAME,
            message: format!("{count} documents share the name `{name}`"),
        })
        .collect()
}

fn compile_finding(document: &Document, err: &CompileError) -> Finding {
    match err {
        CompileError::Syntax {
            message,
            line,
            column,
        } => {
            let at = match (line, column) {
                (Some(line), Some(column)) => format!(" at line {line}, column {column}"),
                (Some(line), None) => format!(" at line {line}"),
                _ => String::new(),
            };
            Finding::document(
                document,
                finding::SYNTAX_ERROR,
                format!("syntax error{at}: {message}"),
            )
        }
        CompileError::Unsupported { message } => Finding::document(
            document,
            finding::ENTRYPOINT_MISSING,
            format!("content does not define the `{POLICY_ENTRYPOINT}` rule: {message}"),
        ),
    }
}

fn screen_resource_types(
    document: &Document,
    known: &HashSet<String>,
    findings: &mut Vec<Finding>,
) {
    if document.resource_types.is_empty() {
        findings.push(Finding::document(
            document,
            finding::RESOURCE_TYPES_EMPTY,
            "the document lists no resource types, so it would apply to nothing".to_owned(),
        ));
    }
    for pattern in &document.resource_types {
        if let Err(err) = GtsIdPattern::try_new(pattern) {
            findings.push(Finding::document(
                document,
                finding::INVALID_PATTERN,
                format!("`{pattern}` is not a valid GTS pattern: {err}"),
            ));
        } else if !pattern.trim().ends_with('*') && !known.contains(pattern.trim()) {
            findings.push(Finding::document(
                document,
                finding::RESOURCE_TYPE_UNKNOWN,
                format!("resource type `{pattern}` is not known to the types registry"),
            ));
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "validation_tests.rs"]
mod validation_tests;
