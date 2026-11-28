// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Conformance suite for [`CredStorePluginClientV2`] implementations.
//!
//! The backend contract (ADR-0006, [`crate::plugin_api`]) is small but strict:
//! `put` returns a durable, provider-assigned version; `get` returns exactly
//! the bytes of the `put` that returned a version, or `None` when it is gone;
//! `delete_key` and `destroy` are idempotent; and a backend that provides
//! `destroy` must hand out versions in order per key. The gear builds its
//! write protocol on those guarantees, so a plugin that bends one of them
//! corrupts data rather than failing loudly. This module is the executable
//! form of the contract: every plugin (the in-tree static and Vault plugins
//! as well as out-of-tree ones, such as a customer's `PostgreSQL` plugin)
//! runs the same checks against a real backend and proves it conforms.
//!
//! Enable it with the `conformance` feature of the SDK, in the plugin's
//! dev-dependencies. The generated tests also need `tokio` with the `rt` and
//! `macros` features there.
//!
//! # Running it in a plugin
//!
//! One line in a test file gives the plugin one `#[tokio::test]` per check,
//! each against a fresh plugin built by the expression:
//!
//! ```no_run
//! # #[cfg(feature = "conformance")]
//! # mod conformance_tests {
//! # use async_trait::async_trait;
//! # use credstore_v2_sdk::{
//! #     CredStorePluginClientV2, CredStorePluginError, SecretValue, StoreKey, ValueVersion,
//! # };
//! # use toolkit_security::SecurityContext;
//! # struct MyPlugin;
//! # impl MyPlugin {
//! #     fn connect() -> Self {
//! #         Self
//! #     }
//! # }
//! # #[async_trait]
//! # impl CredStorePluginClientV2 for MyPlugin {
//! #     async fn put(&self, _: &SecurityContext, _: &StoreKey, _: SecretValue)
//! #         -> Result<ValueVersion, CredStorePluginError> { unimplemented!() }
//! #     async fn get(&self, _: &SecurityContext, _: &StoreKey, _: &ValueVersion)
//! #         -> Result<Option<SecretValue>, CredStorePluginError> { unimplemented!() }
//! #     async fn delete_key(&self, _: &SecurityContext, _: &StoreKey)
//! #         -> Result<(), CredStorePluginError> { unimplemented!() }
//! # }
//! use credstore_v2_sdk::credstore_plugin_conformance;
//!
//! // Evaluated once per generated test; it may `.await` (the tests are async).
//! credstore_plugin_conformance!(MyPlugin::connect());
//! # }
//! # fn main() {}
//! ```
//!
//! The expression must yield the plugin value itself (anything that coerces
//! from `&T` to `&dyn CredStorePluginClientV2`), not an `Arc` or `Box` of it.
//! The macro defines plain functions named after the checks at the call site,
//! so wrap two invocations in two modules.
//!
//! Outer attributes written before the expression are applied to every
//! generated test. A plugin whose backend needs Docker keeps the suite out of
//! a default `cargo test` with `#[ignore]`:
//!
//! ```no_run
//! # #[cfg(feature = "conformance")]
//! # mod conformance_tests {
//! # use async_trait::async_trait;
//! # use credstore_v2_sdk::{
//! #     CredStorePluginClientV2, CredStorePluginError, SecretValue, StoreKey, ValueVersion,
//! # };
//! # use toolkit_security::SecurityContext;
//! # struct MyPlugin;
//! # impl MyPlugin {
//! #     async fn connect_to_docker_backend() -> Self {
//! #         Self
//! #     }
//! # }
//! # #[async_trait]
//! # impl CredStorePluginClientV2 for MyPlugin {
//! #     async fn put(&self, _: &SecurityContext, _: &StoreKey, _: SecretValue)
//! #         -> Result<ValueVersion, CredStorePluginError> { unimplemented!() }
//! #     async fn get(&self, _: &SecurityContext, _: &StoreKey, _: &ValueVersion)
//! #         -> Result<Option<SecretValue>, CredStorePluginError> { unimplemented!() }
//! #     async fn delete_key(&self, _: &SecurityContext, _: &StoreKey)
//! #         -> Result<(), CredStorePluginError> { unimplemented!() }
//! # }
//! credstore_v2_sdk::credstore_plugin_conformance!(
//!     #[ignore = "needs Docker; run with `-- --ignored`"]
//!     MyPlugin::connect_to_docker_backend().await
//! );
//! # }
//! # fn main() {}
//! ```
//!
//! Without the macro, call the checks directly, or run them all against one
//! plugin instance:
//!
//! ```no_run
//! # use credstore_v2_sdk::CredStorePluginClientV2;
//! # async fn demo(plugin: &dyn CredStorePluginClientV2) {
//! credstore_v2_sdk::conformance::run_all(plugin).await;
//! # }
//! # fn main() {}
//! ```
//!
//! # How the checks behave
//!
//! * Each check panics with `conformance[<check name>]: ...` on a violation
//!   and returns normally otherwise. Panic messages carry lengths and offsets,
//!   never the stored bytes.
//! * Each check builds its own correlation-only [`SecurityContext`] and uses
//!   fresh random tenant and record ids, so checks never see each other's
//!   data and can share one backend (for example a single Vault). A passing
//!   check deletes the keys it wrote at the end.
//! * The suite never compares version strings for order, because versions are
//!   opaque. Order is observed through `destroy(Below(..))` instead: a put
//!   that starts after another put on the same key has returned must get a
//!   version that `Below` treats as greater.
//! * A version the suite needs but cannot construct (a version "never issued
//!   for this key") is borrowed from another key of the same plugin, so it is
//!   always in the plugin's own format.
//! * The checks marked "destroy" below are skipped, with a `tracing` warning
//!   (install a subscriber to see it), when `destroyer()` is `None`;
//!   the suite never calls `destroy` on such a plugin.
//! * The concurrent checks drive their calls with `join_all` on the calling
//!   task, so they overlap exactly as far as the plugin awaits (network or
//!   database calls overlap, a purely synchronous in-memory plugin does not).
//!
//! # Checks
//!
//! Always run:
//!
//! | Check | What it proves |
//! |-------|----------------|
//! | [`put_get_text`] | text values (Unicode, whitespace, JSON-like, control characters) come back byte for byte, and a read does not consume the version |
//! | [`put_get_binary`] | binary values, including NUL bytes and invalid UTF-8, come back byte for byte |
//! | [`put_get_empty`] | an empty value is a value, not an absence |
//! | [`put_get_large`] | a 64 KiB value comes back byte for byte |
//! | [`puts_yield_distinct_immutable_versions`] | every `put` returns a new version (even for equal bytes) and earlier versions keep their own bytes |
//! | [`get_never_written_key_is_none`] | `get` on a key that was never written is `None`, not an error |
//! | [`get_unissued_version_is_none`] | `get` of a version never issued for an existing key is `None` |
//! | [`keys_are_isolated_across_tenants`] | the same record id under two tenants is two keys |
//! | [`keys_are_isolated_within_tenant`] | two records of one tenant are two keys |
//! | [`delete_key_removes_all_versions`] | `delete_key` removes every version |
//! | [`delete_key_is_idempotent`] | `delete_key` twice, and on a key never written, is success |
//! | [`delete_key_does_not_touch_other_keys`] | `delete_key` removes only its own key |
//! | [`concurrent_puts_yield_distinct_versions`] | concurrent puts to one key all succeed with pairwise distinct, readable versions |
//!
//! Destroy (only when `destroyer()` is `Some`):
//!
//! | Check | What it proves |
//! |-------|----------------|
//! | [`destroy_below_removes_only_older_versions`] | `Below(v2)` over `v1 < v2 < v3` removes `v1` only |
//! | [`destroy_exactly_removes_only_that_version`] | `Exactly(v2)` removes `v2` only |
//! | [`destroy_is_idempotent`] | repeating `Below` and `Exactly`, also over versions already gone, is success and changes nothing |
//! | [`destroy_missing_key_is_ok`] | `destroy` on a never-written, a deleted or a fully destroyed key is success |
//! | [`destroy_does_not_touch_other_keys`] | `destroy` affects only its own key |
//! | [`versions_are_ordered_across_sequential_puts`] | sequential puts get increasing versions (`Below(last)` leaves only the last) |
//! | [`versions_are_ordered_after_concurrent_puts`] | a put that starts after concurrent puts returned is greater than all of them, and concurrent puts are greater than an earlier one |
//! | [`destroyed_versions_are_not_reissued`] | a destroyed version, even the newest, is never handed out again |
//!
//! # What the suite does not cover
//!
//! * [`CredStorePluginError::Unavailable`](crate::CredStorePluginError::Unavailable),
//!   [`CredStorePluginError::CredentialsRejected`](crate::CredStorePluginError::CredentialsRejected)
//!   and the permanent [`CredStorePluginError::Internal`](crate::CredStorePluginError::Internal)
//!   of a version that exists but can never be read: a backend cannot be
//!   made to go down or lose a decryption key from the outside. Test those
//!   in the plugin.
//! * Durability across a restart of the plugin or the backend (the suite can
//!   only read back through the same plugin instance).
//! * Behaviour the contract leaves open: `put` after `delete_key` on the same
//!   key (the gear never reuses a key), and `get` or `destroy` with a version
//!   string that is not in the plugin's format at all.
#![allow(
    clippy::missing_panics_doc,
    reason = "every check panics on a contract violation; documented once on the module"
)]

use std::collections::HashSet;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use futures_util::future::join_all;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::{
    CredStorePluginClientV2, DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion,
};

/// Calls `$callback! { $($context)* ; <check>, <check>, ... }` with the name of
/// every check. The one list of checks: `run_all` and the generated tests are
/// both expanded from it, so they cannot drift apart.
#[doc(hidden)]
#[macro_export]
macro_rules! __credstore_conformance_checks {
    ($callback:path, $($context:tt)*) => {
        $callback! {
            $($context)* ;
            put_get_text,
            put_get_binary,
            put_get_empty,
            put_get_large,
            puts_yield_distinct_immutable_versions,
            get_never_written_key_is_none,
            get_unissued_version_is_none,
            keys_are_isolated_across_tenants,
            keys_are_isolated_within_tenant,
            delete_key_removes_all_versions,
            delete_key_is_idempotent,
            delete_key_does_not_touch_other_keys,
            concurrent_puts_yield_distinct_versions,
            destroy_below_removes_only_older_versions,
            destroy_exactly_removes_only_that_version,
            destroy_is_idempotent,
            destroy_missing_key_is_ok,
            destroy_does_not_touch_other_keys,
            versions_are_ordered_across_sequential_puts,
            versions_are_ordered_after_concurrent_puts,
            destroyed_versions_are_not_reissued
        }
    };
}

/// Expands to one test per check: `$attrs` is the `[...]`-wrapped list of outer
/// attributes of the invocation (one token tree, so it can be repeated for
/// every check). Internal: see `credstore_plugin_conformance!`.
#[doc(hidden)]
#[macro_export]
macro_rules! __credstore_conformance_tests {
    ($attrs:tt $factory:expr ; $($check:ident),+) => {
        $(
            $crate::__credstore_conformance_test!($attrs $check $factory);
        )+
    };
}

/// One generated test. Internal: see `credstore_plugin_conformance!`.
#[doc(hidden)]
#[macro_export]
macro_rules! __credstore_conformance_test {
    ([$($attr:tt)*] $check:ident $factory:expr) => {
        $($attr)*
        #[::tokio::test]
        async fn $check() {
            let plugin = $factory;
            $crate::conformance::$check(&plugin).await;
        }
    };
}

/// Collects the leading outer attributes of a `credstore_plugin_conformance!`
/// invocation, then hands them and the factory expression on. (An `expr`
/// matcher cannot follow a repetition of attributes: both start with `#`.)
/// Internal.
#[doc(hidden)]
#[macro_export]
macro_rules! __credstore_conformance_parse {
    ([$($attrs:tt)*] # $attr:tt $($rest:tt)*) => {
        $crate::__credstore_conformance_parse!([$($attrs)* # $attr] $($rest)*);
    };
    ([$($attrs:tt)*] $factory:expr $(,)?) => {
        $crate::__credstore_conformance_checks!(
            $crate::__credstore_conformance_tests,
            [$($attrs)*] $factory
        );
    };
}

/// Defines one `#[tokio::test]` per conformance check, each running against a
/// fresh plugin built by `$factory`.
///
/// `$factory` is an expression evaluated inside each (async) test, so it may
/// `.await` (and `return` to skip a test); it must yield a value implementing
/// [`CredStorePluginClientV2`](crate::CredStorePluginClientV2) (not an `Arc`
/// or `Box` of one). The generated functions are named after the checks of
/// [`conformance`](crate::conformance), so a plugin gets readable test names
/// (`put_get_binary`, `delete_key_is_idempotent`, ...). Invoke it once per
/// module. The calling crate needs `tokio` (features `rt` and `macros`) in
/// its dev-dependencies.
///
/// Outer attributes written before the expression are applied to every
/// generated test, for example `#[ignore = "needs Docker"]`.
///
/// ```no_run
/// # #[cfg(feature = "conformance")]
/// # mod conformance_tests {
/// # use async_trait::async_trait;
/// # use credstore_v2_sdk::{
/// #     CredStorePluginClientV2, CredStorePluginError, SecretValue, StoreKey, ValueVersion,
/// # };
/// # use toolkit_security::SecurityContext;
/// # #[derive(Default)]
/// # struct MyPlugin;
/// # #[async_trait]
/// # impl CredStorePluginClientV2 for MyPlugin {
/// #     async fn put(&self, _: &SecurityContext, _: &StoreKey, _: SecretValue)
/// #         -> Result<ValueVersion, CredStorePluginError> { unimplemented!() }
/// #     async fn get(&self, _: &SecurityContext, _: &StoreKey, _: &ValueVersion)
/// #         -> Result<Option<SecretValue>, CredStorePluginError> { unimplemented!() }
/// #     async fn delete_key(&self, _: &SecurityContext, _: &StoreKey)
/// #         -> Result<(), CredStorePluginError> { unimplemented!() }
/// # }
/// credstore_v2_sdk::credstore_plugin_conformance!(MyPlugin::default());
/// # }
/// # fn main() {}
/// ```
///
/// With attributes (the tests then show up as ignored in a default run):
///
/// ```no_run
/// # #[cfg(feature = "conformance")]
/// # mod conformance_tests {
/// # use async_trait::async_trait;
/// # use credstore_v2_sdk::{
/// #     CredStorePluginClientV2, CredStorePluginError, SecretValue, StoreKey, ValueVersion,
/// # };
/// # use toolkit_security::SecurityContext;
/// # #[derive(Default)]
/// # struct MyPlugin;
/// # #[async_trait]
/// # impl CredStorePluginClientV2 for MyPlugin {
/// #     async fn put(&self, _: &SecurityContext, _: &StoreKey, _: SecretValue)
/// #         -> Result<ValueVersion, CredStorePluginError> { unimplemented!() }
/// #     async fn get(&self, _: &SecurityContext, _: &StoreKey, _: &ValueVersion)
/// #         -> Result<Option<SecretValue>, CredStorePluginError> { unimplemented!() }
/// #     async fn delete_key(&self, _: &SecurityContext, _: &StoreKey)
/// #         -> Result<(), CredStorePluginError> { unimplemented!() }
/// # }
/// credstore_v2_sdk::credstore_plugin_conformance!(
///     #[ignore = "needs Docker"]
///     MyPlugin::default()
/// );
/// # }
/// # fn main() {}
/// ```
#[macro_export]
macro_rules! credstore_plugin_conformance {
    ($($input:tt)*) => {
        $crate::__credstore_conformance_parse!([] $($input)*);
    };
}

/// Awaits every check in turn against `$plugin`.
macro_rules! run_each {
    ($plugin:ident ; $($check:ident),+) => {
        $( $check($plugin).await; )+
    };
}

/// Runs every check of the suite, in turn, against one plugin instance.
///
/// Prefer [`credstore_plugin_conformance!`](crate::credstore_plugin_conformance)
/// in a plugin's tests: it gives each check its own test and its own fresh
/// plugin. This function is for harnesses that already hold one long-lived
/// plugin (for example a smoke test against a deployed backend). It stops at
/// the first violated check.
#[allow(
    clippy::cognitive_complexity,
    reason = "a flat sequence of awaits expanded from the single list of checks"
)]
pub async fn run_all(plugin: &dyn CredStorePluginClientV2) {
    crate::__credstore_conformance_checks!(run_each, plugin);
}

// ---------------------------------------------------------------------------
// Checks: values
// ---------------------------------------------------------------------------

/// Text values come back byte for byte, and reading a version does not
/// consume it.
///
/// Covers Unicode, surrounding whitespace and line breaks, JSON- and
/// base64-looking text and control characters, to catch a backend that
/// re-encodes, trims or normalizes what it stores.
pub async fn put_get_text(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("put_get_text", plugin);
    let key = fresh_key();
    let cases: [(&str, &str); 7] = [
        ("plain ASCII", "correct horse battery staple"),
        (
            "Unicode",
            "\u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}-\u{5bc6}\u{7801}-\u{1f511}",
        ),
        ("surrounding whitespace", "  padded value\r\n\t\n"),
        ("JSON lookalike", r#"{"user":"u","password":"p\"q\\r"}"#),
        ("base64 lookalike", "dGVzdA=="),
        ("the word null", "null"),
        ("control characters", "line1\nline2\u{7}\u{1b}[0m"),
    ];
    for (what, text) in cases {
        let version = h.put(&key, text.as_bytes()).await;
        h.expect_value(&key, &version, text.as_bytes(), what).await;
        h.expect_value(&key, &version, text.as_bytes(), what).await;
    }
    h.finish().await;
}

/// Binary values, including NUL bytes and invalid UTF-8, come back byte for
/// byte.
///
/// Secret values are opaque bytes, not strings: a backend that stores them in
/// a text column or a JSON string without encoding them would fail here.
pub async fn put_get_binary(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("put_get_binary", plugin);
    let key = fresh_key();
    let cases: [(&str, Vec<u8>); 8] = [
        ("every byte value", (0..=u8::MAX).collect()),
        ("a single NUL", vec![0x00]),
        ("embedded NULs", b"\0abc\0\0".to_vec()),
        ("invalid UTF-8 (0xff 0xfe 0xfd)", vec![0xff, 0xfe, 0xfd]),
        (
            "invalid UTF-8 (bad and truncated sequences)",
            vec![0xc3, 0x28, 0xe2, 0x82],
        ),
        ("a lone continuation byte", vec![0x80]),
        (
            "a UTF-8 byte order mark prefix",
            vec![0xef, 0xbb, 0xbf, b'x'],
        ),
        ("1 KiB of pseudo-random bytes", pseudo_random_bytes(7, 1024)),
    ];
    for (what, bytes) in &cases {
        let version = h.put(&key, bytes).await;
        h.expect_value(&key, &version, bytes, what).await;
    }
    h.finish().await;
}

/// An empty value is a value: it round-trips as `Some` of zero bytes, never
/// as `None`.
///
/// The contract has no carve-out from "exact bytes" for empty values, and the
/// gear accepts an empty secret of the `generic` type, so a backend must be
/// able to store one.
pub async fn put_get_empty(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("put_get_empty", plugin);
    let key = fresh_key();
    let version = h.put(&key, b"").await;
    h.expect_value(&key, &version, b"", "an empty value").await;
    h.finish().await;
}

/// A 64 KiB value comes back byte for byte.
///
/// Catches truncation at a backend's field, row or request-size limit that is
/// lower than a secret may legitimately need.
pub async fn put_get_large(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("put_get_large", plugin);
    let key = fresh_key();
    let bytes = pseudo_random_bytes(0x00C0_FFEE, 64 * 1024);
    let version = h.put(&key, &bytes).await;
    h.expect_value(&key, &version, &bytes, "a 64 KiB value")
        .await;
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Checks: versions and absence
// ---------------------------------------------------------------------------

/// Every `put` to one key returns a new version, and every earlier version
/// keeps exactly its own bytes.
///
/// A put of bytes equal to an earlier put still creates a new version: the
/// gear relies on a version identifying one `put`, never on content.
pub async fn puts_yield_distinct_immutable_versions(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("puts_yield_distinct_immutable_versions", plugin);
    let key = fresh_key();
    let mut written = h.put_series(&key, "rotation", 3).await;
    let repeated = written[0].bytes.clone();
    let version = h.put(&key, &repeated).await;
    written.push(Written {
        version,
        bytes: repeated,
    });
    h.ensure_distinct(&written, "sequential puts to one key");
    h.expect_all_readable(&key, &written, "after four puts")
        .await;
    h.finish().await;
}

/// `get` on a key that was never written is `Ok(None)`.
///
/// The version asked for is borrowed from another key of the same plugin, so
/// it is well formed for the plugin and only the key is unknown.
pub async fn get_never_written_key_is_none(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("get_never_written_key_is_none", plugin);
    let donor = fresh_key();
    let donor_bytes = unique_text("donor");
    let version = h.put(&donor, &donor_bytes).await;
    let never_written = fresh_key();
    h.expect_gone(&never_written, &version, "a key that was never written")
        .await;
    h.expect_value(&donor, &version, &donor_bytes, "the donor key")
        .await;
    h.finish().await;
}

/// `get` of a version that was never issued for an existing key is
/// `Ok(None)`.
///
/// The versions asked for are issued by the plugin for another key (and
/// differ from the target's own), so they are well formed for the plugin but
/// were never returned by a `put` to the target key. Asking must neither fail
/// nor return anybody's bytes.
pub async fn get_unissued_version_is_none(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("get_unissued_version_is_none", plugin);
    let donor = fresh_key();
    let target = fresh_key();
    let donor_versions = h.put_series(&donor, "donor", 3).await;
    let target_bytes = unique_text("target");
    let kept = h.put(&target, &target_bytes).await;
    let mut probed = 0_usize;
    for foreign in &donor_versions {
        if foreign.version == kept {
            continue;
        }
        probed += 1;
        h.expect_gone(
            &target,
            &foreign.version,
            "a version never issued for this key",
        )
        .await;
    }
    if probed == 0 {
        h.fail(format_args!(
            "every put returned the same version {kept}, so no unissued version could be built; \
             versions must be distinct per put"
        ));
    }
    h.expect_value(&target, &kept, &target_bytes, "the target's own version")
        .await;
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Checks: key isolation and delete_key
// ---------------------------------------------------------------------------

/// The same record id under two tenants is two independent keys.
pub async fn keys_are_isolated_across_tenants(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("keys_are_isolated_across_tenants", plugin);
    let record_id = Uuid::new_v4();
    let a = StoreKey::new(TenantId(Uuid::new_v4()), record_id);
    let b = StoreKey::new(TenantId(Uuid::new_v4()), record_id);
    h.exercise_isolated_pair(&a, &b, "the same record under two tenants")
        .await;
    h.finish().await;
}

/// Two records of one tenant are two independent keys.
pub async fn keys_are_isolated_within_tenant(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("keys_are_isolated_within_tenant", plugin);
    let tenant_id = TenantId(Uuid::new_v4());
    let a = StoreKey::new(tenant_id, Uuid::new_v4());
    let b = StoreKey::new(tenant_id, Uuid::new_v4());
    h.exercise_isolated_pair(&a, &b, "two records of one tenant")
        .await;
    h.finish().await;
}

/// `delete_key` removes the key with every version it holds.
pub async fn delete_key_removes_all_versions(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("delete_key_removes_all_versions", plugin);
    let key = fresh_key();
    let written = h.put_series(&key, "doomed", 3).await;
    h.expect_all_readable(&key, &written, "before delete_key")
        .await;
    h.delete_key(&key, "delete_key of a key holding three versions")
        .await;
    h.expect_all_gone(&key, &written, "after delete_key").await;
    h.finish().await;
}

/// `delete_key` is idempotent: repeating it, and calling it on a key that was
/// never written, is success.
pub async fn delete_key_is_idempotent(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("delete_key_is_idempotent", plugin);
    let key = fresh_key();
    let written = h.put_series(&key, "doomed", 2).await;
    h.delete_key(&key, "the first delete_key").await;
    h.delete_key(&key, "a second delete_key of the same key")
        .await;
    h.delete_key(&key, "a third delete_key of the same key")
        .await;
    h.expect_all_gone(&key, &written, "after repeated delete_key")
        .await;
    h.delete_key(&fresh_key(), "delete_key of a key never written")
        .await;
    h.finish().await;
}

/// `delete_key` removes only its own key: a sibling record, the same record
/// id under another tenant, and keys the plugin never held are unaffected.
pub async fn delete_key_does_not_touch_other_keys(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("delete_key_does_not_touch_other_keys", plugin);
    let record_id = Uuid::new_v4();
    let tenant_id = TenantId(Uuid::new_v4());
    let doomed = StoreKey::new(tenant_id, record_id);
    let sibling = StoreKey::new(tenant_id, Uuid::new_v4());
    let other_tenant = StoreKey::new(TenantId(Uuid::new_v4()), record_id);
    let doomed_written = h.put_series(&doomed, "doomed", 2).await;
    let sibling_written = h.put_series(&sibling, "sibling", 2).await;
    let other_written = h.put_series(&other_tenant, "other-tenant", 2).await;

    h.delete_key(&doomed, "delete_key of one of three keys")
        .await;
    h.delete_key(
        &StoreKey::new(tenant_id, Uuid::new_v4()),
        "delete_key of a missing key",
    )
    .await;

    h.expect_all_gone(&doomed, &doomed_written, "the deleted key")
        .await;
    h.expect_all_readable(
        &sibling,
        &sibling_written,
        "a sibling record after the delete",
    )
    .await;
    h.expect_all_readable(
        &other_tenant,
        &other_written,
        "the same record of another tenant after the delete",
    )
    .await;
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Checks: concurrency
// ---------------------------------------------------------------------------

/// Eight concurrent puts to one key all succeed with pairwise distinct
/// versions, and each version reads back its own bytes.
///
/// Two rounds run back to back, and the versions of both rounds must be
/// distinct from each other.
pub async fn concurrent_puts_yield_distinct_versions(plugin: &dyn CredStorePluginClientV2) {
    let h = Harness::new("concurrent_puts_yield_distinct_versions", plugin);
    let key = fresh_key();
    let mut written = h.put_concurrently(&key, "round-one", 8).await;
    h.ensure_distinct(&written, "the first round of eight concurrent puts");
    written.extend(h.put_concurrently(&key, "round-two", 8).await);
    h.ensure_distinct(&written, "two rounds of eight concurrent puts");
    h.expect_all_readable(&key, &written, "after concurrent puts")
        .await;
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Checks: destroy (skipped when the plugin has no destroyer())
// ---------------------------------------------------------------------------

/// Destroy: `Below(v2)` over `v1 < v2 < v3` removes `v1` only; `v2` and `v3`
/// stay readable.
pub async fn destroy_below_removes_only_older_versions(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("destroy_below_removes_only_older_versions", plugin) else {
        return;
    };
    let key = fresh_key();
    let w = h.put_series(&key, "below", 3).await;

    h.destroy(&key, below(&w[1]), "destroy(Below(v2))").await;
    h.expect_removed(&key, &w[0], "v1 after destroy(Below(v2))")
        .await;
    h.expect_written(&key, &w[1], "v2 after destroy(Below(v2))")
        .await;
    h.expect_written(&key, &w[2], "v3 after destroy(Below(v2))")
        .await;

    h.destroy(&key, below(&w[2]), "destroy(Below(v3))").await;
    h.expect_removed(&key, &w[1], "v2 after destroy(Below(v3))")
        .await;
    h.expect_written(&key, &w[2], "v3 after destroy(Below(v3))")
        .await;

    h.delete_key(&key, "delete_key after destroy").await;
    h.expect_removed(&key, &w[2], "v3 after delete_key").await;
    h.finish().await;
}

/// Destroy: `Exactly(v2)` removes `v2` only; the versions around it stay
/// readable.
pub async fn destroy_exactly_removes_only_that_version(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("destroy_exactly_removes_only_that_version", plugin) else {
        return;
    };
    let key = fresh_key();
    let w = h.put_series(&key, "exactly", 3).await;

    h.destroy(&key, exactly(&w[1]), "destroy(Exactly(v2))")
        .await;
    h.expect_removed(&key, &w[1], "v2 after destroy(Exactly(v2))")
        .await;
    h.expect_written(&key, &w[0], "v1 after destroy(Exactly(v2))")
        .await;
    h.expect_written(&key, &w[2], "v3 after destroy(Exactly(v2))")
        .await;

    h.destroy(&key, exactly(&w[0]), "destroy(Exactly(v1))")
        .await;
    h.expect_removed(&key, &w[0], "v1 after destroy(Exactly(v1))")
        .await;
    h.expect_written(&key, &w[2], "v3 after destroy(Exactly(v1))")
        .await;

    h.destroy(&key, exactly(&w[2]), "destroy(Exactly(v3))")
        .await;
    h.expect_all_gone(&key, &w, "after every version was destroyed")
        .await;
    h.finish().await;
}

/// Destroy: repeating `Below` and `Exactly`, also over versions that are
/// already gone, is success and changes nothing.
///
/// The gear delivers destroys at least once, so a duplicate (or a `Below`
/// whose bound was removed by an `Exactly` in between) must neither fail nor
/// remove a version that is still current.
pub async fn destroy_is_idempotent(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("destroy_is_idempotent", plugin) else {
        return;
    };
    let key = fresh_key();
    let w = h.put_series(&key, "idempotent", 3).await;

    for attempt in ["first", "second"] {
        h.destroy(
            &key,
            below(&w[1]),
            &format!("the {attempt} destroy(Below(v2))"),
        )
        .await;
        h.expect_removed(&key, &w[0], "v1 after repeated Below(v2)")
            .await;
        h.expect_written(&key, &w[1], "v2 after repeated Below(v2)")
            .await;
        h.expect_written(&key, &w[2], "v3 after repeated Below(v2)")
            .await;
    }

    for attempt in ["first", "second"] {
        h.destroy(
            &key,
            exactly(&w[1]),
            &format!("the {attempt} destroy(Exactly(v2))"),
        )
        .await;
        h.expect_removed(&key, &w[1], "v2 after repeated Exactly(v2)")
            .await;
        h.expect_written(&key, &w[2], "v3 after repeated Exactly(v2)")
            .await;
    }

    h.destroy(
        &key,
        below(&w[1]),
        "destroy(Below(v2)) once v2 itself is gone",
    )
    .await;
    h.destroy(
        &key,
        exactly(&w[0]),
        "destroy(Exactly(v1)) of a version already gone",
    )
    .await;
    h.destroy(
        &key,
        below(&w[2]),
        "destroy(Below(v3)) with nothing older left",
    )
    .await;
    h.expect_written(&key, &w[2], "v3 after the duplicate destroys")
        .await;
    h.finish().await;
}

/// Destroy: on a key that was never written, was deleted, or has no version
/// left, `destroy` is success.
pub async fn destroy_missing_key_is_ok(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("destroy_missing_key_is_ok", plugin) else {
        return;
    };

    // Never written: a version borrowed from another key stands in for "any
    // version of the plugin's format".
    let foreign = h.foreign_version().await;
    let never_written = fresh_key();
    h.destroy(
        &never_written,
        DestroySelector::Exactly(foreign.clone()),
        "destroy(Exactly) on a key never written",
    )
    .await;
    h.destroy(
        &never_written,
        DestroySelector::Below(foreign),
        "destroy(Below) on a key never written",
    )
    .await;

    // Deleted.
    let deleted = fresh_key();
    let w = h.put_series(&deleted, "deleted", 2).await;
    h.delete_key(&deleted, "delete_key before destroy").await;
    h.destroy(
        &deleted,
        exactly(&w[1]),
        "destroy(Exactly) on a deleted key",
    )
    .await;
    h.destroy(&deleted, below(&w[1]), "destroy(Below) on a deleted key")
        .await;

    // Every version destroyed one by one, the key itself still known.
    let emptied = fresh_key();
    let w = h.put_series(&emptied, "emptied", 2).await;
    for written in &w {
        h.destroy(
            &emptied,
            exactly(written),
            "destroy(Exactly) of each version",
        )
        .await;
    }
    h.destroy(
        &emptied,
        below(&w[1]),
        "destroy(Below) on a key with no version left",
    )
    .await;
    h.destroy(
        &emptied,
        exactly(&w[1]),
        "destroy(Exactly) on a key with no version left",
    )
    .await;
    h.finish().await;
}

/// Destroy: `destroy` of one key leaves its sibling record and the same
/// record id under another tenant untouched.
pub async fn destroy_does_not_touch_other_keys(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("destroy_does_not_touch_other_keys", plugin) else {
        return;
    };
    let record_id = Uuid::new_v4();
    let tenant_id = TenantId(Uuid::new_v4());
    let target = StoreKey::new(tenant_id, record_id);
    let sibling = StoreKey::new(tenant_id, Uuid::new_v4());
    let other_tenant = StoreKey::new(TenantId(Uuid::new_v4()), record_id);
    let t = h.put_series(&target, "target", 3).await;
    let s = h.put_series(&sibling, "sibling", 3).await;
    let o = h.put_series(&other_tenant, "other-tenant", 3).await;

    h.destroy(&target, below(&t[2]), "destroy(Below(v3)) on the target")
        .await;
    h.destroy(
        &target,
        exactly(&t[2]),
        "destroy(Exactly(v3)) on the target",
    )
    .await;
    h.expect_all_gone(&target, &t, "the target after both destroys")
        .await;
    h.expect_all_readable(&sibling, &s, "a sibling record after a destroy")
        .await;
    h.expect_all_readable(
        &other_tenant,
        &o,
        "the same record of another tenant after a destroy",
    )
    .await;
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Checks: ordered versions (the precondition of `destroy(Below(..))`)
// ---------------------------------------------------------------------------

/// Destroy: puts that each start after the previous one returned get
/// increasing versions.
///
/// Observed without comparing opaque strings. On one key, five sequential
/// puts followed by `Below(last)` must leave only the last readable. On a
/// second key, `Below(v_i)` applied in turn must each remove exactly the
/// version before it and keep `v_i` and everything newer.
pub async fn versions_are_ordered_across_sequential_puts(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("versions_are_ordered_across_sequential_puts", plugin)
    else {
        return;
    };

    let all_at_once = fresh_key();
    let w = h.put_series(&all_at_once, "ordered", 5).await;
    h.ensure_distinct(&w, "five sequential puts");
    let last = w.len() - 1;
    h.destroy(&all_at_once, below(&w[last]), "destroy(Below(last))")
        .await;
    h.expect_all_gone(&all_at_once, &w[..last], "after destroy(Below(last))")
        .await;
    h.expect_written(
        &all_at_once,
        &w[last],
        "the last version after destroy(Below(last))",
    )
    .await;

    let step_by_step = fresh_key();
    let w = h.put_series(&step_by_step, "stepped", 5).await;
    for i in 1..w.len() {
        h.destroy(
            &step_by_step,
            below(&w[i]),
            &format!("destroy(Below(v{}))", i + 1),
        )
        .await;
        h.expect_removed(
            &step_by_step,
            &w[i - 1],
            &format!(
                "v{} after destroy(Below(v{})): a later put must get a greater version",
                i,
                i + 1
            ),
        )
        .await;
        h.expect_all_readable(
            &step_by_step,
            &w[i..],
            &format!("v{} and newer after destroy(Below(v{}))", i + 1, i + 1),
        )
        .await;
    }
    h.finish().await;
}

/// Destroy: a put that starts after concurrent puts returned is greater than
/// all of them, and concurrent puts that start after a put returned are
/// greater than it.
///
/// Concurrent puts are not ordered among themselves; only their relation to
/// puts that are strictly before or after the batch is checked.
pub async fn versions_are_ordered_after_concurrent_puts(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("versions_are_ordered_after_concurrent_puts", plugin) else {
        return;
    };
    let key = fresh_key();

    let before = h.put(&key, &unique_text("before")).await;
    let batch = h.put_concurrently(&key, "batch", 8).await;
    h.ensure_distinct(&batch, "eight concurrent puts");
    h.destroy(&key, below(&batch[0]), "destroy(Below(first of the batch))")
        .await;
    h.expect_gone(
        &key,
        &before,
        "a put made before the batch, after destroy(Below(a batch version))",
    )
    .await;
    h.expect_written(&key, &batch[0], "the batch version used as the bound")
        .await;

    let after_bytes = unique_text("after");
    let after = h.put(&key, &after_bytes).await;
    h.destroy(
        &key,
        DestroySelector::Below(after.clone()),
        "destroy(Below(after the batch))",
    )
    .await;
    h.expect_all_gone(&key, &batch, "the batch after destroy(Below(a later put))")
        .await;
    h.expect_value(&key, &after, &after_bytes, "the put made after the batch")
        .await;
    h.finish().await;
}

/// Destroy: a destroyed version, even the newest, is never handed out again
/// and never reads anything.
///
/// A backend that derives the next version from the versions it still holds
/// (for example `max + 1`) would reissue the number of a destroyed newest
/// version; a stale reader holding it would then get different bytes, which
/// the contract forbids ("never different bytes").
pub async fn destroyed_versions_are_not_reissued(plugin: &dyn CredStorePluginClientV2) {
    let Some(h) = Harness::for_destroy("destroyed_versions_are_not_reissued", plugin) else {
        return;
    };
    let key = fresh_key();
    let w = h.put_series(&key, "reissue", 2).await;

    h.destroy(&key, exactly(&w[1]), "destroy(Exactly(newest))")
        .await;
    let next_bytes = unique_text("next");
    let next = h.put(&key, &next_bytes).await;
    if next == w[1].version || next == w[0].version {
        h.fail(format_args!(
            "a put after destroying the newest version returned {next}, which was already issued \
             under this key; versions must never be reissued"
        ));
    }
    h.expect_removed(
        &key,
        &w[1],
        "the destroyed newest version after a later put",
    )
    .await;
    h.expect_value(&key, &next, &next_bytes, "the put after the destroy")
        .await;

    // The put after the destroy must also be greater than what is left.
    h.destroy(
        &key,
        DestroySelector::Below(next.clone()),
        "destroy(Below(next))",
    )
    .await;
    h.expect_removed(&key, &w[0], "v1 after destroy(Below(next))")
        .await;
    h.expect_value(
        &key,
        &next,
        &next_bytes,
        "the put after the destroy, after Below",
    )
    .await;
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One version a check wrote: what the plugin returned and the bytes put.
struct Written {
    version: ValueVersion,
    bytes: Vec<u8>,
}

fn below(written: &Written) -> DestroySelector {
    DestroySelector::Below(written.version.clone())
}

fn exactly(written: &Written) -> DestroySelector {
    DestroySelector::Exactly(written.version.clone())
}

/// A key under a fresh random tenant and record id.
fn fresh_key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

/// Human-readable form of a key for panic messages.
fn describe(key: &StoreKey) -> String {
    format!("key(tenant {}, record {})", key.tenant_id.0, key.record_id)
}

/// A distinct text value per call, so checks sharing one backend never write
/// equal bytes and a read of the wrong version cannot pass by accident.
fn unique_text(label: &str) -> Vec<u8> {
    format!("conformance/{label}/{}", Uuid::new_v4()).into_bytes()
}

/// `len` deterministic bytes from an xorshift generator seeded with `seed`.
fn pseudo_random_bytes(seed: u32, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        bytes.push(state.to_le_bytes()[3]);
    }
    bytes
}

/// Offset of the first byte at which `a` and `b` differ (the shorter length
/// when one is a prefix of the other).
fn first_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()))
}

/// Per-check state: the plugin under test, a correlation-only context, and the
/// keys written (deleted again by [`Harness::finish`]).
struct Harness<'a> {
    check: &'static str,
    plugin: &'a dyn CredStorePluginClientV2,
    ctx: SecurityContext,
    touched: Mutex<Vec<StoreKey>>,
}

impl<'a> Harness<'a> {
    fn new(check: &'static str, plugin: &'a dyn CredStorePluginClientV2) -> Self {
        let ctx = match SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
        {
            Ok(ctx) => ctx,
            Err(e) => panic!("conformance[{check}]: cannot build the request context: {e}"),
        };
        Self {
            check,
            plugin,
            ctx,
            touched: Mutex::new(Vec::new()),
        }
    }

    /// A harness for a check that needs `destroy`, or `None` (after a
    /// `tracing` warning) when the plugin has no `destroyer()`.
    fn for_destroy(check: &'static str, plugin: &'a dyn CredStorePluginClientV2) -> Option<Self> {
        if plugin.destroyer().is_some() {
            Some(Self::new(check, plugin))
        } else {
            tracing::warn!(
                check,
                "conformance check skipped: the plugin does not provide destroyer()"
            );
            None
        }
    }

    fn fail(&self, message: fmt::Arguments<'_>) -> ! {
        panic!("conformance[{}]: {message}", self.check)
    }

    fn touch(&self, key: &StoreKey) {
        let mut touched = self.touched.lock().unwrap_or_else(PoisonError::into_inner);
        if !touched.contains(key) {
            touched.push(key.clone());
        }
    }

    /// Deletes every key the check wrote, so a passing check leaves a shared
    /// backend as it found it.
    async fn finish(self) {
        let keys =
            std::mem::take(&mut *self.touched.lock().unwrap_or_else(PoisonError::into_inner));
        for key in &keys {
            self.delete_key(key, "cleanup delete_key").await;
        }
    }

    async fn put(&self, key: &StoreKey, value: &[u8]) -> ValueVersion {
        self.touch(key);
        match self
            .plugin
            .put(&self.ctx, key, SecretValue::new(value.to_vec()))
            .await
        {
            Ok(version) => version,
            Err(e) => self.fail(format_args!(
                "put of {} bytes to {} failed: {e}",
                value.len(),
                describe(key)
            )),
        }
    }

    async fn get(&self, key: &StoreKey, version: &ValueVersion) -> Option<Vec<u8>> {
        match self.plugin.get(&self.ctx, key, version).await {
            Ok(found) => found.map(|value| value.as_bytes().to_vec()),
            Err(e) => self.fail(format_args!(
                "get of version {version} of {} failed: {e}",
                describe(key)
            )),
        }
    }

    async fn delete_key(&self, key: &StoreKey, what: &str) {
        if let Err(e) = self.plugin.delete_key(&self.ctx, key).await {
            self.fail(format_args!("{what} ({}) failed: {e}", describe(key)));
        }
    }

    async fn destroy(&self, key: &StoreKey, selector: DestroySelector, what: &str) {
        let Some(destroyer) = self.plugin.destroyer() else {
            self.fail(format_args!(
                "{what} ({}): the plugin returned no destroyer(); destroy checks run only for \
                 plugins that provide one",
                describe(key)
            ))
        };
        if let Err(e) = destroyer.destroy(&self.ctx, key, selector).await {
            self.fail(format_args!("{what} ({}) failed: {e}", describe(key)));
        }
    }

    /// `get` must return exactly `expected`.
    async fn expect_value(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
        expected: &[u8],
        what: &str,
    ) {
        match self.get(key, version).await {
            None => self.fail(format_args!(
                "{what}: get of version {version} of {} returned None, but the version was \
                 returned by a put and never removed",
                describe(key)
            )),
            Some(got) if got != expected => self.fail(format_args!(
                "{what}: get of version {version} of {} returned {} bytes, not the {} bytes put \
                 (first difference at offset {})",
                describe(key),
                got.len(),
                expected.len(),
                first_difference(&got, expected)
            )),
            Some(_) => {}
        }
    }

    /// `get` of `written`'s version must return the bytes of that put.
    async fn expect_written(&self, key: &StoreKey, written: &Written, what: &str) {
        self.expect_value(key, &written.version, &written.bytes, what)
            .await;
    }

    /// `get` of `written`'s version must return `Ok(None)`.
    async fn expect_removed(&self, key: &StoreKey, written: &Written, what: &str) {
        self.expect_gone(key, &written.version, what).await;
    }

    /// `get` must return `Ok(None)`.
    async fn expect_gone(&self, key: &StoreKey, version: &ValueVersion, what: &str) {
        if let Some(got) = self.get(key, version).await {
            self.fail(format_args!(
                "{what}: get of version {version} of {} returned {} bytes, expected None",
                describe(key),
                got.len()
            ));
        }
    }

    /// `get` may return `None` or the bytes of the key's own version, but never
    /// `forbidden` (the bytes of another key).
    async fn expect_not_bytes(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
        forbidden: &[u8],
        what: &str,
    ) {
        if let Some(got) = self.get(key, version).await
            && got == forbidden
        {
            self.fail(format_args!(
                "{what}: get of version {version} of {} returned the bytes of a different key",
                describe(key)
            ));
        }
    }

    async fn expect_all_readable(&self, key: &StoreKey, written: &[Written], what: &str) {
        for (i, w) in written.iter().enumerate() {
            self.expect_value(
                key,
                &w.version,
                &w.bytes,
                &format!("{what} (version #{})", i + 1),
            )
            .await;
        }
    }

    async fn expect_all_gone(&self, key: &StoreKey, written: &[Written], what: &str) {
        for (i, w) in written.iter().enumerate() {
            self.expect_gone(key, &w.version, &format!("{what} (version #{})", i + 1))
                .await;
        }
    }

    fn ensure_distinct(&self, written: &[Written], what: &str) {
        let mut seen = HashSet::new();
        for w in written {
            if !seen.insert(&w.version) {
                self.fail(format_args!(
                    "{what}: version {} was returned by two different puts to one key",
                    w.version
                ));
            }
        }
    }

    /// `count` puts to `key`, each starting after the previous one returned.
    async fn put_series(&self, key: &StoreKey, label: &str, count: usize) -> Vec<Written> {
        let mut written = Vec::with_capacity(count);
        for n in 0..count {
            let bytes = unique_text(&format!("{label}-{n}"));
            let version = self.put(key, &bytes).await;
            written.push(Written { version, bytes });
        }
        written
    }

    /// `count` puts to `key` issued concurrently.
    async fn put_concurrently(&self, key: &StoreKey, label: &str, count: usize) -> Vec<Written> {
        let payloads: Vec<Vec<u8>> = (0..count)
            .map(|n| unique_text(&format!("{label}-{n}")))
            .collect();
        let versions = join_all(payloads.iter().map(|bytes| self.put(key, bytes))).await;
        versions
            .into_iter()
            .zip(payloads)
            .map(|(version, bytes)| Written { version, bytes })
            .collect()
    }

    /// A version the plugin issued for a throwaway key: well formed for the
    /// plugin, unrelated to any key the caller uses.
    async fn foreign_version(&self) -> ValueVersion {
        self.put(&fresh_key(), &unique_text("foreign")).await
    }

    /// Two distinct keys do not see each other: each reads its own bytes, a
    /// version of one never yields the other's bytes, and writing to one
    /// leaves the other as it was.
    async fn exercise_isolated_pair(&self, a: &StoreKey, b: &StoreKey, what: &str) {
        let a_bytes = unique_text("key-a");
        let b_bytes = unique_text("key-b");
        let a_version = self.put(a, &a_bytes).await;
        let b_version = self.put(b, &b_bytes).await;
        self.expect_value(a, &a_version, &a_bytes, &format!("{what}: first key"))
            .await;
        self.expect_value(b, &b_version, &b_bytes, &format!("{what}: second key"))
            .await;

        self.expect_not_bytes(
            b,
            &a_version,
            &a_bytes,
            &format!("{what}: first key's version on second key"),
        )
        .await;
        self.expect_not_bytes(
            a,
            &b_version,
            &b_bytes,
            &format!("{what}: second key's version on first key"),
        )
        .await;

        let a_next_bytes = unique_text("key-a-next");
        let a_next = self.put(a, &a_next_bytes).await;
        self.expect_value(
            a,
            &a_next,
            &a_next_bytes,
            &format!("{what}: first key, new version"),
        )
        .await;
        self.expect_value(
            b,
            &b_version,
            &b_bytes,
            &format!("{what}: second key after a write to the first"),
        )
        .await;
        self.expect_value(
            a,
            &a_version,
            &a_bytes,
            &format!("{what}: first key, old version"),
        )
        .await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "conformance_tests.rs"]
mod conformance_tests;
