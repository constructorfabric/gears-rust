//! Typed cluster profile marker and profile-scope resolution.
//!
//! A consumer declares a profile once as a zero-sized type implementing
//! [`ClusterProfile`]; the SDK reads the marker's [`ClusterProfile::NAME`] and
//! maps it to a stable [`ClientScope`] via [`profile_scope`]. The profile
//! string is therefore defined once on the marker and never re-typed at call
//! sites, removing magic-string profile names by construction.

use toolkit::client_hub::ClientScope;

use crate::error::ClusterError;

/// The character rule every cluster profile name must satisfy: between 1 and
/// [`MAX_CLUSTER_NAME_LEN`] ASCII alphanumerics, `_`, or `-`. `/` is excluded
/// because it is the scope separator used by per-primitive scoping (DESIGN §3.6).
pub const CLUSTER_NAME_RULE: &str = "[a-zA-Z0-9_-]{1,255}";

/// The maximum length (in bytes) of a cluster profile name. Names map to a
/// `cluster:{profile}` lookup scope and must stay within the bounds a backend
/// key component can carry; the cap is part of the frozen contract so that
/// tightening it later is not a breaking change.
pub const MAX_CLUSTER_NAME_LEN: usize = 255;

/// A typed marker for a cluster profile.
///
/// Implemented once by the consumer on a zero-sized type; the associated
/// [`NAME`](ClusterProfile::NAME) is the single source of truth for the profile
/// string and is passed by type — not by string — at resolver call sites.
pub trait ClusterProfile: Copy + Send + Sync + 'static {
    /// The stable profile name. Must satisfy [`CLUSTER_NAME_RULE`].
    const NAME: &'static str;
}

/// One inventoried [`ClusterProfile`] marker, submitted by
/// [`register_cluster_profile!`](crate::register_cluster_profile).
///
/// The consumer-side counterpart of the gear's config `profiles` map: it is how a
/// *process* states which profiles it intends to use, without the profile string
/// appearing in a third place. Invariant I10 allows exactly two — the marker and
/// the `.profile()` call — so the wiring enumerates these instead of reading a
/// list (§4.9.2).
///
/// Unfeatured on purpose. Profile 1 has no consumer registration at all, and the
/// readiness contributor (`K5`) needs the same enumeration to know which profiles
/// this process cares about; a `grpc-client`-only registry would leave the
/// embedded profile with no notion of an intended profile set.
#[derive(Debug, Clone, Copy)]
pub struct RegisteredProfile {
    /// The marker's [`ClusterProfile::NAME`].
    pub name: &'static str,
}

toolkit::inventory::collect!(RegisteredProfile);

/// Every profile this process declared through
/// [`register_cluster_profile!`](crate::register_cluster_profile), deduplicated
/// and in a stable order.
///
/// Deduplicated because the same marker may legitimately be registered by two
/// crates in one binary (a gear and its test fixture, say), and a duplicate would
/// otherwise be reported as a second profile in every diagnostic built from this.
/// Sorted so those diagnostics do not vary between runs — `inventory`'s iteration
/// order is link order, which is not stable.
#[must_use]
pub fn registered_profiles() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = toolkit::inventory::iter::<RegisteredProfile>
        .into_iter()
        .map(|profile| profile.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Declares a [`ClusterProfile`] marker to the process, so cluster's consumer
/// wiring and readiness contributor can enumerate it (§4.9.2).
///
/// ```
/// # use cluster_sdk::{ClusterProfile, register_cluster_profile};
/// #[derive(Clone, Copy)]
/// pub struct EventBrokerProfile;
/// impl ClusterProfile for EventBrokerProfile {
///     const NAME: &'static str = "event-broker";
/// }
/// register_cluster_profile!(EventBrokerProfile);
/// ```
///
/// # What it is not
///
/// It is **not** a wiring call and not a prerequisite for resolving. A facade
/// resolves whether or not its profile was registered here, in both deployment
/// profiles — this exists so the process can say *which profiles it expects*,
/// which lets the wiring warn about a profile the server does not bind
/// and lets readiness gate on the profiles this consumer actually uses rather
/// than on every profile the cluster gear happens to serve (§4.4).
///
/// Invoke it at module scope, once per marker. Registering the same marker twice
/// is harmless: [`registered_profiles`] deduplicates.
#[macro_export]
macro_rules! register_cluster_profile {
    ($marker:ty) => {
        $crate::inventory::submit! {
            $crate::profile::RegisteredProfile {
                name: <$marker as $crate::ClusterProfile>::NAME,
            }
        }
    };
}

/// Returns `true` if `name` satisfies [`CLUSTER_NAME_RULE`].
#[must_use]
pub fn is_valid_cluster_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CLUSTER_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Validates a cluster profile or coordination name against
/// [`CLUSTER_NAME_RULE`].
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `name` is empty or contains a
/// character outside the rule.
pub fn validate_cluster_name(name: &str) -> Result<(), ClusterError> {
    if is_valid_cluster_name(name) {
        Ok(())
    } else {
        Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason: CLUSTER_NAME_RULE,
        })
    }
}

/// The rule an *optionally scope-prefixed* coordination name must satisfy: one or
/// more `/`-separated segments, each matching [`CLUSTER_NAME_RULE`], with no empty
/// segment. Unlike [`CLUSTER_NAME_RULE`] this permits `/` because it is the scope
/// separator (DESIGN §3.8): a scoped lock/leader name composes as `prefix/name`.
pub const SCOPED_CLUSTER_NAME_RULE: &str = "[a-zA-Z0-9_-]+(/[a-zA-Z0-9_-]+)* (max 1024 chars)";

/// The maximum length (in bytes) of a fully-composed scoped coordination name on
/// the wire. Generous enough for stacked scopes (`a/b/c/name`); bounds a
/// pathological wire name. Part of the frozen contract.
pub const MAX_SCOPED_CLUSTER_NAME_LEN: usize = 1024;

/// Validates an *optionally scope-prefixed* coordination name — the form the
/// lock/leader **server** path receives after client-side `.scoped()` composes
/// `prefix/name`. Each `/`-separated segment must satisfy [`CLUSTER_NAME_RULE`].
///
/// The facade validates the *unscoped* leaf name with [`validate_cluster_name`];
/// the receiver must accept the composed name too, so it uses this scope-aware
/// rule (mirroring how a scoped cache key is validated). Using
/// [`validate_cluster_name`] here instead — which forbids `/` — rejects every
/// scoped lock/leader operation on the wire even though `.scoped()` is public
/// API, and it does so only in Profile 3 (the local backend never re-validates),
/// violating the Profile-1/Profile-3 parity invariant (I1).
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `name` is empty, exceeds
/// [`MAX_SCOPED_CLUSTER_NAME_LEN`], has an empty segment (a leading, trailing, or
/// doubled `/`), or any segment contains a character outside [`CLUSTER_NAME_RULE`].
pub fn validate_scoped_cluster_name(name: &str) -> Result<(), ClusterError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_SCOPED_CLUSTER_NAME_LEN
        && !name.split('/').any(str::is_empty)
        && name.split('/').all(is_valid_cluster_name);
    if ok {
        Ok(())
    } else {
        Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason: SCOPED_CLUSTER_NAME_RULE,
        })
    }
}

/// Maps a profile name to its stable cluster lookup [`ClientScope`]
/// (`cluster:{profile}`), the scope under which every primitive resolves and
/// registers its backend for that profile.
///
/// Internal-only (ADR-007): the typed [`ClusterProfile`] marker is the sole
/// consumer-facing profile path, so this is `pub(crate)` — consumers never name
/// a profile by string. The resolvers and registration helpers call it with the
/// marker's [`ClusterProfile::NAME`].
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `name` violates
/// [`CLUSTER_NAME_RULE`], before any backend lookup is attempted.
pub(crate) fn profile_scope(name: &str) -> Result<ClientScope, ClusterError> {
    validate_cluster_name(name)?;
    let scope = ClientScope::new(format!("cluster:{name}"));
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::{ClusterProfile, is_valid_cluster_name, profile_scope, validate_cluster_name};
    use crate::error::ClusterError;

    #[derive(Clone, Copy)]
    struct OrdersProfile;
    impl ClusterProfile for OrdersProfile {
        const NAME: &'static str = "orders";
    }

    #[derive(Clone, Copy)]
    struct InventoriedByProfileTests;
    impl ClusterProfile for InventoriedByProfileTests {
        const NAME: &'static str = "profile-tests-inventoried";
    }
    crate::register_cluster_profile!(InventoriedByProfileTests);

    /// The macro and its enumeration must work with **no features enabled**.
    ///
    /// This is invariant I10's mechanism and `K5`'s input, and both have to exist
    /// in Profile 1 — where there is no consumer registration at all, so a
    /// `grpc-client`-gated registry would leave the embedded profile with no notion
    /// of an intended profile set. `wiring_tests.rs` covers the same ground behind
    /// the feature; this test is here so a default-feature build covers it too.
    #[test]
    fn a_marker_is_inventoried_without_any_feature_enabled() {
        let profiles = super::registered_profiles();
        assert!(
            profiles.contains(&InventoriedByProfileTests::NAME),
            "the unfeatured build must enumerate registered markers, got {profiles:?}"
        );
    }

    #[test]
    fn valid_names_accepted() {
        assert!(is_valid_cluster_name("default"));
        assert!(is_valid_cluster_name("svc-shard-1_a"));
        assert!(is_valid_cluster_name(OrdersProfile::NAME));
    }

    #[test]
    fn invalid_names_rejected() {
        assert!(!is_valid_cluster_name(""));
        assert!(!is_valid_cluster_name("has space"));
        assert!(!is_valid_cluster_name("bad:colon"));
        // `/` is the scope separator and is not allowed in profile names.
        assert!(!is_valid_cluster_name("svc/shard"));
    }

    #[test]
    fn name_length_is_capped() {
        use super::MAX_CLUSTER_NAME_LEN;
        let at_cap = "a".repeat(MAX_CLUSTER_NAME_LEN);
        assert!(is_valid_cluster_name(&at_cap), "a name at the cap is valid");
        let over_cap = "a".repeat(MAX_CLUSTER_NAME_LEN + 1);
        assert!(
            !is_valid_cluster_name(&over_cap),
            "a name past the cap is rejected"
        );
    }

    #[test]
    fn profile_scope_composes_cluster_prefix() {
        let Ok(scope) = profile_scope(OrdersProfile::NAME) else {
            panic!("a valid profile name must resolve to a scope");
        };
        assert_eq!(scope.as_str(), "cluster:orders");
    }

    #[test]
    fn profile_scope_rejects_invalid_name_before_lookup() {
        assert!(matches!(
            profile_scope("nope:bad"),
            Err(ClusterError::InvalidName { .. })
        ));
    }

    #[test]
    fn validate_returns_invalid_name_error() {
        assert!(validate_cluster_name("ok-name").is_ok());
        assert!(matches!(
            validate_cluster_name("x y"),
            Err(ClusterError::InvalidName { .. })
        ));
    }

    #[test]
    fn scoped_name_accepts_slash_separated_segments() {
        use super::validate_scoped_cluster_name;
        // A bare leaf and a scoped `prefix/name` must both be accepted — the
        // latter is exactly what `.scoped()` composes and what a bare
        // `validate_cluster_name` wrongly rejected on the server path.
        assert!(validate_scoped_cluster_name("reservation").is_ok());
        assert!(validate_scoped_cluster_name("cluster-consumer/reservation").is_ok());
        assert!(validate_scoped_cluster_name("a/b/c/leaf").is_ok());
    }

    #[test]
    fn scoped_name_rejects_empty_segments_and_bad_chars() {
        use super::{MAX_SCOPED_CLUSTER_NAME_LEN, validate_scoped_cluster_name};
        for bad in [
            "",
            "/leading",
            "trailing/",
            "a//b",
            "has space",
            "bad:colon/x",
        ] {
            assert!(
                matches!(
                    validate_scoped_cluster_name(bad),
                    Err(ClusterError::InvalidName { .. })
                ),
                "`{bad}` must be rejected"
            );
        }
        let over_cap = "a".repeat(MAX_SCOPED_CLUSTER_NAME_LEN + 1);
        assert!(validate_scoped_cluster_name(&over_cap).is_err());
    }
}
