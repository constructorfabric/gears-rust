//! Public models for the github-mirror gear.
//!
//! Transport-agnostic data structures defining the contract between the
//! github-mirror gear and its consumers. All models carry `#[domain_model]`
//! so infrastructure types cannot leak into them.

use toolkit_macros::domain_model;

/// Runtime identity of the mirror gear.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorStatus {
    pub gear: String,
    pub version: String,
    pub api_base_url: String,
}

/// A mirrored GitHub repository (minimal read-slice shape).
///
/// Field set intentionally starts small — it mirrors what the first
/// read-slice (`GET /github-mirror/v1/repos`) serves from the local store
/// and grows as further entity fields are ported.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// GitHub's numeric repository id.
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
}

/// A mirrored GitHub issue (read-slice shape).
///
/// GitHub's API treats pull requests as issues too — `is_pull_request`
/// carries that distinction so consumers can filter either way.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// GitHub's numeric issue id.
    pub id: i64,
    /// Owning repository's GitHub id.
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub is_pull_request: bool,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: Option<String>,
}
