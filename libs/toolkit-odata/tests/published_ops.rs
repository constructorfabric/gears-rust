#![allow(clippy::unwrap_used, clippy::expect_used)]

//! A field that names the operators it serves narrows the parser too.
//!
//! `FilterField::published_ops` is what `OperationBuilder::with_odata_filter`
//! publishes in `x-odata-filter.allowedFields`; the parser refuses the
//! operators the list leaves out, so the contract and the endpoint agree.

use toolkit_odata::filter::{
    FieldKind, FilterError, FilterField, FilterOp, parse_odata_filter, published_ops_error,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Code,
    Stars,
    Memo,
}

impl FilterField for Field {
    const FIELDS: &'static [Self] = &[Self::Code, Self::Stars, Self::Memo];

    fn name(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Stars => "stars",
            Self::Memo => "memo",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Code | Self::Memo => FieldKind::String,
            Self::Stars => FieldKind::I64,
        }
    }

    fn published_ops(&self) -> Option<&'static [FilterOp]> {
        match self {
            Self::Code => Some(&[FilterOp::Eq, FilterOp::In]),
            Self::Stars => Some(&[FilterOp::Ge]),
            Self::Memo => Some(&[FilterOp::Ne]),
        }
    }

    fn nullable(&self) -> bool {
        matches!(self, Self::Memo)
    }
}

fn refused_as_unserved(filter: &str) {
    match parse_odata_filter::<Field>(filter) {
        Err(FilterError::UnsupportedOperation(message)) => {
            assert!(
                message.ends_with("; the endpoint does not serve it"),
                "{filter}: {message}"
            );
        }
        other => panic!("{filter} must be refused as unserved, got {other:?}"),
    }
}

#[test]
fn the_listed_operators_are_accepted() {
    for filter in [
        "code eq 'x'",
        "code in ('x','y')",
        "stars ge 3",
        "memo ne 'x'",
        "memo ne null",
    ] {
        assert!(
            parse_odata_filter::<Field>(filter).is_ok(),
            "{filter} must be accepted"
        );
    }
}

#[test]
fn an_operator_the_list_leaves_out_is_refused() {
    refused_as_unserved("code ne 'x'");
    refused_as_unserved("contains(code,'x')");
    refused_as_unserved("startswith(code,'x')");
    refused_as_unserved("endswith(code,'x')");
    refused_as_unserved("stars eq 3");
    refused_as_unserved("stars gt 3");
    refused_as_unserved("stars in (1,2)");
    refused_as_unserved("memo eq 'x'");
    refused_as_unserved("memo in ('x')");
    refused_as_unserved("memo eq null");
}

#[test]
fn a_list_never_widens_past_the_kind() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct Widened;

    impl FilterField for Widened {
        const FIELDS: &'static [Self] = &[Self];

        fn name(&self) -> &'static str {
            "code"
        }

        fn kind(&self) -> FieldKind {
            FieldKind::String
        }

        fn published_ops(&self) -> Option<&'static [FilterOp]> {
            Some(&[FilterOp::Eq, FilterOp::Gt])
        }
    }

    assert!(!Widened.serves(FilterOp::Gt));
    assert!(matches!(
        parse_odata_filter::<Widened>("code gt 'x'"),
        Err(FilterError::UnsupportedOperation(message)) if message.ends_with("of type String")
    ));
    assert_eq!(
        published_ops_error::<Widened>().as_deref(),
        Some("field `code` publishes `gt`, which its kind String does not allow")
    );
}

#[test]
fn a_sound_declaration_has_no_error() {
    assert_eq!(published_ops_error::<Field>(), None);
}
