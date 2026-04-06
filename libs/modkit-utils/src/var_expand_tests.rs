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
        assert!(matches!(&err, ExpandVarsError::Var { name, .. } if name == "EXPAND_DEF_NODEF"));
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
