// Created: 2026-08-12 by Constructor Tech
//! Tests for the `ClusterError` ⇄ `CanonicalError` codec.
//!
//! The load-bearing one is [`every_cluster_error_variant_round_trips`]: it
//! enumerates **every** [`ClusterError`] variant, pushes each through the full
//! encode/decode path — `ClusterError` → [`ClusterWireError`] → [`Problem`] →
//! `ClusterWireError` → `ClusterError` — and asserts variant *and* field equality
//! coming back. It is written as a table rather than one test per variant so a new
//! variant cannot be added without appearing in it.

use std::time::Duration;

use toolkit_canonical_errors::{Problem, ProblemCategory};

use super::{ClusterWireError, LeaseContext, to_cluster_error, transport_failure};
use crate::cache::CacheEntry;
use crate::error::{ClusterError, ProviderErrorKind};

/// The full encode/decode path, exactly as the two processes run it.
fn round_trip(error: ClusterError) -> ClusterError {
    let wire = ClusterWireError::from(error);
    let problem = Problem::from(wire);
    to_cluster_error(problem, LeaseContext::None).expect("a typed error decodes to an error")
}

/// Every `ClusterError` variant. Exhaustive by construction: the codec matches
/// exhaustively in both directions, so a new variant breaks the build before it
/// can reach this list — and this list is what proves the *round trip*, which the
/// compiler cannot.
fn every_variant() -> Vec<ClusterError> {
    vec![
        ClusterError::CapabilityNotMet {
            primitive: "ClusterCacheV1",
            capability: "Linearizable",
            provider: "postgres",
        },
        ClusterError::ProfileNotBound { profile: "orders" },
        ClusterError::ProfileNotSpecified,
        ClusterError::InvalidName {
            name: "Bad Name".to_owned(),
            reason: "lowercase alphanumeric and dashes",
        },
        ClusterError::InvalidConfig {
            reason: "eventually-consistent cache bound to a linearizable default".to_owned(),
        },
        ClusterError::LockContended {
            name: "ledger".to_owned(),
        },
        ClusterError::LockTimeout {
            name: "ledger".to_owned(),
            waited: Duration::from_millis(250),
        },
        ClusterError::LockExpired {
            name: "ledger".to_owned(),
        },
        ClusterError::Unsupported {
            feature: "prefix_watch",
        },
        ClusterError::CasConflict {
            key: "counter".to_owned(),
            current: Some(CacheEntry {
                value: b"41".to_vec(),
                version: 7,
            }),
        },
        ClusterError::CasConflict {
            key: "counter".to_owned(),
            current: None,
        },
        ClusterError::Shutdown,
        ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            message: "connection reset".to_owned(),
        },
        ClusterError::Provider {
            kind: ProviderErrorKind::Timeout,
            message: "statement timeout".to_owned(),
        },
        ClusterError::Provider {
            kind: ProviderErrorKind::ResourceExhausted,
            message: "pool exhausted".to_owned(),
        },
        ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            message: "password authentication failed".to_owned(),
        },
        ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: "relation does not exist".to_owned(),
        },
    ]
}

/// `C1`'s central exit criterion.
#[test]
fn every_cluster_error_variant_round_trips() {
    for original in every_variant() {
        let decoded = round_trip(original.clone());
        assert_eq!(
            format!("{decoded:?}"),
            format!("{original:?}"),
            "variant did not survive the round trip"
        );
        assert_eq!(
            decoded.to_string(),
            original.to_string(),
            "the operator-facing message did not survive the round trip"
        );
    }
}

/// Retryability is not an incidental property of the decoded error — it is what
/// `RestartingWatch` branches on, so it is asserted for every variant.
#[test]
fn retryability_is_preserved_for_every_variant() {
    for original in every_variant() {
        let decoded = round_trip(original.clone());
        assert_eq!(
            decoded.is_retryable(),
            original.is_retryable(),
            "retryability changed across the round trip for {original:?}"
        );
    }
}

/// The reason `ProviderErrorKind` cannot be inferred from the canonical category:
/// `Shutdown` and `Provider{ConnectionLost}` are both `ServiceUnavailable`, and one
/// is terminal while the other is retryable. Getting this wrong makes the
/// auto-restart combinator retry a shutdown forever (§6.9).
#[test]
fn shutdown_and_connection_lost_share_a_category_but_not_a_verdict() {
    let shutdown = Problem::from(ClusterWireError::from(ClusterError::Shutdown));
    let lost = Problem::from(ClusterWireError::from(ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "pod gone".to_owned(),
    }));

    assert_eq!(
        shutdown.status,
        ProblemCategory::ServiceUnavailable.http_status()
    );
    assert_eq!(lost.status, shutdown.status, "same canonical category");
    assert_ne!(
        lost.error_code, shutdown.error_code,
        "the discriminant must travel explicitly, in the error code"
    );

    let decoded_shutdown = to_cluster_error(shutdown, LeaseContext::None).expect("an error");
    let decoded_lost = to_cluster_error(lost, LeaseContext::None).expect("an error");

    assert!(matches!(decoded_shutdown, ClusterError::Shutdown));
    assert!(!decoded_shutdown.is_retryable(), "shutdown is terminal");
    assert!(
        matches!(
            decoded_lost,
            ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                ..
            }
        ),
        "got {decoded_lost:?}"
    );
    assert!(
        decoded_lost.is_retryable(),
        "a lost connection is retryable"
    );
}

/// `RestartingWatch` resubscribes only on a retryable `Closed(_)`. This asserts the
/// property it depends on across the boundary: the terminal event a broken stream
/// produces decodes back to something the combinator classifies as retryable, while
/// a shutdown decodes to something it propagates.
#[test]
fn restarting_watch_retryability_survives_the_round_trip() {
    // A broken transport with no canonical body — the case §6.9 synthesises.
    let synthesised = transport_failure("channel closed");
    assert!(
        synthesised.is_retryable(),
        "an unreachable cluster gear must look like an unreachable Postgres"
    );

    // The same verdict after a full wire round trip, which is what a server-sent
    // `Closed(Provider{ConnectionLost})` goes through.
    let closed = round_trip(ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "upstream watch dropped".to_owned(),
    });
    assert!(closed.is_retryable(), "got {closed:?}");

    // And the terminal case the combinator must *not* retry.
    let shutdown = round_trip(ClusterError::Shutdown);
    assert!(!shutdown.is_retryable(), "got {shutdown:?}");
}

/// §6.9's per-kind canonical mapping, asserted rather than assumed — it is the
/// reason `Provider` fans out into five wire variants.
#[test]
fn provider_kinds_map_to_their_documented_canonical_categories() {
    let cases = [
        (
            ProviderErrorKind::ConnectionLost,
            ProblemCategory::ServiceUnavailable,
        ),
        (
            ProviderErrorKind::Timeout,
            ProblemCategory::DeadlineExceeded,
        ),
        (
            ProviderErrorKind::ResourceExhausted,
            ProblemCategory::ResourceExhausted,
        ),
        // `Internal`, not `Unauthenticated`: the failing credential is the
        // cluster gear's against Postgres, not the caller's against cluster.
        (ProviderErrorKind::AuthFailure, ProblemCategory::Internal),
        (ProviderErrorKind::Other, ProblemCategory::Internal),
    ];

    for (kind, expected) in cases {
        let problem = Problem::from(ClusterWireError::from(ClusterError::Provider {
            kind,
            message: "boom".to_owned(),
        }));
        assert_eq!(
            problem.status,
            expected.http_status(),
            "{kind:?} mapped to the wrong canonical category"
        );
    }
}

#[test]
fn every_wire_error_carries_the_cluster_domain() {
    for original in every_variant() {
        let problem = Problem::from(ClusterWireError::from(original));
        assert_eq!(
            problem.error_domain.as_deref(),
            Some(super::CLUSTER_ERROR_DOMAIN)
        );
        assert!(problem.error_code.is_some(), "every variant carries a code");
    }
}

/// §6.11's skew rule: a code this build does not know becomes `Provider{Other}` —
/// not a panic, not a silent `Ok`, and not something retryable.
#[test]
fn an_unknown_error_code_decodes_as_non_retryable_provider_other() {
    let from_the_future = Problem::contract_error(
        ProblemCategory::Aborted,
        "quorum_lost",
        super::CLUSTER_ERROR_DOMAIN,
        "a code from a newer server",
        serde_json::json!({ "replicas": 3 }),
    );

    let decoded = to_cluster_error(from_the_future, LeaseContext::None).expect("an error");

    assert!(
        matches!(
            &decoded,
            ClusterError::Provider {
                kind: ProviderErrorKind::Other,
                message,
            } if message.contains("a code from a newer server")
        ),
        "got {decoded:?}"
    );
    assert!(!decoded.is_retryable());
}

#[test]
fn an_unknown_domain_decodes_as_provider_other() {
    let foreign = Problem::contract_error(
        ProblemCategory::Aborted,
        "lock_contended",
        "someone.else.v1",
        "not ours",
        serde_json::Value::Null,
    );

    assert!(matches!(
        to_cluster_error(foreign, LeaseContext::None),
        Some(ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// Lease-keyed absence
// ---------------------------------------------------------------------------

/// A bare canonical `NotFound` — no cluster `error_code` — is the stale-lease
/// class, and which variant it becomes depends on which operation asked (§6.9).
fn bare_not_found() -> Problem {
    Problem::contract_error(
        ProblemCategory::NotFound,
        "not_found",
        "some.other.domain",
        "no rows matched",
        serde_json::Value::Null,
    )
}

#[test]
fn bare_not_found_on_a_lock_renewal_is_lock_expired() {
    let decoded = to_cluster_error(bare_not_found(), LeaseContext::LockRenew { name: "ledger" })
        .expect("a lost lease is an error the caller must act on");

    assert!(
        matches!(&decoded, ClusterError::LockExpired { name } if name == "ledger"),
        "got {decoded:?}"
    );
}

#[test]
fn bare_not_found_on_a_release_is_not_an_error_at_all() {
    // Idempotent by absence: a token matching nothing has already achieved what
    // the caller wanted, and reporting otherwise would make the token probeable.
    assert!(
        to_cluster_error(bare_not_found(), LeaseContext::LeaseRelease).is_none(),
        "release is idempotent by absence, so absence is not an error"
    );
}

#[test]
fn bare_not_found_on_an_election_renewal_names_the_election() {
    let decoded = to_cluster_error(
        bare_not_found(),
        LeaseContext::ElectionRenew { name: "compactor" },
    )
    .expect("the pump needs a loss signal");

    assert!(
        matches!(&decoded, ClusterError::LockExpired { name } if name == "compactor"),
        "got {decoded:?}"
    );
}

#[test]
fn bare_not_found_on_a_subscription_is_a_terminal_shutdown() {
    // The subscription's replica went away. Terminal and non-retryable, so
    // `RestartingWatch` propagates rather than resubscribing (§6.9).
    let decoded =
        to_cluster_error(bare_not_found(), LeaseContext::ElectionSubscription).expect("an error");

    assert!(matches!(decoded, ClusterError::Shutdown), "got {decoded:?}");
    assert!(!decoded.is_retryable());
}

/// The lease context must never override a *typed* answer — the server already
/// said what happened, and letting the caller's guess win would let the two
/// contradict each other.
#[test]
fn a_typed_code_wins_over_the_lease_context() {
    let typed = Problem::from(ClusterWireError::from(ClusterError::ProfileNotBound {
        profile: "orders",
    }));

    let decoded = to_cluster_error(typed, LeaseContext::LockRenew { name: "ledger" })
        .expect("a typed error decodes to an error");

    assert!(
        matches!(&decoded, ClusterError::ProfileNotBound { profile } if *profile == "orders"),
        "got {decoded:?}"
    );
}

// ---------------------------------------------------------------------------
// CasConflict payload fidelity (decision 17a)
// ---------------------------------------------------------------------------

#[test]
fn cas_conflict_carries_the_full_entry_when_the_server_supplies_one() {
    let decoded = round_trip(ClusterError::CasConflict {
        key: "counter".to_owned(),
        current: Some(CacheEntry {
            value: b"41".to_vec(),
            version: 7,
        }),
    });

    let ClusterError::CasConflict {
        current: Some(entry),
        ..
    } = decoded
    else {
        panic!("expected a conflict carrying the current entry, got {decoded:?}");
    };
    assert_eq!(entry.value, b"41".to_vec());
    assert_eq!(entry.version, 7);
}

/// Decision 17a's fallback, which is contract-legal either way because `current` is
/// SHOULD. A version-only conflict decodes as absent, because `CacheEntry` has no
/// representation for "version known, value not" — so the caller re-reads rather
/// than being handed a fabricated value.
#[test]
fn a_version_only_cas_conflict_decodes_as_an_absent_entry() {
    let version_only = Problem::from(ClusterWireError::CasConflict {
        key: "counter".to_owned(),
        current_version: Some(7),
        current_value: None,
    });

    let decoded = to_cluster_error(version_only, LeaseContext::None).expect("an error");

    assert!(
        matches!(&decoded, ClusterError::CasConflict { key, current: None } if key == "counter"),
        "got {decoded:?}"
    );
}

// ---------------------------------------------------------------------------
// Interning keeps the frozen error model frozen
// ---------------------------------------------------------------------------

/// The `&'static str` fields survive a wire hop as `String` and come back
/// promoted, which is what lets invariant I3 hold against a wire that speaks
/// `String`.
#[test]
fn static_str_fields_are_reinterned_on_decode() {
    let decoded = round_trip(ClusterError::CapabilityNotMet {
        primitive: "ClusterCacheV1",
        capability: "Linearizable",
        provider: "postgres",
    });

    let ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    } = decoded
    else {
        panic!("expected CapabilityNotMet, got {decoded:?}");
    };
    assert_eq!(primitive, "ClusterCacheV1");
    assert_eq!(capability, "Linearizable");
    // The server-side provider, never "remote" — the operator has to see which
    // real backend failed the requirement (§5.5).
    assert_eq!(provider, "postgres");
}

// ---------------------------------------------------------------------------
// WireError ⇄ Problem, the watch-stream projection
// ---------------------------------------------------------------------------

#[test]
fn a_terminal_watch_error_round_trips_through_wire_error() {
    use crate::dto::WireError;

    let original = ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "upstream watch dropped".to_owned(),
    };

    // Server: ClusterError → wire → the shape a `Closed` event carries.
    let carried = WireError::from(ClusterWireError::from(original.clone()));
    assert_eq!(carried.error_domain, super::CLUSTER_ERROR_DOMAIN);
    assert_eq!(carried.error_code, "provider_connection_lost");

    // Client: back through the one codec.
    let decoded = to_cluster_error(Problem::from(carried), LeaseContext::None).expect("an error");
    assert_eq!(format!("{decoded:?}"), format!("{original:?}"));
    assert!(
        decoded.is_retryable(),
        "a `Closed` carrying a lost connection must stay retryable so `RestartingWatch` resubscribes"
    );
}

// ---------------------------------------------------------------------------
// ClusterError -> tonic::Status, the server half (`S1`)
// ---------------------------------------------------------------------------

/// The full cross-process path the four service impls sit on: the gear encodes a
/// [`ClusterError`] into a `Status`, and the client decodes it back through the
/// **same** codec by way of `map_tonic_status`'s trailer.
///
/// This is what makes the mapping's non-injectivity harmless. `Shutdown` and
/// `Provider{ConnectionLost}` both travel as `Unavailable`; only the trailer's
/// `error_code` tells them apart, and only the right one is retryable.
#[cfg(feature = "grpc-client")]
#[test]
fn every_variant_round_trips_through_a_tonic_status() {
    use toolkit_transport_grpc::extract_problem;

    for original in every_variant() {
        let status = super::to_status(original.clone());
        let problem: Problem = extract_problem(status.metadata())
            .expect("the trailer decodes")
            .expect("a cluster status always carries the problem trailer");

        let decoded = to_cluster_error(problem, LeaseContext::None)
            .expect("a typed error decodes to an error");
        assert_eq!(
            format!("{decoded:?}"),
            format!("{original:?}"),
            "variant did not survive the status round trip"
        );
        assert_eq!(
            decoded.is_retryable(),
            original.is_retryable(),
            "retryability changed across the status round trip for {original:?}"
        );
        assert_eq!(
            status.message(),
            original.to_string(),
            "the status message must carry the same operator-facing text as the error"
        );
    }
}

/// The codes a peer that does not speak the problem envelope sees, per DESIGN
/// section 6.9's table. Written out rather than derived so a change to the
/// mapping is a change to this test.
#[cfg(feature = "grpc-client")]
#[test]
fn the_status_code_follows_the_canonical_mapping() {
    use tonic::Code;

    let cases: Vec<(ClusterError, Code)> = vec![
        (
            ClusterError::ProfileNotBound { profile: "orders" },
            Code::NotFound,
        ),
        (
            ClusterError::CapabilityNotMet {
                primitive: "ClusterCacheV1",
                capability: "Linearizable",
                provider: "postgres",
            },
            Code::FailedPrecondition,
        ),
        (
            ClusterError::LockExpired {
                name: "ledger".to_owned(),
            },
            Code::FailedPrecondition,
        ),
        (
            ClusterError::LockContended {
                name: "ledger".to_owned(),
            },
            Code::Aborted,
        ),
        (
            ClusterError::CasConflict {
                key: "counter".to_owned(),
                current: None,
            },
            Code::Aborted,
        ),
        (
            ClusterError::LockTimeout {
                name: "ledger".to_owned(),
                waited: Duration::from_millis(250),
            },
            Code::DeadlineExceeded,
        ),
        (
            ClusterError::Unsupported {
                feature: "prefix_watch",
            },
            Code::Unimplemented,
        ),
        (ClusterError::Shutdown, Code::Unavailable),
        (
            ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "reset".to_owned(),
            },
            Code::Unavailable,
        ),
        (
            ClusterError::Provider {
                kind: ProviderErrorKind::ResourceExhausted,
                message: "pool exhausted".to_owned(),
            },
            Code::ResourceExhausted,
        ),
        // Never `Unauthenticated`: these are the gear's credentials against its
        // own backend, not the caller's against cluster (DESIGN section 6.9).
        (
            ClusterError::Provider {
                kind: ProviderErrorKind::AuthFailure,
                message: "password authentication failed".to_owned(),
            },
            Code::Internal,
        ),
        (ClusterError::ProfileNotSpecified, Code::InvalidArgument),
    ];

    for (error, expected) in cases {
        assert_eq!(
            super::to_status(error.clone()).code(),
            expected,
            "wrong gRPC code for {error:?}"
        );
    }
}

/// `Shutdown` and a lost connection share a code on purpose, and are still told
/// apart after decode. The assertion is the pair, not either half.
#[cfg(feature = "grpc-client")]
#[test]
fn shutdown_and_connection_lost_share_a_code_and_stay_distinguishable() {
    use toolkit_transport_grpc::extract_problem;

    let shutdown = super::to_status(ClusterError::Shutdown);
    let lost = super::to_status(ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "reset".to_owned(),
    });
    assert_eq!(shutdown.code(), lost.code());

    let decode = |status: tonic::Status| {
        to_cluster_error(
            extract_problem::<Problem>(status.metadata())
                .expect("decodes")
                .expect("present"),
            LeaseContext::None,
        )
        .expect("an error")
    };
    assert!(!decode(shutdown).is_retryable());
    assert!(decode(lost).is_retryable());
}

// ---------------------------------------------------------------------------
// The status decoders, and the reason they do not go through `CanonicalError`
// ---------------------------------------------------------------------------

/// `from_status` is the whole client half of section 6.9, and this is the property
/// it exists for: a status the gear encoded reconstructs into the **exact**
/// variant, retryability included.
///
/// It is asserted against `to_status` rather than a hand-built `Problem` because
/// the pair is the contract; either one alone could drift.
#[cfg(feature = "grpc-client")]
#[test]
fn from_status_reconstructs_every_variant_the_gear_encodes() {
    for original in every_variant() {
        let decoded = super::from_status(&super::to_status(original.clone()));
        assert_eq!(
            format!("{decoded:?}"),
            format!("{original:?}"),
            "variant did not survive to_status -> from_status"
        );
        assert_eq!(decoded.is_retryable(), original.is_retryable());
    }
}

/// **The measured reason `from_status` reads the trailer directly.**
///
/// The generated gRPC client's error type is `CanonicalError`, and the hop into
/// it is lossy in precisely the fields this codec keys on: `TryFrom<Problem> for
/// CanonicalError` keeps neither `error_domain`, nor `error_code`, nor
/// `context["data"]`. Everything cluster raises would arrive as
/// `Provider{Other}`, and `Shutdown` — terminal — would come back retryable.
///
/// This test pins the defect so the day it is fixed upstream is a test failure
/// rather than a silent opportunity missed.
#[cfg(feature = "grpc-client")]
#[test]
fn the_canonical_error_hop_is_lossy_which_is_why_it_is_not_taken() {
    use toolkit_canonical_errors::CanonicalError;

    let original = ClusterError::LockContended {
        name: "orders".to_owned(),
    };
    let status = super::to_status(original.clone());

    // The path this codec takes: straight off the trailer.
    assert_eq!(
        format!("{:?}", super::from_status(&status)),
        format!("{original:?}"),
        "the direct path must reconstruct the variant"
    );

    // The path the generated client takes.
    let canonical = CanonicalError::from(toolkit_contract::grpc::map_tonic_status(&status));
    let round_tripped = Problem::from(canonical);
    assert_eq!(
        round_tripped.error_code, None,
        "CanonicalError drops error_code - if this ever fails, the generated \
         client became usable and this codec can be simplified"
    );
    assert_eq!(round_tripped.error_domain, None, "and error_domain");
    let via_canonical =
        to_cluster_error(round_tripped, LeaseContext::None).expect("still an error");
    assert!(
        matches!(
            via_canonical,
            ClusterError::Provider {
                kind: ProviderErrorKind::Other,
                ..
            }
        ),
        "the typed variant is gone, got: {via_canonical:?}"
    );
}

/// A status with no problem envelope is the channel being down, and the exit
/// criterion says what that must become: `Provider{ConnectionLost}`, retryable, so
/// an unreachable cluster gear recovers like an unreachable Postgres.
#[cfg(feature = "grpc-client")]
#[test]
fn a_status_without_an_envelope_is_a_retryable_connection_loss() {
    let bare = tonic::Status::unavailable("connection reset");
    let decoded = super::from_status(&bare);
    assert!(
        matches!(
            decoded,
            ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                ..
            }
        ),
        "expected Provider{{ConnectionLost}}, got: {decoded:?}"
    );
    assert!(decoded.is_retryable());
}

/// Release-by-absence is the only `None`, and only for the release context.
#[cfg(feature = "grpc-client")]
#[test]
fn only_a_lease_release_decodes_to_no_error_at_all() {
    // A bare canonical `NotFound` - one from an intermediary that did not type its
    // error, which is the case `LeaseContext` defends against (section 6.9).
    let bare = tonic::Status::not_found("no such row");

    assert!(
        super::from_lease_status(&bare, LeaseContext::LeaseRelease).is_none(),
        "a release that matched nothing achieved what its caller wanted"
    );
    assert!(matches!(
        super::from_lease_status(&bare, LeaseContext::LockRenew { name: "ledger" }),
        Some(ClusterError::LockExpired { .. })
    ));
    assert!(
        super::from_lease_status(&bare, LeaseContext::None).is_some(),
        "and everything else is still an error"
    );
}

// ---------------------------------------------------------------------------
// An oversized envelope, and the codes with no envelope at all (`ERR-1`, `SEAM-4`)
// ---------------------------------------------------------------------------

/// **`ERR-1`, the property.** A payload that does not fit the problem trailer
/// must not change the *answer*, and above all must not make a non-retryable
/// error retryable — `RetryPolicy::default()` sets `max_retries: None`, so a
/// wrongly-retryable CAS conflict is an infinite loop against a write that will
/// keep conflicting (§6.10).
///
/// The audit measured six variants flipping at 4000-byte payloads, all in the
/// same direction. Every one of them is here.
#[cfg(feature = "grpc-client")]
#[test]
fn an_oversized_payload_never_makes_an_error_retryable() {
    let big = "x".repeat(4000);
    let cases = vec![
        ClusterError::CasConflict {
            key: "counter".to_owned(),
            current: Some(CacheEntry {
                value: big.clone().into_bytes(),
                version: 7,
            }),
        },
        ClusterError::InvalidConfig {
            reason: big.clone(),
        },
        ClusterError::InvalidName {
            name: big.clone(),
            reason: "too long",
        },
        ClusterError::LockContended { name: big.clone() },
        ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            message: big.clone(),
        },
        ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: big,
        },
    ];

    for original in cases {
        assert!(
            !original.is_retryable(),
            "test setup: {original:?} is retryable before the hop"
        );
        let decoded = super::from_status(&super::to_status(original.clone()));
        assert!(
            !decoded.is_retryable(),
            "an oversized {original:?} came back retryable as {decoded:?} - a consumer would \
             retry it forever"
        );
    }
}

/// **`ERR-1`'s CAS half, in full.** §6.9 decision 17a makes `current` a SHOULD
/// "if cheaply obtainable", so the server may decline to ship the value — but it
/// may not lose the *variant*, which is what the CAS retry loop branches on.
///
/// So an oversized conflict degrades exactly one step: `CasConflict{current:
/// Some}` becomes `CasConflict{current: None}`, and the caller re-reads. It does
/// not become `Provider{ConnectionLost}`.
#[cfg(feature = "grpc-client")]
#[test]
fn an_oversized_cas_conflict_sheds_its_value_and_keeps_its_variant() {
    let small = super::from_status(&super::to_status(ClusterError::CasConflict {
        key: "counter".to_owned(),
        current: Some(CacheEntry {
            value: b"41".to_vec(),
            version: 7,
        }),
    }));
    assert!(
        matches!(&small, ClusterError::CasConflict { key, current: Some(entry) }
            if key == "counter" && entry.version == 7 && entry.value == b"41"),
        "a conflict that fits must still carry its entry, got {small:?}"
    );

    let large = super::from_status(&super::to_status(ClusterError::CasConflict {
        key: "counter".to_owned(),
        current: Some(CacheEntry {
            value: vec![b'x'; 4000],
            version: 7,
        }),
    }));
    assert!(
        matches!(&large, ClusterError::CasConflict { key, current: None } if key == "counter"),
        "a conflict too large for the trailer degrades to version-absent, not to a \
         connection loss - got {large:?}"
    );
    assert!(!large.is_retryable());
}

/// **`SEAM-4`.** Every `tonic::Code` that can reach the untyped path, and the
/// verdict it must produce. Written out rather than derived, so a change to the
/// classification is a change to this test.
///
/// The two halves matter for different reasons. The retryable half is
/// **unchanged** behaviour and is asserted so a future edit cannot quietly make
/// a transient failure terminal — that would strand a consumer on a blip. The
/// non-retryable half is the fix: each of these means the server answered, and a
/// second identical request gets the same answer.
#[cfg(feature = "grpc-client")]
#[test]
fn every_untyped_grpc_code_has_an_explicit_verdict() {
    use tonic::{Code, Status};

    // §6.9's "channel down, pod gone". Retryable, exactly as before the fix.
    for code in [
        Code::Unavailable,
        Code::DeadlineExceeded,
        Code::Cancelled,
        Code::ResourceExhausted,
        Code::Unknown,
    ] {
        let decoded = super::from_status(&Status::new(code, "no envelope"));
        assert!(
            matches!(
                decoded,
                ClusterError::Provider {
                    kind: ProviderErrorKind::ConnectionLost,
                    ..
                }
            ),
            "{code:?} must stay a retryable connection loss, got {decoded:?}"
        );
        assert!(decoded.is_retryable(), "{code:?}");
    }

    // §6.11's rolling-deployment skew: the method does not exist on the peer,
    // and the design's table maps it to `Unsupported`, not retryable.
    let skew = super::from_status(&Status::new(Code::Unimplemented, "unknown service"));
    assert!(
        matches!(skew, ClusterError::Unsupported { .. }),
        "an `Unimplemented` is a version skew, got {skew:?}"
    );
    assert!(!skew.is_retryable());

    // The server answered and said no. Retrying sends the same bytes.
    for code in [
        Code::Aborted,
        Code::FailedPrecondition,
        Code::InvalidArgument,
        Code::OutOfRange,
        Code::AlreadyExists,
        Code::PermissionDenied,
        Code::Unauthenticated,
        Code::Internal,
        Code::DataLoss,
    ] {
        let decoded = super::from_status(&Status::new(code, "no envelope"));
        assert!(
            !decoded.is_retryable(),
            "{code:?} must not be retryable, got {decoded:?}"
        );
        // Never `AuthFailure`: §6.9 reserves that for the gear's own credentials
        // against its backend, and pointing an operator at the wrong credential
        // is the mistake that variant exists to prevent.
        assert!(
            !matches!(
                decoded,
                ClusterError::Provider {
                    kind: ProviderErrorKind::AuthFailure,
                    ..
                }
            ),
            "{code:?} must not be reported as the gear's own auth failure"
        );
    }

    // And `NotFound` still belongs to `LeaseContext`, untouched by the table.
    assert!(
        super::from_lease_status(
            &Status::not_found("no such row"),
            LeaseContext::LeaseRelease
        )
        .is_none()
    );
}

// ---------------------------------------------------------------------------
// The watch-event decoders
// ---------------------------------------------------------------------------

#[test]
fn a_cache_watch_event_round_trips_through_its_flat_wire_form() {
    use crate::cache::{CacheEvent, CacheWatchEvent};
    use crate::dto::{CacheWatchEventDto, CacheWatchEventKind};

    let changed = super::to_cache_watch_event(CacheWatchEventDto::key_event(
        CacheWatchEventKind::Changed,
        "ledger".to_owned(),
    ));
    assert!(matches!(
        changed,
        CacheWatchEvent::Event(CacheEvent::Changed { ref key }) if key == "ledger"
    ));

    let lagged = super::to_cache_watch_event(CacheWatchEventDto::lagged(7));
    assert!(matches!(lagged, CacheWatchEvent::Lagged { dropped: 7 }));

    assert!(matches!(
        super::to_cache_watch_event(CacheWatchEventDto::reset()),
        CacheWatchEvent::Reset
    ));

    // A terminal event carries its error through the same codec, so a `Closed`
    // stays retryable and `RestartingWatch` resubscribes.
    let closed = super::to_cache_watch_event(CacheWatchEventDto::closed(
        crate::dto::WireError::from(ClusterWireError::from(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            message: "upstream gone".to_owned(),
        })),
    ));
    let CacheWatchEvent::Closed(error) = closed else {
        panic!("expected a terminal event");
    };
    assert!(error.is_retryable());
}

#[test]
fn a_frame_whose_payload_contradicts_its_kind_degrades_to_reset() {
    // The flat wire type cannot express "the kind decides which fields are
    // present", so a peer *can* send `Changed` with no key. Reset is the safe
    // reading - "you may have missed something, re-read" - and it is the one
    // choice that neither drops the frame nor kills the subscription.
    use crate::cache::CacheWatchEvent;
    use crate::dto::{CacheWatchEventDto, CacheWatchEventKind};
    use crate::leader::LeaderWatchEvent;

    let keyless = CacheWatchEventDto {
        kind: CacheWatchEventKind::Changed,
        key: None,
        dropped: None,
        error: None,
    };
    assert!(matches!(
        super::to_cache_watch_event(keyless),
        CacheWatchEvent::Reset
    ));

    let statusless = crate::dto::LeaderWatchEventDto {
        kind: crate::dto::LeaderWatchEventKind::Status,
        status: None,
        dropped: None,
        error: None,
    };
    assert!(matches!(
        super::to_leader_watch_event(statusless),
        LeaderWatchEvent::Reset
    ));
}

#[test]
fn a_terminal_watch_event_with_no_error_is_retryable_rather_than_fatal() {
    // A protocol violation neither side can describe. Retryable, so a
    // subscription lost to one malformed frame is recoverable; the alternative
    // strands the consumer permanently on one bad message.
    use crate::cache::CacheWatchEvent;
    use crate::dto::{CacheWatchEventDto, CacheWatchEventKind};

    let errorless = CacheWatchEventDto {
        kind: CacheWatchEventKind::Closed,
        key: None,
        dropped: None,
        error: None,
    };
    let CacheWatchEvent::Closed(error) = super::to_cache_watch_event(errorless) else {
        panic!("expected a terminal event");
    };
    assert!(error.is_retryable());
}

#[test]
fn a_leader_watch_event_round_trips_through_its_flat_wire_form() {
    use crate::dto::{LeaderStatusDto, LeaderWatchEventDto};
    use crate::leader::{LeaderStatus, LeaderWatchEvent};

    assert!(matches!(
        super::to_leader_watch_event(LeaderWatchEventDto::status(LeaderStatusDto::Lost)),
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        super::to_leader_watch_event(LeaderWatchEventDto::lagged(3)),
        LeaderWatchEvent::Lagged { dropped: 3 }
    ));
    assert!(matches!(
        super::to_leader_watch_event(LeaderWatchEventDto::reset()),
        LeaderWatchEvent::Reset
    ));

    // The one the shutdown sequence delivers (section 4.8).
    let closed = super::to_leader_watch_event(LeaderWatchEventDto::closed(
        crate::dto::WireError::from(ClusterWireError::from(ClusterError::Shutdown)),
    ));
    let LeaderWatchEvent::Closed(error) = closed else {
        panic!("expected a terminal event");
    };
    assert!(
        !error.is_retryable(),
        "a shutdown must stay terminal, or auto-restart retries it forever"
    );
}
