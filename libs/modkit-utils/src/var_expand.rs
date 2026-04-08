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
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_no_placeholders() {
        let result = expand_env_vars("plain string without vars").unwrap();
        assert_eq!(result, "plain string without vars");
    }

    #[test]
    fn single_variable() {
        temp_env::with_vars([("EXPAND_SINGLE", Some("replaced"))], || {
            let result = expand_env_vars("prefix_${EXPAND_SINGLE}_suffix").unwrap();
            assert_eq!(result, "prefix_replaced_suffix");
        });
    }

    #[test]
    fn multiple_variables() {
        temp_env::with_vars(
            [
                ("EXPAND_HOST", Some("localhost")),
                ("EXPAND_PORT", Some("5432")),
            ],
            || {
                let result = expand_env_vars("${EXPAND_HOST}:${EXPAND_PORT}").unwrap();
                assert_eq!(result, "localhost:5432");
            },
        );
    }

    #[test]
    fn missing_var_returns_error_with_name() {
        temp_env::with_vars([("EXPAND_MISSING_CANARY", None::<&str>)], || {
            let err = expand_env_vars("${EXPAND_MISSING_CANARY}").unwrap_err();
            assert!(
                matches!(&err, ExpandVarsError::Var { name, .. } if name == "EXPAND_MISSING_CANARY")
            );
            let msg = err.to_string();
            assert!(
                msg.contains("EXPAND_MISSING_CANARY"),
                "error should contain var name, got: {msg}"
            );
        });
    }

    #[test]
    fn fails_on_first_missing_variable() {
        temp_env::with_vars(
            [
                ("EXPAND_FIRST_MISS", None::<&str>),
                ("EXPAND_SECOND_OK", Some("present")),
            ],
            || {
                let err = expand_env_vars("${EXPAND_FIRST_MISS}_${EXPAND_SECOND_OK}").unwrap_err();
                assert!(
                    matches!(&err, ExpandVarsError::Var { name, .. } if name == "EXPAND_FIRST_MISS")
                );
            },
        );
    }

    #[test]
    fn default_value_used_when_var_missing() {
        temp_env::with_vars([("EXPAND_DEF_MISS", None::<&str>)], || {
            let result = expand_env_vars("${EXPAND_DEF_MISS:-8080}").unwrap();
            assert_eq!(result, "8080");
        });
    }

    #[test]
    fn empty_default_expands_to_empty_string() {
        temp_env::with_vars([("EXPAND_DEF_EMPTY", None::<&str>)], || {
            let result = expand_env_vars("prefix_${EXPAND_DEF_EMPTY:-}_suffix").unwrap();
            assert_eq!(result, "prefix__suffix");
        });
    }

    #[test]
    fn default_ignored_when_var_is_set() {
        temp_env::with_vars([("EXPAND_DEF_SET", Some("actual"))], || {
            let result = expand_env_vars("${EXPAND_DEF_SET:-fallback}").unwrap();
            assert_eq!(result, "actual");
        });
    }

    #[test]
    fn empty_var_uses_empty_value_not_default() {
        temp_env::with_vars([("EXPAND_DEF_EMPTYVAL", Some(""))], || {
            let result = expand_env_vars("${EXPAND_DEF_EMPTYVAL:-fallback}").unwrap();
            assert_eq!(result, "");
        });
    }

    #[test]
    fn no_default_still_errors_on_missing() {
        temp_env::with_vars([("EXPAND_DEF_NODEF", None::<&str>)], || {
            let err = expand_env_vars("${EXPAND_DEF_NODEF}").unwrap_err();
            assert!(
                matches!(&err, ExpandVarsError::Var { name, .. } if name == "EXPAND_DEF_NODEF")
            );
        });
    }

    #[test]
    fn multiple_defaults_in_one_string() {
        temp_env::with_vars(
            [
                ("EXPAND_MULTI_A", None::<&str>),
                ("EXPAND_MULTI_B", Some("set")),
            ],
            || {
                let result =
                    expand_env_vars("${EXPAND_MULTI_A:-alpha}_${EXPAND_MULTI_B:-beta}").unwrap();
                assert_eq!(result, "alpha_set");
            },
        );
    }

    /// Regression: values containing `${...}` must not be re-expanded.
    /// Input `${A}_${B}` with A=`${B}` and B=`val` must yield `${B}_val`, not `val_val`.
    #[test]
    fn no_double_expansion() {
        temp_env::with_vars(
            [
                ("EXPAND_TEST_A", Some("${EXPAND_TEST_B}")),
                ("EXPAND_TEST_B", Some("val")),
            ],
            || {
                let result = expand_env_vars("${EXPAND_TEST_A}_${EXPAND_TEST_B}").unwrap();
                assert_eq!(result, "${EXPAND_TEST_B}_val");
            },
        );
    }
}
