//! Single-pass expansion of `${VAR}` and `${VAR:-default}` placeholders from environment
//! variables.

use std::sync::LazyLock;

use regex::Regex;

/// Error returned by [`expand_env_vars`].
#[derive(Debug)]
pub enum ExpandVarsError {
    /// An environment variable referenced by the input is missing or contains invalid Unicode.
    Var {
        name: String,
        source: std::env::VarError,
    },
    /// The internal regex failed to compile (should never happen with a literal pattern).
    Regex(String),
}

impl std::fmt::Display for ExpandVarsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Var { name, source } => {
                write!(f, "environment variable '{name}': {source}")
            }
            Self::Regex(msg) => write!(f, "env expansion regex error: {msg}"),
        }
    }
}

impl std::error::Error for ExpandVarsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Var { source, .. } => Some(source),
            Self::Regex(_) => None,
        }
    }
}

/// Expand `${VAR_NAME}` and `${VAR_NAME:-default}` placeholders in `input` with values
/// from the environment.
///
/// - `${VAR}` — replaced with the value of `VAR`; **errors** if the variable is not set.
/// - `${VAR:-value}` — replaced with the value of `VAR`, or `value` if the variable is
///   not set. An empty default (`${VAR:-}`) is allowed and expands to the empty string.
///   Default values must not contain `}` (nested `${…}` placeholders are not supported).
/// - If a variable is set but empty, its (empty) value is always used regardless of any
///   default.
///
/// Uses single-pass `Regex::replace_all` so that values themselves containing
/// `${...}` are **not** re-expanded.  Fails on the first unresolvable variable
/// that has no default.
///
/// # Errors
///
/// Returns [`ExpandVarsError::Var`] if a referenced environment variable is missing
/// and no default value was provided.
pub fn expand_env_vars(input: &str) -> Result<String, ExpandVarsError> {
    static RE: LazyLock<Result<Regex, String>> = LazyLock::new(|| {
        Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-((?:[^}])*))?\}").map_err(|e| e.to_string())
    });
    let re = RE.as_ref().map_err(|e| ExpandVarsError::Regex(e.clone()))?;

    let mut err: Option<ExpandVarsError> = None;
    let result = re.replace_all(input, |caps: &regex::Captures| {
        if err.is_some() {
            return String::new();
        }
        let name = &caps[1];
        match std::env::var(name) {
            Ok(val) => val,
            Err(e) => {
                if matches!(&e, std::env::VarError::NotPresent)
                    && let Some(default) = caps.get(2)
                {
                    return default.as_str().to_owned();
                }
                err = Some(ExpandVarsError::Var {
                    name: name.to_owned(),
                    source: e,
                });
                String::new()
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(result.into_owned())
}

/// Trait for types whose `String` fields can be expanded from environment variables.
///
/// Typically derived via `#[derive(ExpandVars)]` from `modkit-macros`.
/// Fields marked with `#[expand_vars]` will have `${VAR}` placeholders
/// replaced with the corresponding environment variable values.
///
/// # Errors
///
/// Returns [`ExpandVarsError`] if a referenced environment variable is missing
/// or contains invalid Unicode.
pub trait ExpandVars {
    /// Expand `${VAR}` placeholders in marked fields from environment variables.
    ///
    /// # Errors
    ///
    /// Returns [`ExpandVarsError`] if a referenced environment variable is missing
    /// or contains invalid Unicode.
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError>;
}

impl ExpandVars for String {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        *self = expand_env_vars(self)?;
        Ok(())
    }
}

impl<T: ExpandVars> ExpandVars for Option<T> {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        if let Some(inner) = self {
            inner.expand_vars()?;
        }
        Ok(())
    }
}

impl<T: ExpandVars> ExpandVars for Vec<T> {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        for item in self {
            item.expand_vars()?;
        }
        Ok(())
    }
}

impl<K, V: ExpandVars, S: std::hash::BuildHasher> ExpandVars
    for std::collections::HashMap<K, V, S>
{
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        for val in self.values_mut() {
            val.expand_vars()?;
        }
        Ok(())
    }
}

impl ExpandVars for secrecy::SecretString {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        use secrecy::ExposeSecret;
        let expanded = expand_env_vars(self.expose_secret())?;
        *self = secrecy::SecretString::from(expanded);
        Ok(())
    }
}

#[cfg(test)]
#[path = "var_expand_tests.rs"]
mod tests;
