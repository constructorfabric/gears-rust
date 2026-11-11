//! A wrapper over the real `ActivationRepo` that changes one statement.
//!
//! The failure and race tests need a repository that is the real one except
//! for the call under test. [`forwarding_activation_repo`] writes the
//! `ActivationRepository` impl for such a wrapper: the methods the test
//! scripts in the braces, and every other listed method forwarded to
//! `ActivationRepo` as it is.

/// `forwarding_activation_repo!(impl ActivationRepository for Wrapper {
/// scripted methods } forwarded: [method, ...])` expands to the
/// `#[async_trait]` impl: the scripted methods verbatim, then one method per
/// listed name that calls the same method on `ActivationRepo`.
macro_rules! forwarding_activation_repo {
    (impl ActivationRepository for $ty:ty { $($scripted:tt)* } forwarded: [$($name:ident),* $(,)?]) => {
        forwarding_activation_repo!(@collect [$ty] [$($scripted)*] [] $($name)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] register $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [register (instance: &$crate::domain::activation::InstanceKey, label: Option<&str>) $crate::domain::activation::Subscription]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] of_key $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [of_key (key: &str) Vec<$crate::domain::activation::Subscription>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] find $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [find (instance: &$crate::domain::activation::InstanceKey) Option<$crate::domain::activation::Subscription>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] registrations $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [registrations (subscriber: &str, key: &str, tenant: &str) u64]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] oldest_evictable $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [oldest_evictable (subscriber: &str, key: &str, tenant: &str) Option<$crate::domain::activation::Subscription>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] silent_subscriptions $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [silent_subscriptions (key: &str, tenant: &str) Vec<$crate::domain::activation::Subscription>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] unsubscribe $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [unsubscribe (instance: &$crate::domain::activation::InstanceKey) bool]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] delete_key $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [delete_key (key: &str) (usize, usize)]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] write_states $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [write_states (write: &$crate::domain::activation::publisher::StateWrite) Vec<$crate::domain::activation::SubscriberTenant>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] state $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [state (instance: &$crate::domain::activation::InstanceKey) Option<$crate::domain::activation::ActivationState>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] rearm $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [rearm (instance: &$crate::domain::activation::InstanceKey, change_ref: uuid::Uuid) bool]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] clear_silent $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [clear_silent (instance: &$crate::domain::activation::InstanceKey) bool]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] states_of $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [states_of (key: &str, tenant: &str) Vec<$crate::domain::activation::ActivationState>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] activation_rows $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [activation_rows (key: &str, tenant: &str) Vec<$crate::domain::activation::ActivationRow>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] report $($rest:ident)*) => {
        forwarding_activation_repo!(@collect $t $s [$($acc)* [report (transition: &$crate::domain::activation::recorder::Transition) Option<time::OffsetDateTime>]] $($rest)*);
    };
    (@collect [$ty:ty] [$($scripted:tt)*] [$([$name:ident ($($arg:ident: $argty:ty),*) $ret:ty])*]) => {
        #[async_trait::async_trait]
        impl $crate::domain::activation::ActivationRepository for $ty {
            $($scripted)*
            $(
                async fn $name<C: toolkit_db::secure::DBRunner>(
                    &self,
                    conn: &C
                    $(, $arg: $argty)*
                ) -> Result<$ret, $crate::domain::error::DomainError> {
                    $crate::infra::storage::activation_repo::ActivationRepo
                        .$name(conn $(, $arg)*)
                        .await
                }
            )*
        }
    };
}

pub(crate) use forwarding_activation_repo;
