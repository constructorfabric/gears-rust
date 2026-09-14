//! The one place four packages costs anything.
//!
//! Each contract projects into its own proto package (see the [module
//! docs](super) for why it cannot be one shared `cluster.v1`), and four messages
//! are reachable from two contracts each:
//!
//! | DTO | Packages | `ProtoBridge` home | Bridged here |
//! |---|---|---|---|
//! | [`LeaseToken`] | lock, leader | `cluster.lock.v1` | `cluster.leader.v1` |
//! | [`LeaseRef`] | lock, leader | `cluster.lock.v1` | `cluster.leader.v1` |
//! | [`RenewResponse`] | lock, leader | `cluster.lock.v1` | `cluster.leader.v1` |
//! | [`WireError`] | cache, leader | `cluster.cache.v1` | `cluster.leader.v1` |
//!
//! `#[derive(ProtoBridge)]` takes a single `stub =` path, so it can generate the
//! conversion for one package's copy. The second copy is written out here.
//!
//! **The DTO stays the single source of truth**, which keeps this
//! mechanical: every impl below is a field-for-field move, and adding a field to
//! one of these four DTOs is a compile error here until it is carried across. The
//! alternative — renaming the DTOs per contract so nothing is shared — would push
//! `LockLeaseToken` / `ElectionLeaseToken` into the SDK's surface for a type §12.1
//! defines as one thing.

use toolkit_contract::grpc_repr::{MissingRequiredMessage, ProtoDecodeError, TryFromProto};

use crate::dto::{LeaseRef, LeaseToken, RenewResponse, WireError};
use crate::grpc::stubs::leader;

// ---------------------------------------------------------------------------
// LeaseToken
// ---------------------------------------------------------------------------

impl From<LeaseToken> for leader::LeaseToken {
    fn from(value: LeaseToken) -> Self {
        Self {
            name: value.name,
            owner: value.owner,
            fence: value.fence,
        }
    }
}

impl From<leader::LeaseToken> for LeaseToken {
    fn from(value: leader::LeaseToken) -> Self {
        Self {
            name: value.name,
            owner: value.owner,
            fence: value.fence,
        }
    }
}

impl TryFromProto<leader::LeaseToken> for LeaseToken {
    fn try_from_proto_wire(proto: leader::LeaseToken) -> Result<Self, ProtoDecodeError> {
        Ok(Self::from(proto))
    }
}

// ---------------------------------------------------------------------------
// LeaseRef
// ---------------------------------------------------------------------------

impl From<LeaseRef> for leader::LeaseRef {
    fn from(value: LeaseRef) -> Self {
        Self {
            profile: value.profile,
            token: Some(value.token.into()),
            ttl_ms: value.ttl_ms,
            client_request_id: value.client_request_id,
        }
    }
}

impl From<leader::LeaseRef> for LeaseRef {
    fn from(value: leader::LeaseRef) -> Self {
        Self {
            profile: value.profile,
            token: value.token.map(Into::into).unwrap_or_default(),
            ttl_ms: value.ttl_ms,
            client_request_id: value.client_request_id,
        }
    }
}

impl TryFromProto<leader::LeaseRef> for LeaseRef {
    /// A `LeaseRef` with no token is the whole authority of the operation missing,
    /// so it is a decode error rather than a defaulted token that would fail the
    /// server's predicate later with a less useful message.
    fn try_from_proto_wire(proto: leader::LeaseRef) -> Result<Self, ProtoDecodeError> {
        let token =
            proto
                .token
                .ok_or(ProtoDecodeError::MissingMessage(MissingRequiredMessage {
                    field: "token",
                }))?;
        Ok(Self {
            profile: proto.profile,
            token: token.into(),
            ttl_ms: proto.ttl_ms,
            client_request_id: proto.client_request_id,
        })
    }
}

// ---------------------------------------------------------------------------
// RenewResponse
// ---------------------------------------------------------------------------

impl From<RenewResponse> for leader::RenewResponse {
    fn from(value: RenewResponse) -> Self {
        Self {
            generation: value.generation,
        }
    }
}

impl From<leader::RenewResponse> for RenewResponse {
    fn from(value: leader::RenewResponse) -> Self {
        Self {
            generation: value.generation,
        }
    }
}

impl TryFromProto<leader::RenewResponse> for RenewResponse {
    fn try_from_proto_wire(proto: leader::RenewResponse) -> Result<Self, ProtoDecodeError> {
        Ok(Self::from(proto))
    }
}

// ---------------------------------------------------------------------------
// WireError
// ---------------------------------------------------------------------------

impl From<WireError> for leader::WireError {
    fn from(value: WireError) -> Self {
        Self {
            error_domain: value.error_domain,
            error_code: value.error_code,
            detail: value.detail,
            data: value.data,
        }
    }
}

impl From<leader::WireError> for WireError {
    fn from(value: leader::WireError) -> Self {
        Self {
            error_domain: value.error_domain,
            error_code: value.error_code,
            detail: value.detail,
            data: value.data,
        }
    }
}

impl TryFromProto<leader::WireError> for WireError {
    fn try_from_proto_wire(proto: leader::WireError) -> Result<Self, ProtoDecodeError> {
        Ok(Self::from(proto))
    }
}

#[cfg(test)]
mod tests {
    use toolkit_contract::grpc_repr::ProtoDecodeError;

    use super::{LeaseRef, LeaseToken, TryFromProto, leader};

    fn token() -> LeaseToken {
        LeaseToken {
            name: "ledger".to_owned(),
            owner: "client-7".to_owned(),
            fence: 42,
        }
    }

    /// The whole point of this module: a `LeaseToken` must survive a hop through
    /// the *leader* package's copy, not only the lock package's.
    #[test]
    fn lease_token_round_trips_through_the_leader_package() {
        let original = token();
        let decoded = LeaseToken::from(leader::LeaseToken::from(original.clone()));
        assert_eq!(decoded, original);
    }

    #[test]
    fn lease_ref_round_trips_through_the_leader_package() {
        let original = LeaseRef {
            profile: "orders".to_owned(),
            token: token(),
            ttl_ms: Some(30_000),
            client_request_id: None,
        };
        let decoded = LeaseRef::from(leader::LeaseRef::from(original.clone()));
        assert_eq!(decoded, original);
    }

    /// The fallible surface reports an absent token rather than defaulting it —
    /// the infallible `From` cannot, so the client uses `TryFromProto`.
    ///
    /// M1: the failure is reported as the `MissingMessage` variant, so a caller
    /// distinguishes an absent required message from a malformed `via_string`
    /// value **by matching the enum** — no `Box<dyn Error>` downcast.
    #[test]
    fn a_lease_ref_with_no_token_is_a_decode_error() {
        let headless = leader::LeaseRef {
            profile: "orders".to_owned(),
            token: None,
            ttl_ms: None,
            client_request_id: None,
        };
        let err = LeaseRef::try_from_proto_wire(headless).expect_err("a missing token is an error");
        match err {
            ProtoDecodeError::MissingMessage(missing) => assert_eq!(missing.field, "token"),
            ProtoDecodeError::ViaString(other) => {
                panic!("expected MissingMessage, got a string-parse error: {other}")
            }
        }
    }
}
