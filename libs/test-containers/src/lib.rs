//! Single source of truth for the database container images used by the
//! workspace integration tests. Change database versions here and nowhere else.
//!
//! # Why this crate exists
//!
//! Nearly every fixture in the workspace used to call `Postgres::default()` or
//! `Mysql::default()`, whose tags are hardcoded inside `testcontainers-modules`.
//! That had three consequences: the tests ran against EOL database versions, the
//! version was pinned only transitively through `Cargo.lock` — so bumping
//! `testcontainers-modules` silently changed the database under every test in
//! the repository — and pins had begun to diverge, with three different
//! `PostgreSQL` versions in one repo.
//!
//! Every fixture now goes through the helpers below, so a version change is one
//! edit in one file. [`default_postgres_tag_is_unchanged`] fails if the upstream
//! default moves, which forces whoever bumps the dependency to re-read this.
//!
//! # Overrides
//!
//! Each tag can be overridden by an environment variable, so CI can run a
//! version matrix without touching code:
//!
//! | Helper | Variable |
//! |---|---|
//! | [`postgres_tag`] | `GEARS_TEST_PG_TAG` |
//! | [`postgres_graph_tag`] | `GEARS_TEST_PG_GRAPH_TAG` |
//! | [`mysql_tag`] | `GEARS_TEST_MYSQL_TAG` |

use std::env;

use testcontainers::core::ContainerRequest;
use testcontainers::{GenericImage, ImageExt as _};
use testcontainers_modules::mysql::Mysql;
use testcontainers_modules::postgres::Postgres;

/// Official `postgres` image tag used by the whole workspace.
///
/// `PostgreSQL` 18 is the floor. `testcontainers-modules` still defaults to
/// `11-alpine`, which has been EOL since November 2023; that default is
/// asserted by [`tests::default_postgres_tag_is_unchanged`] so a dependency
/// bump cannot move the database under the tests without someone noticing.
pub const POSTGRES_TAG: &str = "18-alpine";

/// `PostgreSQL` 19, reserved for the graph-storage gear (SQL/PGQ, `GRAPH_TABLE`).
///
/// Pre-GA: this stays a beta tag until `PostgreSQL` 19 ships (expected
/// September/October 2026 — see `docs/arch/secure-orm/ADR/0002`). Replace with
/// `19-alpine` after GA. Unused until that gear lands.
pub const POSTGRES_GRAPH_TAG: &str = "19beta3-alpine";

/// Official `mysql` image tag used by the whole workspace.
///
/// Upstream defaults to `8.1`, which is EOL.
pub const MYSQL_TAG: &str = "9.7";

/// `TimescaleDB` image, which is not an official `postgres` build.
pub const TIMESCALEDB_IMAGE: &str = "timescale/timescaledb";

/// `TimescaleDB` tag, on the same `PostgreSQL` major as the floor above.
pub const TIMESCALEDB_TAG: &str = "2.29.2-pg18";

/// `MariaDB` image, used by the outbox throughput benchmark.
pub const MARIADB_IMAGE: &str = "mariadb";

/// `MariaDB` tag.
///
/// An exact version rather than the floating `lts` the outbox benchmark used
/// before: a moving tag means a benchmark can change what it measures without
/// any commit.
pub const MARIADB_TAG: &str = "11.8";

/// Postgres tag in effect, honouring `GEARS_TEST_PG_TAG`.
#[must_use]
pub fn postgres_tag() -> String {
    tag_from_env("GEARS_TEST_PG_TAG", POSTGRES_TAG)
}

/// Graph-lane Postgres tag in effect, honouring `GEARS_TEST_PG_GRAPH_TAG`.
#[must_use]
pub fn postgres_graph_tag() -> String {
    tag_from_env("GEARS_TEST_PG_GRAPH_TAG", POSTGRES_GRAPH_TAG)
}

/// `MySQL` tag in effect, honouring `GEARS_TEST_MYSQL_TAG`.
#[must_use]
pub fn mysql_tag() -> String {
    tag_from_env("GEARS_TEST_MYSQL_TAG", MYSQL_TAG)
}

/// An empty or whitespace-only variable is treated as unset: a CI job that
/// exports the name without a value should get the pin, not an image called
/// `postgres:`.
fn tag_from_env(var: &str, fallback: &str) -> String {
    match env::var(var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => fallback.to_owned(),
    }
}

/// A Postgres container request on the pinned tag.
///
/// Add environment variables and wait strategies on the result as usual; this
/// only fixes the image.
pub fn postgres() -> ContainerRequest<Postgres> {
    ContainerRequest::from(Postgres::default()).with_tag(postgres_tag())
}

/// A Postgres container request whose database is named `db_name`.
///
/// `with_db_name` lives on the image rather than on the request, so a fixture
/// that needs it cannot start from [`postgres`]. It is offered here rather than
/// as a generic "configure the image yourself" hook on purpose: the point of
/// this crate is that no fixture outside it names an image constructor, and a
/// hook taking a `Postgres` would put one back in every caller.
pub fn postgres_with_db(db_name: &str) -> ContainerRequest<Postgres> {
    ContainerRequest::from(Postgres::default().with_db_name(db_name)).with_tag(postgres_tag())
}

/// A `PostgreSQL` 19 container request, for the graph lane only.
///
/// See [`graph_lane_required`] for how an unavailable image should be handled.
pub fn postgres_graph() -> ContainerRequest<Postgres> {
    ContainerRequest::from(Postgres::default()).with_tag(postgres_graph_tag())
}

/// A `MySQL` container request on the pinned tag.
pub fn mysql() -> ContainerRequest<Mysql> {
    ContainerRequest::from(Mysql::default()).with_tag(mysql_tag())
}

/// A `TimescaleDB` image on the pinned tag.
///
/// Returns a bare [`GenericImage`] rather than a request, because the caller
/// adds its own wait strategy.
pub fn timescaledb() -> GenericImage {
    GenericImage::new(TIMESCALEDB_IMAGE, TIMESCALEDB_TAG)
}

/// A `MariaDB` image on the pinned tag.
pub fn mariadb() -> GenericImage {
    GenericImage::new(MARIADB_IMAGE, MARIADB_TAG)
}

/// Whether an unavailable graph-lane image is a failure or a skip.
///
/// While `PostgreSQL` 19 is pre-GA the default is `false`, so a machine without
/// that image skips those tests gracefully. `GEARS_TEST_PG_GRAPH_REQUIRED=1`
/// turns the same situation into a failure — the same shape as
/// `RG_PG_REQUIRE_DOCKER` in resource-group, so that a CI lane which is
/// supposed to cover PG19 cannot pass vacuously.
#[must_use]
pub fn graph_lane_required() -> bool {
    matches!(
        env::var("GEARS_TEST_PG_GRAPH_REQUIRED").as_deref(),
        Ok("1" | "true" | "TRUE")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::Image as _;

    /// The workspace pins deliberately differ from the upstream defaults, which
    /// are EOL. Pinning the *default* too means a `testcontainers-modules` bump
    /// that changes it fails here and forces a decision, instead of quietly
    /// altering what every fixture in the repository runs against.
    #[test]
    fn default_postgres_tag_is_unchanged() {
        assert_eq!(
            Postgres::default().tag(),
            "11-alpine",
            "testcontainers-modules changed its default Postgres tag; \
             re-read libs/test-containers and decide whether POSTGRES_TAG follows"
        );
    }

    #[test]
    fn default_mysql_tag_is_unchanged() {
        assert_eq!(
            Mysql::default().tag(),
            "8.1",
            "testcontainers-modules changed its default MySQL tag; \
             re-read libs/test-containers and decide whether MYSQL_TAG follows"
        );
    }

    /// The pins must actually be ahead of those defaults, or this crate is
    /// documentation rather than a floor.
    #[test]
    fn the_pins_are_not_the_eol_defaults() {
        assert_ne!(POSTGRES_TAG, Postgres::default().tag());
        assert_ne!(MYSQL_TAG, Mysql::default().tag());
    }

    /// An exported-but-empty variable must not produce `postgres:`. Set for
    /// real (empty, then whitespace-only) rather than merely absent, so the
    /// `Ok(v)` guard is what the test exercises — an absent variable never
    /// reaches it.
    #[test]
    fn an_empty_override_falls_back_to_the_pin() {
        temp_env::with_var("GEARS_TEST_EMPTY_VAR_XYZ", Some(""), || {
            assert_eq!(
                tag_from_env("GEARS_TEST_EMPTY_VAR_XYZ", "fallback"),
                "fallback"
            );
        });
        temp_env::with_var("GEARS_TEST_EMPTY_VAR_XYZ", Some("   "), || {
            assert_eq!(
                tag_from_env("GEARS_TEST_EMPTY_VAR_XYZ", "fallback"),
                "fallback"
            );
        });
    }

    /// And the unset case still falls back, via the `Err` arm.
    #[test]
    fn an_absent_override_falls_back_to_the_pin() {
        temp_env::with_var_unset("GEARS_TEST_ABSENT_VAR_XYZ", || {
            assert_eq!(
                tag_from_env("GEARS_TEST_ABSENT_VAR_XYZ", "fallback"),
                "fallback"
            );
        });
    }

    /// A set, non-empty variable wins over the pin.
    #[test]
    fn a_real_override_replaces_the_pin() {
        temp_env::with_var("GEARS_TEST_SET_VAR_XYZ", Some("20-alpine"), || {
            assert_eq!(
                tag_from_env("GEARS_TEST_SET_VAR_XYZ", "fallback"),
                "20-alpine"
            );
        });
    }
}
