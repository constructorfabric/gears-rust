//! The write precondition on a row version (DESIGN §3.3 optimistic
//! concurrency).

use toolkit_macros::domain_model;

/// The version tag of a row: its bare row version, quoted. REST sends it as
/// the strong `ETag`.
#[must_use]
pub fn version_tag(version: i64) -> String {
    format!("\"{version}\"")
}

/// What a write requires of the locked row's version.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// Unconditional write.
    None,
    /// The SDK's `expected_version`.
    Version(i64),
    /// A validator sent by the caller, compared with the row's version tag
    /// as a string.
    Tag(String),
}

impl Precondition {
    #[must_use]
    pub fn holds(&self, version: i64) -> bool {
        match self {
            Self::None => true,
            Self::Version(expected) => *expected == version,
            Self::Tag(tag) => *tag == version_tag(version),
        }
    }
}

impl From<Option<i64>> for Precondition {
    fn from(expected_version: Option<i64>) -> Self {
        expected_version.map_or(Self::None, Self::Version)
    }
}

impl From<Option<String>> for Precondition {
    fn from(tag: Option<String>) -> Self {
        tag.map_or(Self::None, Self::Tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_holds_only_as_the_exact_version_tag() {
        assert!(Precondition::Tag("\"3\"".into()).holds(3));
        for tag in ["\"03\"", "\"+3\"", "3", "W/\"3\"", "\"abc\"", "\"4\""] {
            assert!(!Precondition::Tag(tag.into()).holds(3), "{tag}");
        }
        assert!(Precondition::None.holds(3));
        assert!(Precondition::from(Some(3)).holds(3));
        assert!(!Precondition::from(Some(2)).holds(3));
    }
}
