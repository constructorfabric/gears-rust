//! Security context scoping for clients
//!
//! This module provides a lightweight, zero-allocation wrapper that binds a `SecurityContext`
//! to any client type, enabling security-aware API calls without cloning or Arc overhead.
//!
//! # Example
//!
//! ```rust,ignore
//! use modkit_sdk::secured::{Secured, WithSecurityContext};
//! use modkit_security::SecurityContext;
//!
//! let client = MyClient::new();
//! let ctx = SecurityContext::builder()
//!     .subject_id(TEST_SUBJECT_ID)
//!     .subject_tenant_id(TEST_TENANT_ID)
//!     .build()?;
//!
//! // Bind the security context to the client
//! let secured = client.security_ctx(&ctx);
//!
//! // Access the client and context
//! let client_ref = secured.client();
//! let ctx_ref = secured.ctx();
//! ```

use modkit_security::SecurityContext;

/// A wrapper that binds a `SecurityContext` to a client reference.
///
/// This struct provides a zero-cost abstraction for carrying both a client
/// and its associated security context together, without any allocation or cloning.
///
/// # Type Parameters
///
/// * `'a` - The lifetime of both the client and security context references
/// * `C` - The client type being wrapped
#[derive(Debug)]
pub struct Secured<'a, C> {
    client: &'a C,
    ctx: &'a SecurityContext,
}

impl<'a, C> Secured<'a, C> {
    /// Creates a new `Secured` wrapper binding a client and security context.
    ///
    /// # Arguments
    ///
    /// * `client` - Reference to the client
    /// * `ctx` - Reference to the security context
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let secured = Secured::new(&client, &ctx);
    /// ```
    #[must_use]
    pub fn new(client: &'a C, ctx: &'a SecurityContext) -> Self {
        Self { client, ctx }
    }

    /// Returns a reference to the wrapped client.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let client_ref = secured.client();
    /// ```
    #[must_use]
    pub fn client(&self) -> &'a C {
        self.client
    }

    /// Returns a reference to the security context.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let ctx_ref = secured.ctx();
    /// let tenant_id = ctx_ref.subject_tenant_id();
    /// ```
    #[must_use]
    pub fn ctx(&self) -> &'a SecurityContext {
        self.ctx
    }

    /// Create a new query builder for the given schema.
    ///
    /// This provides an ergonomic entrypoint for building queries from a secured client.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use modkit_sdk::odata::items_stream;
    ///
    /// let items = items_stream(
    ///     client.security_ctx(&ctx)
    ///         .query::<UserSchema>()
    ///         .filter(user::email().contains("@example.com")),
    ///     |query| async move { client.list_users(query).await },
    /// );
    /// ```
    #[must_use]
    pub fn query<S: crate::odata::Schema>(&self) -> crate::odata::QueryBuilder<S> {
        crate::odata::QueryBuilder::new()
    }
}

impl<C> Clone for Secured<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for Secured<'_, C> {}

/// Extension trait that adds the `security_ctx` method to any type.
///
/// This trait enables any client to be wrapped with a security context
/// using a fluent API: `client.security_ctx(&ctx)`.
///
/// # Example
///
/// ```rust,ignore
/// use modkit_sdk::secured::WithSecurityContext;
///
/// let secured = my_client.security_ctx(&security_context);
/// ```
pub trait WithSecurityContext {
    /// Binds a security context to this client, returning a `Secured` wrapper.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Reference to the security context to bind
    ///
    /// # Returns
    ///
    /// A `Secured` wrapper containing references to both the client and context.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let secured = client.security_ctx(&ctx);
    /// assert_eq!(secured.ctx().subject_tenant_id(), ctx.subject_tenant_id());
    /// ```
    fn security_ctx<'a>(&'a self, ctx: &'a SecurityContext) -> Secured<'a, Self>
    where
        Self: Sized;
}

impl<T> WithSecurityContext for T {
    fn security_ctx<'a>(&'a self, ctx: &'a SecurityContext) -> Secured<'a, Self>
    where
        Self: Sized,
    {
        Secured::new(self, ctx)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "secured_tests.rs"]
mod tests;
