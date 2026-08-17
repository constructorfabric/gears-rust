// Created: 2026-08-12 by Constructor Tech
//! The profile/discovery service (DESIGN-DEPLOYABLE-GEAR §5.5, §6.7).
//!
//! One call, and it is what makes a *remote* backend's **synchronous**
//! `consistency()` / `features()` / `provider_name()` answerable at all: the
//! client fetches descriptors once after wiring and then answers those accessors
//! from its cache, without a call per question (§5.5).
//!
//! # It reads the registry snapshot, not the backends
//!
//! Descriptors are computed at wiring time from the real backends and carried on
//! [`BoundProfile`](crate::BoundProfile) (item `R2`), so serving this call touches
//! no backend and does no I/O — which is what lets `resolve()` await it under a
//! bound timeout without startup ever blocking on cluster reachability
//! (invariant I6).
//!
//! Profiles enumerate in **name order**, because the snapshot is a `BTreeMap`, so
//! the response is deterministic across replicas and across calls.
//!
//! # The generation is the point of the response, not decoration
//!
//! It is §5.6's staleness detector: a client re-reads descriptors when the
//! generation it holds no longer matches, rather than polling for a diff.

use cluster_sdk::dto;
use cluster_sdk::grpc::stubs::profile as stubs;
use tonic::{Request, Response, Status};

use super::ServiceContext;

/// Profile discovery, served over the wire.
#[derive(Debug, Clone)]
pub struct ClusterProfileService {
    ctx: ServiceContext,
}

impl ClusterProfileService {
    /// Builds the service over the shared [`ServiceContext`].
    #[must_use]
    pub fn new(ctx: ServiceContext) -> Self {
        Self { ctx }
    }
}

#[tonic::async_trait]
impl stubs::cluster_profile_api_server::ClusterProfileApi for ClusterProfileService {
    /// Describes the bound profiles, or the named subset.
    ///
    /// **A named profile that is not bound is an error, not an omission.** An
    /// empty request means "all", so silently dropping an unknown name from the
    /// response would be indistinguishable from a deployment that binds nothing —
    /// and the consumer asking is about to gate its own readiness on the answer
    /// (§4.7.1). It gets the `NotFound`-mapped `ProfileNotBound` instead, the same
    /// error a data-plane call against that name would get.
    async fn describe_profiles(
        &self,
        request: Request<stubs::DescribeProfilesRequest>,
    ) -> Result<Response<stubs::DescribeProfilesResponse>, Status> {
        // No profile to dispatch on: this is the call that *discovers* them.
        let _caller = self.ctx.authorize_only(&request).await?;
        let req = request.into_inner();

        // One snapshot for the whole response, so the descriptors and the
        // generation reported alongside them are the same view. Two loads could
        // report a generation that never described this set.
        let snapshot = self.ctx.profiles().snapshot();

        // `descriptor()`, never `wired_descriptor`: the health a client acts on
        // moves after wiring, and reading it here is what makes the 10 s
        // descriptor poll a live per-profile readiness signal (DESIGN section 4.4).
        let profiles: Vec<dto::ProfileDescriptor> = if req.profiles.is_empty() {
            snapshot
                .profiles
                .values()
                .map(|bound| bound.descriptor())
                .collect()
        } else {
            req.profiles
                .iter()
                .map(|name| {
                    snapshot
                        .profiles
                        .get(name.as_str())
                        .map(|bound| bound.descriptor())
                        .ok_or_else(|| {
                            cluster_sdk::to_status(cluster_sdk::ClusterError::ProfileNotBound {
                                profile: cluster_sdk::intern_existing(name).unwrap_or("<unknown>"),
                            })
                        })
                })
                .collect::<Result<_, Status>>()?
        };

        Ok(Response::new(stubs::DescribeProfilesResponse::from(
            dto::DescribeProfilesResponse {
                profiles,
                generation: snapshot.generation,
            },
        )))
    }
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;
