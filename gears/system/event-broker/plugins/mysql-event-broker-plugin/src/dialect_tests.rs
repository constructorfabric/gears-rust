use crate::dialect::{Dialect, Durability, Topology, Version};

fn v(major: u32, minor: u32, patch: u32) -> Version {
    Version {
        major,
        minor,
        patch,
    }
}

#[test]
fn versions_parse_for_both_engines() {
    assert_eq!(
        [
            "8.0.39",
            "8.4.3-log",
            "9.7.0",
            "11.8.2-MariaDB-ubu2404",
            "10.6.21-MariaDB-log",
            "5.5.5-10.6.21-MariaDB",
            "garbage",
            "8",
        ]
        .map(Dialect::parse),
        [
            Some(Dialect::Mysql(v(8, 0, 39))),
            Some(Dialect::Mysql(v(8, 4, 3))),
            Some(Dialect::Mysql(v(9, 7, 0))),
            Some(Dialect::Mariadb(v(11, 8, 2))),
            Some(Dialect::Mariadb(v(10, 6, 21))),
            Some(Dialect::Mariadb(v(10, 6, 21))),
            None,
            None,
        ]
    );
}

#[test]
fn each_engine_has_its_own_floor() {
    assert_eq!(
        [
            Dialect::Mysql(v(8, 0, 0)),
            Dialect::Mysql(v(5, 7, 44)),
            Dialect::Mariadb(v(10, 6, 0)),
            Dialect::Mariadb(v(10, 5, 27)),
            Dialect::Mariadb(v(11, 8, 2)),
        ]
        .map(Dialect::is_supported),
        [true, false, true, false, true]
    );
    assert_eq!(
        Dialect::Mariadb(v(10, 5, 27)).to_string(),
        "MariaDB 10.5.27"
    );
}

#[test]
fn the_isolation_variable_differs() {
    assert_eq!(
        (
            Dialect::Mysql(v(8, 4, 0)).isolation_query(),
            Dialect::Mariadb(v(10, 6, 0)).isolation_query()
        ),
        (
            "SELECT @@SESSION.transaction_isolation",
            "SELECT @@SESSION.tx_isolation"
        )
    );
}

fn durability(flush: i64, log_bin: bool, sync_binlog: i64, format: &str) -> Durability {
    Durability {
        flush_log_at_trx_commit: flush,
        log_bin,
        sync_binlog,
        binlog_format: format.to_owned(),
    }
}

#[test]
fn durability_needs_the_redo_log_and_any_binary_log_flushed() {
    assert_eq!(
        [
            durability(1, true, 1, "ROW"),
            durability(1, false, 0, "MIXED"),
            durability(2, true, 1, "ROW"),
            durability(1, true, 0, "ROW"),
            durability(0, true, 100, "ROW"),
        ]
        .map(|d| d.shortfalls(MYSQL)),
        [
            vec![],
            vec![],
            vec!["innodb_flush_log_at_trx_commit is not 1"],
            vec!["the binary log is on and sync_binlog is not 1"],
            vec![
                "innodb_flush_log_at_trx_commit is not 1",
                "the binary log is on and sync_binlog is not 1"
            ],
        ]
    );
}

#[test]
fn a_statement_based_binary_log_refuses_read_committed() {
    assert_eq!(
        [
            durability(1, true, 1, "STATEMENT"),
            durability(1, false, 0, "STATEMENT"),
            durability(1, true, 1, "MIXED"),
            durability(1, true, 1, "ROW"),
        ]
        .map(|d| d.refuses_read_committed()),
        [true, false, false, false]
    );
}

const MYSQL: Dialect = Dialect::Mysql(Version {
    major: 8,
    minor: 4,
    patch: 0,
});
const MARIADB: Dialect = Dialect::Mariadb(Version {
    major: 10,
    minor: 6,
    patch: 0,
});

#[test]
fn a_flush_at_prepare_and_commit_is_durable_where_the_engine_says_so() {
    assert_eq!(
        [
            durability(3, false, 0, "ROW").shortfalls(MARIADB),
            durability(3, false, 0, "ROW").shortfalls(MYSQL),
            durability(2, false, 0, "ROW").shortfalls(MARIADB),
        ],
        [
            vec![],
            vec!["innodb_flush_log_at_trx_commit is not 1"],
            vec!["innodb_flush_log_at_trx_commit is neither 1 nor 3"],
        ]
    );
}

#[test]
fn only_a_single_primary_topology_is_accepted() {
    let topology = |wsrep_on: Option<&str>, single_primary: Option<&str>| Topology {
        wsrep_on: wsrep_on.map(str::to_owned),
        group_replication_single_primary: single_primary.map(str::to_owned),
    };
    assert_eq!(
        [
            topology(None, None).refusal(),
            topology(Some("OFF"), None).refusal(),
            topology(Some("ON"), None).refusal(),
            topology(None, Some("ON")).refusal(),
            topology(None, Some("OFF")).refusal(),
        ],
        [
            None,
            None,
            Some(
                "Galera (wsrep_on is ON): named locks are per node, so maintenance on two nodes would collide; single-primary servers only"
            ),
            None,
            Some(
                "multi-primary Group Replication (group_replication_single_primary_mode is OFF): named locks are per member; single-primary servers only"
            ),
        ]
    );
}
