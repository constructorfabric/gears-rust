#![allow(clippy::unwrap_used, clippy::expect_used)]

//! `null` compares with `eq` and `ne` on a field that declares itself nullable, of any kind, and
//! nowhere else.
//!
//! `field eq null` asks for the rows where the field is absent and `field ne null` for the rows
//! where it is present, whatever the field's kind: absence is not a value of the kind, so the
//! value check does not apply to it. An ordering comparison with `null` has no answer, and an
//! `in` list is a set of values, so `null` stays refused in both.
//!
//! Null admission is opt-in (`FilterField::nullable`, `false` by default): on a field that does
//! not declare it, `null` is a value outside the field's kind and is refused as a type mismatch,
//! as it always was.

use toolkit_odata::filter::{
    FieldKind, FilterError, FilterField, FilterNode, FilterOp, ODataValue, parse_odata_filter,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum Field {
    Name,
    Count,
    Ratio,
    Flag,
    Parent,
    At,
    Day,
    Clock,
    Amount,
}

impl FilterField for Field {
    const FIELDS: &'static [Self] = &[
        Self::Name,
        Self::Count,
        Self::Ratio,
        Self::Flag,
        Self::Parent,
        Self::At,
        Self::Day,
        Self::Clock,
        Self::Amount,
    ];

    fn name(&self) -> &'static str {
        self.into()
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Name => FieldKind::String,
            Self::Count => FieldKind::I64,
            Self::Ratio => FieldKind::F64,
            Self::Flag => FieldKind::Bool,
            Self::Parent => FieldKind::Uuid,
            Self::At => FieldKind::DateTimeUtc,
            Self::Day => FieldKind::Date,
            Self::Clock => FieldKind::Time,
            Self::Amount => FieldKind::Decimal,
        }
    }

    fn nullable(&self) -> bool {
        true
    }
}

/// A nullable field of every kind admits `eq null` and `ne null`.
#[test]
fn eq_and_ne_null_are_accepted_on_every_kind() {
    for field in Field::FIELDS {
        for (op, expected) in [("eq", FilterOp::Eq), ("ne", FilterOp::Ne)] {
            let filter = format!("{} {op} null", field.name());
            let node = parse_odata_filter::<Field>(&filter)
                .unwrap_or_else(|e| panic!("{filter} must be accepted: {e}"));
            match node {
                FilterNode::Binary {
                    field: f,
                    op: o,
                    value: ODataValue::Null,
                } => {
                    assert_eq!(f, *field, "{filter}");
                    assert_eq!(o, expected, "{filter}");
                }
                other => panic!("{filter} must be a binary null comparison, got {other:?}"),
            }
        }
    }
}

/// Null equality combines with other terms under `and` / `or`.
#[test]
fn null_equality_composes_with_other_terms() {
    let node = parse_odata_filter::<Field>("parent eq null and not (name ne null) or count gt 2")
        .expect("a null comparison is an ordinary term");
    assert!(matches!(
        node,
        FilterNode::Composite {
            op: FilterOp::Or,
            ..
        }
    ));
}

/// `lt`/`le`/`gt`/`ge` with `null` are refused.
#[test]
fn ordering_comparisons_with_null_are_refused() {
    for field in Field::FIELDS {
        for op in ["gt", "ge", "lt", "le"] {
            let filter = format!("{} {op} null", field.name());
            let error = parse_odata_filter::<Field>(&filter)
                .expect_err(&format!("{filter} must be refused"));
            assert!(
                matches!(error, FilterError::UnsupportedOperation(_)),
                "{filter}: {error:?}"
            );
        }
    }
}

/// `null` inside `in (...)` is refused.
#[test]
fn null_inside_in_is_refused() {
    for filter in [
        "name in (null)",
        "name in ('a', null)",
        "parent in (null, 00000000-0000-0000-0000-000000000001)",
        "count in (1, null)",
    ] {
        let error = parse_odata_filter::<Field>(filter).expect_err(filter);
        assert!(
            matches!(error, FilterError::InvalidExpression(ref m) if m.contains("null")),
            "{filter}: {error:?}"
        );
    }
}

/// A nullable field still refuses a value of another kind.
#[test]
fn a_value_of_another_kind_is_still_a_type_mismatch() {
    for filter in ["parent eq 'x'", "count eq 'x'", "flag eq 1", "name eq 3"] {
        let error = parse_odata_filter::<Field>(filter).expect_err(filter);
        assert!(
            matches!(error, FilterError::TypeMismatch { .. }),
            "{filter}: {error:?}"
        );
    }
}

/// A field that does not declare itself nullable — every field a consumer wrote before null
/// equality existed, and every field `ODataFilterable` derives — always has a value: `null` is
/// refused on it as a value outside its kind, exactly as before null equality existed, with every
/// operator and inside `in`. Its consumers (an `IdP` plugin matching users, say) never see `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum Plain {
    Name,
    Id,
    Count,
}

impl FilterField for Plain {
    const FIELDS: &'static [Self] = &[Self::Name, Self::Id, Self::Count];

    fn name(&self) -> &'static str {
        self.into()
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Name => FieldKind::String,
            Self::Id => FieldKind::Uuid,
            Self::Count => FieldKind::I64,
        }
    }
}

/// A field that does not opt in refuses `null` with the same type mismatch as before.
#[test]
fn a_field_that_always_has_a_value_refuses_null_as_a_type_mismatch() {
    for field in Plain::FIELDS {
        for filter in [
            format!("{} eq null", field.name()),
            format!("{} ne null", field.name()),
            format!("not ({} eq null)", field.name()),
            format!("{} in (null)", field.name()),
        ] {
            let error = parse_odata_filter::<Plain>(&filter).expect_err(&filter);
            assert!(
                matches!(
                    error,
                    FilterError::TypeMismatch { field: ref f, ref got, .. }
                        if f == field.name() && got == "null"
                ),
                "{filter}: {error:?}"
            );
        }
    }
    let error = parse_odata_filter::<Plain>("count gt null").expect_err("count gt null");
    assert!(
        matches!(error, FilterError::TypeMismatch { .. }),
        "an ordering with null on a field that always has a value: {error:?}"
    );
}
