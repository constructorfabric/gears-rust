//! Single source of truth for the database container images this workspace's
//! integration tests start.
//!
//! Every fixture that brings up a database goes through this crate, so a
//! version change is one edit here instead of a grep across ~240 call sites
//! (<https://github.com/constructorfabric/gears-rust/issues/4616>).
//!
//! # Why not `Postgres::default()`
//!
//! `testcontainers-modules` hardcodes its own tags — `postgres:11-alpine` and
//! `mysql:8.1`, both long past end of life. Calling the default constructor
//! pins the database version transitively, through `Cargo.lock`: bumping
//! `testcontainers-modules` then changes the database under every test in the
//! repository with nothing in the diff to show it. [`postgres()`] and
//! [`mysql()`] delegate to those same images but override the tag with the
//! constants below, so the version is stated in one place and reviewed like
//! any other change.
//!
//! # Overrides
//!
//! Each tag can be overridden by an environment variable, so CI can run a
//! version matrix without touching code:
//!
//! | Variable | Overrides |
//! |---|---|
//! | `GEARS_TEST_PG_TAG` | [`POSTGRES_TAG`] |
//! | `GEARS_TEST_PG_GRAPH_TAG` | [`POSTGRES_GRAPH_TAG`] |
//! | `GEARS_TEST_MYSQL_TAG` | [`MYSQL_TAG`] |
//! | `GEARS_TEST_TIMESCALEDB_TAG` | [`TIMESCALEDB_TAG`] |
//! | `GEARS_TEST_MARIADB_TAG` | [`MARIADB_TAG`] |
//!
//! An unset *or empty* variable means "use the constant".
//!
//! Only concrete tags belong in the constants below. A floating alias such as
//! `lts` or `latest` re-points under CI with nothing in the diff to show it,
//! which is the same silent drift as `Postgres::default()`.
//!
//! # Usage
//!
//! Callers keep their own `testcontainers` dev-dependency for the runner and
//! extension traits; this crate deliberately does not re-export them, so the
//! banned `Postgres::default()` stays out of reach through `test_containers::`.
//!
//! ```no_run
//! use testcontainers::runners::AsyncRunner;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Bare, on the pinned tag:
//! let container = test_containers::postgres().start().await?;
//!
//! // Extra settings chain on as usual — the helper returns a
//! // `ContainerRequest`, which `ImageExt` is implemented for:
//! use testcontainers::ImageExt;
//! let container = test_containers::postgres()
//!     .with_env_var("POSTGRES_DB", "app")
//!     .start()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use testcontainers::{ContainerRequest, GenericImage, ImageExt};
use testcontainers_modules::{mysql::Mysql, postgres::Postgres};

/// Tag of the official `postgres` image. The workspace floor.
pub const POSTGRES_TAG: &str = "18-alpine";

/// Tag of the `postgres` image for the future graph-storage gear, which needs
/// `SQL/PGQ` (`GRAPH_TABLE`) — `PostgreSQL` 19 only.
///
/// Reserved: nothing consumes this yet. Pre-`GA`, so it names a beta build;
/// replace it with `19-alpine` once `PostgreSQL` 19 ships (expected
/// September/October 2026, see `docs/arch/secure-orm/ADR/0002`). Beta tags are
/// routinely withdrawn from Docker Hub after `GA`, which is why
/// [`graph_lane_required()`] defaults to `false`.
pub const POSTGRES_GRAPH_TAG: &str = "19beta3-alpine";

/// Tag of the official `mysql` image.
///
/// 9.7 was verified to still emit both literals
/// `testcontainers-modules`' `Mysql::ready_conditions()` waits on
/// (`X Plugin ready for connections. Bind-address` and
/// `/usr/sbin/mysqld: ready for connections.`) — a reword there would hang
/// every container to its timeout rather than fail fast.
pub const MYSQL_TAG: &str = "9.7";

/// Repository of the `TimescaleDB` image (not on Docker Hub's official list,
/// so the name is spelled out rather than derived from an image module).
pub const TIMESCALEDB_IMAGE: &str = "timescale/timescaledb";

/// Tag of the `TimescaleDB` image.
///
/// Keep in sync with `TimescaleDbSidecar.IMAGE` in
/// `testing/e2e/lib/sidecars.py`: a skew means the plugin's migrations are
/// validated against a different `PostgreSQL` major than E2E runs.
pub const TIMESCALEDB_TAG: &str = "2.29.2-pg18";

/// Repository of the `MariaDB` image.
pub const MARIADB_IMAGE: &str = "mariadb";

/// Tag of the `MariaDB` image.
///
/// A concrete version, not the `lts` alias this replaced: an alias re-points
/// across major versions on any pull, so the bench would silently change
/// engines with nothing in the diff.
pub const MARIADB_TAG: &str = "11.8";

/// Environment variable overriding [`POSTGRES_TAG`].
pub const ENV_POSTGRES_TAG: &str = "GEARS_TEST_PG_TAG";
/// Environment variable overriding [`POSTGRES_GRAPH_TAG`].
pub const ENV_POSTGRES_GRAPH_TAG: &str = "GEARS_TEST_PG_GRAPH_TAG";
/// Environment variable overriding [`MYSQL_TAG`].
pub const ENV_MYSQL_TAG: &str = "GEARS_TEST_MYSQL_TAG";
/// Environment variable overriding [`TIMESCALEDB_TAG`].
pub const ENV_TIMESCALEDB_TAG: &str = "GEARS_TEST_TIMESCALEDB_TAG";
/// Environment variable overriding [`MARIADB_TAG`].
pub const ENV_MARIADB_TAG: &str = "GEARS_TEST_MARIADB_TAG";

/// Environment variable turning an unavailable `PostgreSQL` 19 image into a
/// failure instead of a skip. See [`graph_lane_required()`].
pub const ENV_GRAPH_LANE_REQUIRED: &str = "GEARS_TEST_PG_GRAPH_REQUIRED";

/// Resolves an override against a default. Split out from the public tag
/// accessors so the precedence rule is unit-testable without mutating the
/// process environment (`std::env::set_var` is `unsafe` on edition 2024, and
/// this workspace forbids `unsafe_code`).
fn tag_from(override_value: Option<String>, default: &str) -> String {
    override_value.unwrap_or_else(|| default.to_owned())
}

/// Reads `var`, treating unset and empty alike: both yield `None`, so the
/// caller falls back to the pinned constant.
///
/// A value that is set but not valid Unicode is a mistake worth surfacing
/// rather than absorbing — silently falling back would run the suite against a
/// different image than the operator asked for, which is the failure mode this
/// crate exists to end.
fn env_override(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(value) if value.is_empty() => None,
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(raw)) => {
            panic!(
                "{var} is set to a non-UTF-8 value ({}); expected a container tag",
                raw.display()
            )
        }
    }
}

/// `PostgreSQL` tag in effect, honoring `GEARS_TEST_PG_TAG`.
#[must_use]
pub fn postgres_tag() -> String {
    tag_from(env_override(ENV_POSTGRES_TAG), POSTGRES_TAG)
}

/// Graph-lane `PostgreSQL` tag in effect, honoring `GEARS_TEST_PG_GRAPH_TAG`.
#[must_use]
pub fn postgres_graph_tag() -> String {
    tag_from(env_override(ENV_POSTGRES_GRAPH_TAG), POSTGRES_GRAPH_TAG)
}

/// `MySQL` tag in effect, honoring `GEARS_TEST_MYSQL_TAG`.
#[must_use]
pub fn mysql_tag() -> String {
    tag_from(env_override(ENV_MYSQL_TAG), MYSQL_TAG)
}

/// `TimescaleDB` tag in effect, honoring `GEARS_TEST_TIMESCALEDB_TAG`.
#[must_use]
pub fn timescaledb_tag() -> String {
    tag_from(env_override(ENV_TIMESCALEDB_TAG), TIMESCALEDB_TAG)
}

/// `MariaDB` tag in effect, honoring `GEARS_TEST_MARIADB_TAG`.
#[must_use]
pub fn mariadb_tag() -> String {
    tag_from(env_override(ENV_MARIADB_TAG), MARIADB_TAG)
}

/// A `PostgreSQL` container request on the pinned tag.
///
/// Chain `ImageExt` methods (`with_env_var`, `with_mount`, …) onto the result
/// exactly as you would onto `Postgres::default()`.
pub fn postgres() -> ContainerRequest<Postgres> {
    ContainerRequest::from(Postgres::default()).with_tag(postgres_tag())
}

/// A `PostgreSQL` container request on the pinned tag, with a non-default
/// database name.
///
/// `POSTGRES_DB` is an image-level setting on `Postgres`, so it has to be
/// applied before the tag override converts the image into a
/// `ContainerRequest` — which is why this exists rather than callers chaining
/// `with_db_name` onto [`postgres()`].
pub fn postgres_named(db_name: &str) -> ContainerRequest<Postgres> {
    ContainerRequest::from(Postgres::default().with_db_name(db_name)).with_tag(postgres_tag())
}

/// A `PostgreSQL` container request for a suite whose migrations need a higher
/// floor than [`POSTGRES_TAG`].
///
/// `fallback` applies only when `GEARS_TEST_PG_TAG` is unset, so a CI version
/// matrix still reaches the suite — unlike chaining `.with_tag("16-alpine")`
/// onto [`postgres()`], which discards the resolved tag and makes the suite
/// invisible to the override. Delete the call once [`POSTGRES_TAG`] clears the
/// floor the caller needs.
///
/// No caller today: resource-group's PG13 floor was the last one, and the
/// workspace floor cleared it at PG 18. Kept as the escape hatch for the next
/// suite that outruns the floor, so the answer isn't a local `.with_tag(...)`
/// that silently opts out of the override.
pub fn postgres_tagged(fallback: &str) -> ContainerRequest<Postgres> {
    let tag = tag_from(env_override(ENV_POSTGRES_TAG), fallback);
    ContainerRequest::from(Postgres::default()).with_tag(tag)
}

/// A `PostgreSQL` 19 container request, for the graph-storage gear's `SQL/PGQ`
/// suite. Reserved — nothing consumes it yet; see [`POSTGRES_GRAPH_TAG`].
pub fn postgres_graph() -> ContainerRequest<Postgres> {
    ContainerRequest::from(Postgres::default()).with_tag(postgres_graph_tag())
}

/// A `MySQL` container request on the pinned tag.
pub fn mysql() -> ContainerRequest<Mysql> {
    ContainerRequest::from(Mysql::default()).with_tag(mysql_tag())
}

/// A `TimescaleDB` image on the pinned tag.
///
/// Unlike [`postgres()`] and [`mysql()`] there is no image module for this
/// one, so the caller still supplies the wait strategy and environment.
pub fn timescaledb() -> GenericImage {
    // Both parameters share one generic type, so the name is owned too.
    GenericImage::new(TIMESCALEDB_IMAGE.to_owned(), timescaledb_tag())
}

/// A `MariaDB` image on the pinned tag. Same caveat as [`timescaledb()`].
pub fn mariadb() -> GenericImage {
    GenericImage::new(MARIADB_IMAGE.to_owned(), mariadb_tag())
}

/// Whether an unavailable `PostgreSQL` 19 image must fail the run rather than
/// skip it.
///
/// Defaults to `false` while the image is pre-`GA`: the tag can disappear from
/// Docker Hub at `GA`, and no lane should go red for that. `CI` sets
/// `GEARS_TEST_PG_GRAPH_REQUIRED=1` once the lane is meant to be mandatory —
/// the same shape as `RG_PG_REQUIRE_DOCKER` in resource-group's smoke test.
#[must_use]
pub fn graph_lane_required() -> bool {
    flag_from(env_override(ENV_GRAPH_LANE_REQUIRED))
}

/// Truthiness rule behind [`graph_lane_required()`].
///
/// Unset, empty and the usual falsy spellings all mean "off". Reading
/// `GEARS_TEST_PG_GRAPH_REQUIRED=false` as *on* would invert the documented
/// default, so the negatives are matched explicitly rather than by "anything
/// that is not `0`".
fn flag_from(value: Option<String>) -> bool {
    value.is_some_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off" | "no"
        )
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serial_test::serial;
    use testcontainers::Image;

    // Tests that read or write `GEARS_TEST_*` are `#[serial]`: `temp_env`
    // mutates real process environment, which is shared by every test thread
    // in this binary, so running them concurrently makes the tag a race.

    /// Canary, not a preference: it asserts what `testcontainers-modules`
    /// currently defaults to, which is exactly what this crate exists to stop
    /// anyone from inheriting silently.
    ///
    /// If this fails, the dependency changed its built-in tags. That is not a
    /// reason to update the literals below and move on — re-read
    /// <https://github.com/constructorfabric/gears-rust/issues/4616>, check
    /// whether the new upstream default is above or below our floor, and
    /// decide deliberately.
    #[test]
    fn upstream_defaults_are_still_the_eol_images_we_override() {
        assert_eq!(
            Postgres::default().tag(),
            "11-alpine",
            "testcontainers-modules changed its default Postgres tag"
        );
        assert_eq!(
            Mysql::default().tag(),
            "8.1",
            "testcontainers-modules changed its default MySQL tag"
        );
    }

    /// The pins themselves, as literals.
    ///
    /// Asserting a helper against its own constant proves only that the value
    /// was copied, so the value has to be nailed down somewhere. Changing a
    /// database version is then a two-line diff — the constant and this test —
    /// which is the review signal the crate is for.
    #[test]
    fn the_pinned_tags_are_the_values_we_reviewed() {
        assert_eq!(POSTGRES_TAG, "18-alpine");
        assert_eq!(POSTGRES_GRAPH_TAG, "19beta3-alpine");
        assert_eq!(MYSQL_TAG, "9.7");
        assert_eq!(TIMESCALEDB_IMAGE, "timescale/timescaledb");
        assert_eq!(TIMESCALEDB_TAG, "2.29.2-pg18");
        assert_eq!(MARIADB_IMAGE, "mariadb");
        assert_eq!(MARIADB_TAG, "11.8");
    }

    /// Known floating-alias words. Checked per component (split on `-`/`_`),
    /// not as a whole-string match: `MariaDB` actually publishes `lts-noble`,
    /// which a whole-string check would miss entirely.
    const FLOATING_ALIAS_WORDS: [&str; 4] = ["latest", "lts", "stable", "edge"];

    fn is_floating_alias(tag: &str) -> bool {
        tag.split(['-', '_'])
            .any(|part| FLOATING_ALIAS_WORDS.contains(&part.to_lowercase().as_str()))
    }

    /// No constant may carry a floating alias: the whole point is that a
    /// version change is visible in a diff.
    #[test]
    fn no_pin_is_a_floating_alias() {
        for tag in [
            POSTGRES_TAG,
            POSTGRES_GRAPH_TAG,
            MYSQL_TAG,
            TIMESCALEDB_TAG,
            MARIADB_TAG,
        ] {
            assert!(
                !is_floating_alias(tag),
                "{tag} is a floating alias, not a pin"
            );
        }
    }

    /// Component-wise, not substring: a real version segment that merely
    /// contains an alias word as a substring (`stablefoo`) must not be
    /// flagged, only a standalone `-`/`_`-separated alias component.
    #[test]
    fn floating_alias_check_is_component_wise_not_substring() {
        for tag in ["lts-noble", "latest-pg18", "LTS-noble", "edge_alpine"] {
            assert!(
                is_floating_alias(tag),
                "{tag} should be flagged as a floating alias"
            );
        }
        for tag in [
            "18-alpine",
            "19beta3-alpine",
            "9.7",
            "2.29.2-pg18",
            "11.8",
            "stablefoo",
        ] {
            assert!(!is_floating_alias(tag), "{tag} should not be flagged");
        }
    }

    /// The env-var names are a published contract — module docs, `docs/TESTING.md`
    /// and CI invocations all spell them out. A rename that compiles would leave
    /// every one of those silently ignored.
    #[test]
    fn env_var_names_match_the_documented_contract() {
        assert_eq!(ENV_POSTGRES_TAG, "GEARS_TEST_PG_TAG");
        assert_eq!(ENV_POSTGRES_GRAPH_TAG, "GEARS_TEST_PG_GRAPH_TAG");
        assert_eq!(ENV_MYSQL_TAG, "GEARS_TEST_MYSQL_TAG");
        assert_eq!(ENV_TIMESCALEDB_TAG, "GEARS_TEST_TIMESCALEDB_TAG");
        assert_eq!(ENV_MARIADB_TAG, "GEARS_TEST_MARIADB_TAG");
        assert_eq!(ENV_GRAPH_LANE_REQUIRED, "GEARS_TEST_PG_GRAPH_REQUIRED");
    }

    /// Each accessor must consult *its own* variable. Swapping two of them
    /// inside the accessor bodies compiles and would otherwise stay green.
    #[test]
    #[serial]
    fn each_accessor_reads_its_own_variable() {
        /// (environment variable, accessor it must reach, constant it falls back to)
        type AccessorCase = (&'static str, fn() -> String, &'static str);

        let cases: [AccessorCase; 5] = [
            (ENV_POSTGRES_TAG, postgres_tag, POSTGRES_TAG),
            (
                ENV_POSTGRES_GRAPH_TAG,
                postgres_graph_tag,
                POSTGRES_GRAPH_TAG,
            ),
            (ENV_MYSQL_TAG, mysql_tag, MYSQL_TAG),
            (ENV_TIMESCALEDB_TAG, timescaledb_tag, TIMESCALEDB_TAG),
            (ENV_MARIADB_TAG, mariadb_tag, MARIADB_TAG),
        ];
        for (var, accessor, default) in cases {
            temp_env::with_var(var, Some("sentinel-value"), || {
                assert_eq!(
                    accessor(),
                    "sentinel-value",
                    "{var} did not reach its accessor"
                );
            });
            temp_env::with_var(var, None::<&str>, || {
                assert_eq!(accessor(), default, "{var} unset should yield the constant");
            });
        }
    }

    /// Resolved accessors, not constants: asserting against `POSTGRES_TAG`
    /// here would make the suite fail under the crate's own documented
    /// overrides (`GEARS_TEST_PG_TAG=16-alpine`), which CI runs as a matrix.
    #[test]
    #[serial]
    fn helpers_carry_the_resolved_tags() {
        assert_eq!(
            postgres().descriptor(),
            format!("postgres:{}", postgres_tag())
        );
        assert_eq!(
            postgres_graph().descriptor(),
            format!("postgres:{}", postgres_graph_tag())
        );
        assert_eq!(mysql().descriptor(), format!("mysql:{}", mysql_tag()));
        assert_eq!(
            postgres_named("cluster_test").descriptor(),
            format!("postgres:{}", postgres_tag())
        );
    }

    /// The invariant the crate exists for: a helper must never inherit the
    /// upstream default. Guards the case `helpers_carry_the_resolved_tags`
    /// cannot — deleting `.with_tag(...)` from a helper.
    #[test]
    #[serial]
    fn helpers_apply_a_tag_rather_than_inheriting_the_upstream_default() {
        temp_env::with_var(ENV_POSTGRES_TAG, Some("tag-under-test"), || {
            assert_eq!(postgres().descriptor(), "postgres:tag-under-test");
            assert_eq!(postgres_named("db").descriptor(), "postgres:tag-under-test");
        });
        temp_env::with_var(ENV_MYSQL_TAG, Some("tag-under-test"), || {
            assert_eq!(mysql().descriptor(), "mysql:tag-under-test");
        });
    }

    #[test]
    #[serial]
    fn generic_images_carry_the_resolved_tags() {
        assert_eq!(timescaledb().name(), TIMESCALEDB_IMAGE);
        assert_eq!(timescaledb().tag(), timescaledb_tag());
        assert_eq!(mariadb().name(), MARIADB_IMAGE);
        assert_eq!(mariadb().tag(), mariadb_tag());
    }

    /// `postgres_tagged` exists so a higher local floor does not cost the
    /// suite its place in the CI version matrix.
    #[test]
    #[serial]
    fn a_local_floor_still_yields_to_the_override() {
        temp_env::with_var(ENV_POSTGRES_TAG, None::<&str>, || {
            assert_eq!(
                postgres_tagged("16-alpine").descriptor(),
                "postgres:16-alpine"
            );
        });
        temp_env::with_var(ENV_POSTGRES_TAG, Some("18-alpine"), || {
            assert_eq!(
                postgres_tagged("16-alpine").descriptor(),
                "postgres:18-alpine"
            );
        });
    }

    #[test]
    fn an_override_wins_over_the_constant() {
        assert_eq!(
            tag_from(Some("16-alpine".to_owned()), POSTGRES_TAG),
            "16-alpine"
        );
    }

    #[test]
    #[serial]
    fn an_unset_or_empty_override_falls_back_to_the_constant() {
        assert_eq!(tag_from(None, POSTGRES_TAG), POSTGRES_TAG);
        temp_env::with_var(ENV_POSTGRES_TAG, Some(""), || {
            assert_eq!(postgres_tag(), POSTGRES_TAG);
        });
    }

    #[test]
    fn graph_lane_flag_is_off_unless_explicitly_set() {
        for off in ["", "0", "false", "False", "off", "no", "  false  "] {
            assert!(!flag_from(Some(off.to_owned())), "{off:?} should be off");
        }
        assert!(!flag_from(None));
        for on in ["1", "true", "yes"] {
            assert!(flag_from(Some(on.to_owned())), "{on:?} should be on");
        }
    }

    #[test]
    #[serial]
    fn graph_lane_required_reads_its_variable() {
        temp_env::with_var(ENV_GRAPH_LANE_REQUIRED, Some("1"), || {
            assert!(graph_lane_required());
        });
        temp_env::with_var(ENV_GRAPH_LANE_REQUIRED, Some("false"), || {
            assert!(!graph_lane_required());
        });
        temp_env::with_var(ENV_GRAPH_LANE_REQUIRED, None::<&str>, || {
            assert!(!graph_lane_required());
        });
    }

    /// The `TimescaleDB` image is pinned twice — here and in the Python `E2E`
    /// sidecar — so the comments that point at each other are backed by a
    /// check. A skew validates plugin migrations against a different
    /// `PostgreSQL` major than `E2E` actually runs.
    #[test]
    fn e2e_sidecar_pins_the_same_timescaledb_image() {
        let sidecars = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testing/e2e/lib/sidecars.py"
        );
        let Ok(source) = std::fs::read_to_string(sidecars) else {
            panic!("cannot read {sidecars}; update this test if the file moved");
        };
        let expected = format!("IMAGE = \"{TIMESCALEDB_IMAGE}:{TIMESCALEDB_TAG}\"");
        assert!(
            source.contains(&expected),
            "testing/e2e/lib/sidecars.py does not pin {expected}; \
             the E2E lane and the Rust plugin tests would run different PostgreSQL majors"
        );
    }
}
