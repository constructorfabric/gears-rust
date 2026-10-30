use toolkit::domain_model;

/// A backend registry entry's name: the key it is written under in `backends`,
/// and the value a `backend_routing` entry and a resolved topic carry
/// (feature 0005 §2.1).
///
/// Follows the rule cluster profile names follow, `[a-zA-Z0-9_-]{1,255}`, so
/// it is safe to reproduce in an error or a log line: nothing that parses as a
/// name can carry an address, a credential or a newline.
#[domain_model]
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct Name(String);

/// A string that is not a valid [`Name`]. Carries nothing of the input, which
/// may be anything an operator wrote.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a backend name is 1 to 255 characters of [a-zA-Z0-9_-]")]
pub struct InvalidName;

impl Name {
    /// # Errors
    /// [`InvalidName`] when `raw` breaks the rule.
    pub fn parse(raw: &str) -> Result<Self, InvalidName> {
        if cluster_sdk::is_valid_cluster_name(raw) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(InvalidName)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Name {
    type Error = InvalidName;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl From<Name> for String {
    fn from(name: Name) -> Self {
        name.0
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
