use crate::descriptor::ContractDescriptor;
use crate::ir::contract::ContractIr;

/// Connects a `dyn Trait` type to its contract descriptor and IR.
pub trait Contract: Send + Sync + 'static {
    /// Returns the static descriptor for fast runtime lookups.
    fn descriptor() -> &'static ContractDescriptor;

    /// Builds the full Contract IR.
    fn contract_ir() -> ContractIr;
}

pub use Contract as ServiceContract;

/// The `version` a contract trait declares in `#[toolkit::contract]`.
///
/// Implemented by the attribute for `dyn Trait`, as [`Contract`] is. A separate
/// trait so that hand-written [`Contract`] impls do not have to name a version.
pub trait ContractVersion {
    /// The declared version, e.g. `"v1"`.
    const VERSION: &'static str;
}

/// Compile-time check that a gRPC `package` names the same major version as
/// the contract it projects: `billing.v2` for `version = "v2"`. A package whose
/// last segment is not `v<digits>` is not checked.
///
/// Called by `#[toolkit::grpc_contract]` in a `const _: () = ...;`, so a
/// mismatch is a compile error.
///
/// # Panics
/// At compile time, when the versions differ.
pub const fn assert_grpc_package_major<C: ?Sized + ContractVersion>(package: &str) {
    let pkg = package.as_bytes();
    let mut start = pkg.len();
    while start > 0 && pkg[start - 1] != b'.' {
        start -= 1;
    }
    // Last segment must be `v` followed by at least one digit.
    if pkg.len() - start < 2 || pkg[start] != b'v' {
        return;
    }
    let mut i = start + 1;
    while i < pkg.len() {
        if !pkg[i].is_ascii_digit() {
            return;
        }
        i += 1;
    }
    let version = C::VERSION.as_bytes();
    let mut major_len = 0;
    while major_len < version.len() && version[major_len] != b'.' {
        major_len += 1;
    }
    let segment_len = pkg.len() - start;
    let mut same = major_len == segment_len;
    let mut j = 0;
    while same && j < segment_len {
        same = pkg[start + j] == version[j];
        j += 1;
    }
    assert!(
        same,
        "gRPC package major version does not match the contract's `version`: \
         a `vN` package must equal #[toolkit::contract(version = \"vN\")]"
    );
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod version_tests {
    use super::*;

    struct V1;
    impl ContractVersion for V1 {
        const VERSION: &'static str = "v1";
    }
    struct V2;
    impl ContractVersion for V2 {
        const VERSION: &'static str = "v2";
    }

    const _: () = assert_grpc_package_major::<V1>("billing.payment.v1");
    const _: () = assert_grpc_package_major::<V2>("v2");
    const _: () = assert_grpc_package_major::<V1>("billing.payment");
    const _: () = assert_grpc_package_major::<V1>("billing.vendor");
    const _: () = assert_grpc_package_major::<V1>("");

    #[test]
    #[should_panic(expected = "major version")]
    fn a_mismatched_major_panics() {
        assert_grpc_package_major::<V1>(std::hint::black_box("billing.v2"));
    }

    #[test]
    #[should_panic(expected = "major version")]
    fn v1_is_not_v10() {
        assert_grpc_package_major::<V1>(std::hint::black_box("billing.v10"));
    }
}
