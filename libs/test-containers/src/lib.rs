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
/// This is the value `testcontainers-modules` 0.15 defaults to. It is stated
/// here explicitly so that raising the floor is a visible, single-line change
/// rather than a side effect of a dependency bump.
pub const POSTGRES_TAG: &str = "11-alpine";

/// `PostgreSQL` 19, reserved for the graph-storage gear (SQL/PGQ, `GRAPH_TABLE`).
///
/// Pre-GA: this stays a beta tag until `PostgreSQL` 19 ships (expected
/// September/October 2026 — see `docs/arch/secure-orm/ADR/0002`). Replace with
/// `19-alpine` after GA. Unused until that gear lands.
pub const POSTGRES_GRAPH_TAG: &str = "19beta3-alpine";

/// Official `mysql` image tag used by the whole workspace.
pub const MYSQL_TAG: &str = "8.1";

/// `TimescaleDB` image, which is not an official `postgres` build.
pub const TIMESCALEDB_IMAGE: &str = "timescale/timescaledb";

/// `TimescaleDB` tag. Non-OSS variant, matching the usage-collector pin.
pub const TIMESCALEDB_TAG: &str = "2.17.2-pg16";

/// `MariaDB` image, used by the outbox throughput benchmark.
pub const MARIADB_IMAGE: &str = "mariadb";

/// `MariaDB` tag.
///
/// Still the floating `lts` tag the benchmark used before this crate existed;
/// pinning it to an exact version is deliberately left to the version bump so
/// that introducing this crate changes no behaviour.
pub const MARIADB_TAG: &str = "lts";

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

    /// The constants above claim to restate the upstream defaults. If a
    /// `testcontainers-modules` bump moves one, this fails and forces someone to
    /// re-read the decision instead of silently changing the database under
    /// every test in the repository.
    #[test]
    fn default_postgres_tag_is_unchanged() {
        assert_eq!(
            Postgres::default().tag(),
            POSTGRES_TAG,
            "testcontainers-modules changed its default Postgres tag; \
             decide deliberately whether POSTGRES_TAG should follow"
        );
    }

    #[test]
    fn default_mysql_tag_is_unchanged() {
        assert_eq!(
            Mysql::default().tag(),
            MYSQL_TAG,
            "testcontainers-modules changed its default MySQL tag; \
             decide deliberately whether MYSQL_TAG should follow"
        );
    }

    /// An exported-but-empty variable must not produce `postgres:`.
    #[test]
    fn an_empty_override_falls_back_to_the_pin() {
        assert_eq!(
            tag_from_env("GEARS_TEST_ABSENT_VAR_XYZ", "fallback"),
            "fallback"
        );
    }
}
