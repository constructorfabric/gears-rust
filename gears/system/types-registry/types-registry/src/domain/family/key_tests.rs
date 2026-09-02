//! Family-key derivation. Pure.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::family_key;
use toolkit_gts::gts_id;

fn key(id: &str) -> String {
    family_key(&gts::GtsId::try_new(id).unwrap_or_else(|e| panic!("{id}: {e}"))).to_string()
}

/// Every version of one logical type is one family: the major, any of its minors,
/// and the next major all derive the same key.
#[test]
fn every_version_of_one_type_derives_one_key() {
    let expected = "gts.acme.crm.customer.type";
    for id in [
        gts_id!("acme.crm.customer.type.v1~"),
        gts_id!("acme.crm.customer.type.v1.4~"),
        gts_id!("acme.crm.customer.type.v2~"),
        gts_id!("acme.crm.customer.type.v0~"),
    ] {
        assert_eq!(key(id), expected, "for {id}");
    }
}

/// A minor in a **preceding** segment survives verbatim: it names the base that
/// was derived from, which is part of this entity's identity and not a version of
/// it. Two derivations from different minors of one base are different families.
#[test]
fn a_preceding_segment_minor_survives_verbatim() {
    assert_eq!(
        key(gts_id!(
            "acme.crm.customer.type.v1.4~acme.crm.premium.type.v2~"
        )),
        "gts.acme.crm.customer.type.v1.4~acme.crm.premium.type",
    );
    assert_ne!(
        key(gts_id!(
            "acme.crm.customer.type.v1.4~acme.crm.premium.type.v1~"
        )),
        key(gts_id!(
            "acme.crm.customer.type.v1.5~acme.crm.premium.type.v1~"
        )),
    );
}

/// The key is not a GTS Identifier — it has no trailing `~` and would not parse —
/// which is why `database.sql` says it MUST NOT be parsed as one.
#[test]
fn the_key_is_not_itself_an_identifier() {
    let derived = key(gts_id!("acme.crm.customer.type.v1~"));
    assert!(!derived.ends_with('~'));
    assert!(
        gts::GtsId::try_new(&derived).is_err(),
        "a family key must not round-trip as an identifier",
    );
}

/// An Instance and the Type Schema it conforms to are the same family: the
/// Instance's last segment loses its version the same way.
#[test]
fn an_instance_shares_the_family_of_its_own_last_segment() {
    assert_eq!(
        key(gts_id!("acme.crm.customer.type.v1~acme.crm.ns.thing.v1")),
        "gts.acme.crm.customer.type.v1~acme.crm.ns.thing",
    );
}

/// A type token beginning with `v` and digits is not a version suffix. String
/// surgery on the last `.v` would truncate this key; deriving from the parsed
/// segments does not.
#[test]
fn a_type_token_that_looks_like_a_version_is_not_stripped() {
    assert_eq!(
        key(gts_id!("acme.crm.ns.v2thing.v1~")),
        "gts.acme.crm.ns.v2thing"
    );
}

/// The lock order is total and deduplicated, which is the whole of what makes two
/// batches touching the same families unable to deadlock. Sorted output for
/// unsorted input, and one entry per distinct key however many times it is named.
#[test]
fn the_lock_order_is_sorted_and_deduplicated() {
    let keys = [
        family_key(&gts::GtsId::try_new(gts_id!("acme.crm.zeta.type.v1~")).expect("fixture")),
        family_key(&gts::GtsId::try_new(gts_id!("acme.crm.alpha.type.v3~")).expect("fixture")),
        // The same family as the first, spelled through another of its versions.
        family_key(&gts::GtsId::try_new(gts_id!("acme.crm.zeta.type.v2.1~")).expect("fixture")),
    ];
    let ordered: Vec<String> = super::lock_order(&keys)
        .into_iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        ordered,
        vec![
            "gts.acme.crm.alpha.type".to_owned(),
            "gts.acme.crm.zeta.type".to_owned(),
        ],
    );
}
