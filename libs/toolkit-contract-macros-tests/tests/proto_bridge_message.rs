//! Runtime coverage for `#[derive(ProtoBridge)]` `message` mode — the fallible
//! decode path (`TryFromProto`) that commit `5f39789ef` added without tests.
//!
//! trybuild `pass` fixtures only *compile*; they never run, so the behavioural
//! guarantees of the generated code (an absent required message is an `Err`, a
//! present one recurses fallibly all the way down) have to be asserted in a real,
//! executed integration test. This is that test.
//!
//! It covers the four field shapes the derive distinguishes — plain / `Option` /
//! `Vec` / required `message` — and, crucially, the **two-level** nested case that
//! is the whole point of H1: the pre-fix `try_from_proto` only guaranteed the
//! error one level deep because it delegated to the infallible `From` (whose
//! required-message arm is `.unwrap_or_default()`) for anything nested.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use toolkit_contract::ProtoBridge;
use toolkit_contract::grpc_repr::ProtoDecodeError;
use uuid::Uuid;

/// prost-shaped stand-ins: message-typed fields are `Option<T>`, `repeated` is
/// `Vec<T>`, and everything is `Default` because prost derives it.
mod stubs {
    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct Deep {
        pub value: i64,
    }

    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct Inner {
        /// A required message *one level down* — the field H1 is about.
        pub deep: Option<Deep>,
    }

    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct Outer {
        /// Required message (level 1). Its own `deep` is the level-2 required
        /// message the shallow guarantee misses.
        pub inner: Option<Inner>,
        /// Optional message: absent is legitimately `None`, not an error.
        pub maybe: Option<Deep>,
        /// `repeated` message.
        pub many: Vec<Deep>,
        /// Plain scalar.
        pub plain: i64,
        /// `via_string` scalar — exercises the other `ProtoDecodeError` variant.
        pub id: String,
    }
}

#[derive(Debug, Clone, Default, PartialEq, ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::Deep")]
struct Deep {
    value: i64,
}

#[derive(Debug, Clone, Default, PartialEq, ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::Inner")]
struct Inner {
    #[proto_bridge(message)]
    deep: Deep,
}

#[derive(Debug, Clone, Default, PartialEq, ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::Outer")]
struct Outer {
    #[proto_bridge(message)]
    inner: Inner,
    #[proto_bridge(message)]
    maybe: Option<Deep>,
    #[proto_bridge(message)]
    many: Vec<Deep>,
    plain: i64,
    #[proto_bridge(via_string)]
    id: Uuid,
}

fn full_outer() -> Outer {
    Outer {
        inner: Inner {
            deep: Deep { value: 7 },
        },
        maybe: Some(Deep { value: 8 }),
        many: vec![Deep { value: 9 }, Deep { value: 10 }],
        plain: 3,
        id: Uuid::nil(),
    }
}

/// Happy path across all four field shapes: plain, `Option`, `Vec`, and required
/// `message` all survive a round trip through the fallible decoder.
#[test]
fn all_field_shapes_round_trip_through_try_from_proto() {
    let original = full_outer();
    let proto: stubs::Outer = original.clone().into();
    let decoded = Outer::try_from_proto(&proto).expect("a well-formed message decodes");
    assert_eq!(decoded, original);
}

/// `Option` message shape: an absent optional message is `None`, not an error.
#[test]
fn an_absent_optional_message_is_none_not_an_error() {
    let proto = stubs::Outer {
        inner: Some(stubs::Inner {
            deep: Some(stubs::Deep { value: 1 }),
        }),
        maybe: None,
        many: vec![],
        plain: 0,
        id: Uuid::nil().to_string(),
    };
    let decoded = Outer::try_from_proto(&proto).expect("an absent optional message is fine");
    assert_eq!(decoded.maybe, None);
    assert!(decoded.many.is_empty());
}

/// `Vec` (repeated) message shape: every element decodes fallibly, element-wise.
#[test]
fn a_repeated_message_decodes_element_wise() {
    let proto: stubs::Outer = full_outer().into();
    let decoded = Outer::try_from_proto(&proto).unwrap();
    assert_eq!(decoded.many, vec![Deep { value: 9 }, Deep { value: 10 }]);
}

/// A required message absent at the **top** level is an error — this already
/// held before H1, one level deep.
#[test]
fn a_top_level_absent_required_message_is_an_error() {
    let proto = stubs::Outer {
        inner: None,
        ..Default::default()
    };
    let err = Outer::try_from_proto(&proto).expect_err("an absent required message is an error");
    match err {
        ProtoDecodeError::MissingMessage(missing) => assert_eq!(missing.field, "inner"),
        other @ ProtoDecodeError::ViaString(_) => {
            panic!("expected MissingMessage(inner), got {other:?}")
        }
    }
}

/// **H1 crux — the non-vacuity assertion.**
///
/// `inner` is present but its own required `deep` is absent, so the missing
/// message lives *two* levels down. Before the fix, `try_from_proto`'s required
/// arm was `.map(Into::into).ok_or(..)?` — `Into::into` is the **infallible**
/// `From`, whose required-message arm is `.unwrap_or_default()`. So the top-level
/// `.map` succeeded (the message was present) and the nested absence silently
/// became `Deep::default()`: an `Ok`, not an `Err`.
///
/// The two assertions below pin exactly that contrast:
///  1. the infallible `From` path — the same one-level-down mechanism the buggy
///     `try_from_proto` delegated to — still zeroes the nested field to a default
///     and reports no error;
///  2. the fixed fallible path recurses through `try_from_proto_wire` and reports
///     the absence as `MissingMessage { field: "deep" }`.
///
/// Before the fix, assertion (2) would observe the same zeroed default that
/// assertion (1) does. That divergence is the fix.
#[test]
fn a_nested_absent_required_message_is_an_error_not_a_default() {
    let proto = stubs::Outer {
        inner: Some(stubs::Inner { deep: None }),
        maybe: None,
        many: vec![],
        plain: 0,
        id: Uuid::nil().to_string(),
    };

    // (1) The infallible `From` silently zeroes a nested absent required message.
    // `Outer` carries a `via_string` field and so gets no infallible `From<Proto>`;
    // `Inner` exercises the very same required-message arm one level down, is what
    // the buggy `try_from_proto` delegated to, and still has its infallible `From`.
    let defaulted: Inner = stubs::Inner { deep: None }.into();
    assert_eq!(
        defaulted.deep,
        Deep::default(),
        "the infallible From defaults the nested absent message \u{2014} this is the \
         behaviour try_from_proto wrongly inherited before H1",
    );

    // (2) The fallible path now reports it instead of inheriting that default.
    let err = Outer::try_from_proto(&proto)
        .expect_err("a nested absent required message must be a decode error, not a default");
    match err {
        ProtoDecodeError::MissingMessage(missing) => assert_eq!(missing.field, "deep"),
        other @ ProtoDecodeError::ViaString(_) => {
            panic!("expected MissingMessage(deep), got {other:?}")
        }
    }
}

/// M1: the two failure modes are distinguishable **by variant**, no downcast. A
/// malformed `via_string` is `ViaString`; an absent required message is
/// `MissingMessage`.
#[test]
fn the_two_decode_failures_are_distinguishable_by_variant() {
    let bad_string = stubs::Outer {
        inner: Some(stubs::Inner {
            deep: Some(stubs::Deep { value: 1 }),
        }),
        id: "not-a-uuid".to_owned(),
        ..Default::default()
    };
    match Outer::try_from_proto(&bad_string) {
        Err(ProtoDecodeError::ViaString(e)) => assert_eq!(e.field, "id"),
        other => panic!("expected ViaString(id), got {other:?}"),
    }

    let missing = stubs::Outer {
        inner: None,
        id: Uuid::nil().to_string(),
        ..Default::default()
    };
    match Outer::try_from_proto(&missing) {
        Err(ProtoDecodeError::MissingMessage(e)) => assert_eq!(e.field, "inner"),
        other => panic!("expected MissingMessage(inner), got {other:?}"),
    }
}
