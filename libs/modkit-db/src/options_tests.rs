use super::*;

#[test]
fn determine_engine_requires_engine_when_dsn_missing() {
    let cfg = DbConnConfig {
        dsn: None,
        engine: None,
        ..Default::default()
    };

    let err = determine_engine(&cfg).unwrap_err();
    assert!(matches!(err, DbError::InvalidParameter(_)));
    assert!(err.to_string().contains("Missing 'engine'"));
}

#[test]
fn determine_engine_infers_from_dsn_when_engine_missing() {
    let cfg = DbConnConfig {
        engine: None,
        dsn: Some("sqlite::memory:".to_owned()),
        ..Default::default()
    };

    let engine = determine_engine(&cfg).unwrap();
    assert_eq!(engine, DbEngineCfg::Sqlite);
}

#[test]
fn engine_and_dsn_match_ok() {
    let cases = [
        (DbEngineCfg::Postgres, "postgres://user:pass@localhost/db"),
        (DbEngineCfg::Postgres, "postgresql://user:pass@localhost/db"),
        (DbEngineCfg::Mysql, "mysql://user:pass@localhost/db"),
        (DbEngineCfg::Sqlite, "sqlite::memory:"),
        (DbEngineCfg::Sqlite, "sqlite:///tmp/test.db"),
    ];

    for (engine, dsn) in cases {
        let cfg = DbConnConfig {
            engine: Some(engine),
            dsn: Some(dsn.to_owned()),
            ..Default::default()
        };
        validate_config_consistency(&cfg).unwrap();
        assert_eq!(determine_engine(&cfg).unwrap(), engine);
    }
}

#[test]
fn engine_and_dsn_mismatch_is_error() {
    let cases = [
        (DbEngineCfg::Postgres, "mysql://user:pass@localhost/db"),
        (DbEngineCfg::Mysql, "postgres://user:pass@localhost/db"),
        (DbEngineCfg::Sqlite, "postgresql://user:pass@localhost/db"),
    ];

    for (engine, dsn) in cases {
        let cfg = DbConnConfig {
            engine: Some(engine),
            dsn: Some(dsn.to_owned()),
            ..Default::default()
        };

        let err = validate_config_consistency(&cfg).unwrap_err();
        assert!(matches!(err, DbError::ConfigConflict(_)));
    }
}

#[test]
fn unknown_dsn_is_error() {
    let cfg = DbConnConfig {
        engine: None,
        dsn: Some("unknown://localhost/db".to_owned()),
        ..Default::default()
    };

    // Consistency validation doesn't validate unknown schemes unless `engine` is set,
    // but engine determination must fail.
    validate_config_consistency(&cfg).unwrap();
    let err = determine_engine(&cfg).unwrap_err();
    assert!(matches!(err, DbError::UnknownDsn(_)));
}
