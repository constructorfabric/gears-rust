// Created: 2026-08-12 by Constructor Tech
//! The cache service (DESIGN-DEPLOYABLE-GEAR §6.4, §12.6).
//!
//! The stateless shape: every method is the five steps of the [module
//! docs](super) and holds nothing between calls.
//!
//! Two methods do more than forward, and both do it because the wire's shape and
//! the backend trait's shape genuinely differ:
//!
//! - **`scan_prefix` is paginated on the wire** and unbounded on the trait (§6.4),
//!   so the server pages it. See [`ClusterCacheApi::scan_prefix`].
//! - **the two watches are server-push**, so each spawns a pump that turns a
//!   [`CacheWatch`] into a gRPC stream under §6.8's rules. See [`watch_stream`].

use std::time::Duration;

use cluster_sdk::cache::{CacheWatch, CacheWatchEvent, PutRequest, Ttl};
use cluster_sdk::dto;
use cluster_sdk::grpc::stubs::cache as stubs;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::{ServiceContext, wire_error};

/// The largest page `scan_prefix` will return, whatever the caller asks for
/// (§6.4: "the server enforces a max page size").
const MAX_SCAN_PAGE_SIZE: usize = 1_000;

/// The page size used when the caller names none.
const DEFAULT_SCAN_PAGE_SIZE: usize = 256;

/// How many events a watch stream buffers for one remote subscriber before it
/// starts dropping and reporting `Lagged` (§6.8).
///
/// Bounded on purpose, and dropping rather than blocking is the load-bearing half:
/// one wedged consumer must never stall a shared watch's fan-out for everyone
/// else. It mirrors the `try_send`-plus-`Lagged` behaviour `CacheWatchSender`
/// already has in-process, so the consumer-visible signal is the same in both
/// deployment profiles.
const WATCH_STREAM_BUFFER: usize = 256;

/// The cache primitive, served over the wire.
#[derive(Debug, Clone)]
pub struct CacheService {
    ctx: ServiceContext,
}

impl CacheService {
    /// Builds the service over the shared [`ServiceContext`].
    #[must_use]
    pub fn new(ctx: ServiceContext) -> Self {
        Self { ctx }
    }
}

/// The stream type both watch methods return.
pub type CacheWatchStream = ReceiverStream<Result<stubs::CacheWatchEventDto, Status>>;

#[tonic::async_trait]
impl stubs::cluster_cache_api_server::ClusterCacheApi for CacheService {
    async fn get(
        &self,
        request: Request<stubs::GetRequest>,
    ) -> Result<Response<stubs::GetResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let entry = bound
            .cache
            .get(&req.key)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(stubs::GetResponse::from(dto::GetResponse {
            entry: entry.map(Into::into),
        })))
    }

    async fn put(
        &self,
        request: Request<stubs::PutRequest>,
    ) -> Result<Response<stubs::PutResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        bound
            .cache
            .put(PutRequest {
                key: &req.key,
                value: &req.value,
                ttl: ttl(req.ttl_ms),
            })
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(self.ack()))
    }

    async fn put_if_absent(
        &self,
        request: Request<stubs::PutRequest>,
    ) -> Result<Response<stubs::PutIfAbsentResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let created = bound
            .cache
            .put_if_absent(PutRequest {
                key: &req.key,
                value: &req.value,
                ttl: ttl(req.ttl_ms),
            })
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(stubs::PutIfAbsentResponse::from(
            dto::PutIfAbsentResponse {
                created: created.map(Into::into),
            },
        )))
    }

    async fn compare_and_swap(
        &self,
        request: Request<stubs::CasRequest>,
    ) -> Result<Response<stubs::CasResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        // `CasConflict`'s `current` is whatever the backend chose to put in it:
        // the codec carries `current_version` and `current_value` as independently
        // optional fields, so decision 17a is a per-response choice made *below*
        // this line, not a wire shape decided here (§6.9, item `C4`).
        let entry = bound
            .cache
            .compare_and_swap(
                &req.key,
                req.expected_version,
                &req.new_value,
                ttl(req.ttl_ms),
            )
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(stubs::CasResponse::from(dto::CasResponse {
            entry: entry.into(),
        })))
    }

    async fn compare_and_delete(
        &self,
        request: Request<stubs::CadRequest>,
    ) -> Result<Response<stubs::CadResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        // On the wire even though the backend trait defaults it: that default is a
        // non-atomic get-then-delete, which is a real race across a network, and
        // the CAS-based lock and leader release depend on it being atomic (§6.3).
        let deleted = bound
            .cache
            .compare_and_delete(&req.key, &req.expected_value)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(stubs::CadResponse::from(dto::CadResponse {
            deleted,
        })))
    }

    async fn delete(
        &self,
        request: Request<stubs::DeleteRequest>,
    ) -> Result<Response<stubs::DeleteResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let existed = bound
            .cache
            .delete(&req.key)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(stubs::DeleteResponse::from(
            dto::DeleteResponse { existed },
        )))
    }

    async fn contains(
        &self,
        request: Request<stubs::ContainsRequest>,
    ) -> Result<Response<stubs::ContainsResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let present = bound
            .cache
            .contains(&req.key)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(stubs::ContainsResponse::from(
            dto::ContainsResponse { present },
        )))
    }

    /// One page of keys under a prefix.
    ///
    /// The backend trait returns the whole `Vec<String>`, so the server does the
    /// paging: it sorts the result and cuts at the caller's cursor. Two
    /// consequences are worth stating rather than discovering.
    ///
    /// **The cursor is the last key returned, not an offset**, so a key inserted
    /// or removed between pages shifts nothing that was already delivered. An
    /// offset would silently skip or repeat a key under concurrent writes, which
    /// on a coordination keyspace is a wrong answer rather than a slow one.
    ///
    /// **Each page re-scans.** That is the honest cost of paginating over a
    /// whole-`Vec` backend method, and it is why the cap exists: the alternative —
    /// one unbounded message — is what §6.4 rules out. A backend-level cursor is
    /// the fix, and it is a change to the frozen cache contract (invariant I13),
    /// so it is not made here.
    async fn scan_prefix(
        &self,
        request: Request<stubs::ScanRequest>,
    ) -> Result<Response<stubs::ScanResponse>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let page_size = req
            .page_size
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_SCAN_PAGE_SIZE)
            .min(MAX_SCAN_PAGE_SIZE);

        let mut keys = bound
            .cache
            .scan_prefix(&req.prefix)
            .await
            .map_err(cluster_sdk::to_status)?;
        keys.sort_unstable();

        // `page_token` is the last key of the previous page, so the next page
        // starts strictly after it.
        let start = req.page_token.as_ref().map_or(0, |cursor| {
            keys.partition_point(|key| key.as_str() <= cursor.as_str())
        });
        let end = start.saturating_add(page_size).min(keys.len());
        let page: Vec<String> = keys.get(start..end).unwrap_or_default().to_vec();
        let next_page_token = if end < keys.len() {
            page.last().cloned()
        } else {
            None
        };

        Ok(Response::new(stubs::ScanResponse::from(
            dto::ScanResponse {
                keys: page,
                next_page_token,
            },
        )))
    }

    type WatchStream = CacheWatchStream;

    async fn watch(
        &self,
        request: Request<stubs::WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let watch = bound
            .cache
            .watch(&req.key)
            .await
            .map_err(cluster_sdk::to_status)?;
        Ok(Response::new(watch_stream(watch)))
    }

    type WatchPrefixStream = CacheWatchStream;

    async fn watch_prefix(
        &self,
        request: Request<stubs::WatchPrefixRequest>,
    ) -> Result<Response<Self::WatchPrefixStream>, Status> {
        let (_caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        // Deliberately the backend's own `watch_prefix`, including the SDK's
        // polling polyfill when the backend declares no native prefix watch: the
        // wire must not acquire a second prefix-watch implementation, or the two
        // profiles diverge on the semantics the polyfill defines (§6.3).
        let watch = bound
            .cache
            .watch_prefix(&req.prefix)
            .await
            .map_err(cluster_sdk::to_status)?;
        Ok(Response::new(watch_stream(watch)))
    }
}

impl CacheService {
    /// The acknowledgement every `()`-returning backend call answers with.
    ///
    /// It carries the registry generation, which is §5.6's staleness detector: a
    /// client learns the server's profile set moved without waiting for its
    /// descriptor poll. That the response had to carry *something* is protogen's
    /// doing (an empty message is rejected); that it carries this is the design's.
    fn ack(&self) -> stubs::PutResponse {
        stubs::PutResponse::from(dto::PutResponse {
            generation: self.ctx.profiles().generation(),
        })
    }
}

/// Milliseconds off the wire become the backend's [`Ttl`]; absent is indefinite.
fn ttl(ttl_ms: Option<u64>) -> Ttl {
    Ttl::from(ttl_ms.map(Duration::from_millis))
}

/// Turns a backend [`CacheWatch`] into a gRPC stream under §6.8's rules.
///
/// The pump is a spawned task rather than an `async_stream`, and that is what
/// makes the drop-then-`Lagged` rule implementable: the task must keep draining
/// the backend even while the remote subscriber is behind, because the alternative
/// — letting backpressure reach the backend — is one slow consumer stalling a
/// watch shared by every other consumer of that key.
///
/// **The stream carries no timeout.** Not here, and not on the service —
/// because §7.3 puts liveness on the consumer's own operations rather than on
/// the transport: "the transport owes no keepalive". The client side of that
/// rule is item `K2`'s; this side is asserted by
/// `watch_stream_outlives_an_rpc_timeout`.
///
/// This comment previously justified the rule by HTTP/2 keepalive and by a
/// deadline severing "every watch on a fixed interval". Both are false: no
/// keepalive is configured anywhere, and the severing does not reproduce
/// against tonic 0.14 (measured, Appendix A). The rule is right; the reasons
/// were not.
///
/// **Cancellation is unsubscription.** When the subscriber goes away tonic drops
/// the stream, which drops the receiver. The pump *selects* on `tx.closed()` as
/// well as the next backend event, and that arm is what makes the promise true:
/// waiting for the next send to fail would strand the pump on a quiet key, where
/// there is no next send — holding the [`CacheWatch`], and with it the backend's
/// watcher registration, for the life of the process. With the arm the pump exits
/// promptly and drops the [`CacheWatch`] — exactly what an in-process consumer
/// dropping its watch does (invariant I1). Its sibling
/// [`subscription_stream`](super::leader) and the SDK's own
/// `ScopedCacheBackend::strip_watch` carry the same arm for the same reason.
fn watch_stream(mut watch: CacheWatch) -> CacheWatchStream {
    let (tx, rx) = tokio::sync::mpsc::channel(WATCH_STREAM_BUFFER);

    tokio::spawn(async move {
        // Events dropped because this subscriber was behind, owed to it as a
        // `Lagged` as soon as there is room. Accumulated rather than sent
        // eagerly: reporting "you missed 1" three times is three more sends into
        // a channel that is already full, and the consumer's response to any
        // count is the same — re-read (§6.8).
        let mut owed_lagged: u64 = 0;

        loop {
            // Both arms are cancellation-safe: `mpsc::Receiver::recv` guarantees
            // no event is consumed when another branch wins, and `owed_lagged`
            // lives on the task's stack rather than inside either future, so a
            // cancelled poll cannot lose the debt it records.
            let event = tokio::select! {
                event = watch.recv() => event,
                // The subscriber went away; cancelling the stream is
                // unsubscribing, exactly as dropping a `CacheWatch` is
                // in-process. Without this arm a quiet key never wakes the task.
                () = tx.closed() => return,
            };
            // The backend dropped its sender without a terminal event. That is an
            // end of stream, not an error, and it reaches the consumer as one.
            let Some(event) = event else { return };

            let terminal = matches!(event, CacheWatchEvent::Closed(_));

            if owed_lagged > 0 {
                let lagged = to_dto(CacheWatchEvent::Lagged {
                    dropped: owed_lagged,
                });
                match tx.try_send(Ok(lagged)) {
                    Ok(()) => owed_lagged = 0,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                }
            }

            match tx.try_send(Ok(to_dto(event))) {
                Ok(()) => {}
                // The subscriber is behind. Drop the event and owe it a `Lagged`
                // — never block, or the backend's fan-out stalls for everyone.
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    owed_lagged = owed_lagged.saturating_add(1);
                    continue;
                }
                // The subscriber is gone; so is the subscription.
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
            }

            if terminal {
                // `Closed` is terminal by contract: the server sends it, then
                // closes the stream. Dropping the sender is what closes it.
                return;
            }
        }
    });

    ReceiverStream::new(rx)
}

/// A watch-union event becomes its flat wire form.
///
/// The union lives in the Rust type in both directions; the wire's discriminated
/// shape exists because protogen cannot express a `oneof` over payload-free
/// variants, and nothing above the §3.1 seam ever sees it (§6.8).
#[allow(
    clippy::match_same_arms,
    reason = "`Reset` and the wildcard produce the same event for different reasons - one is the backend saying the subscription was re-established, the other is this build meeting an event kind it does not know. Collapsing them would hide that the second case exists at all, and it is the one a future SDK version makes reachable"
)]
fn to_dto(event: CacheWatchEvent) -> stubs::CacheWatchEventDto {
    let dto = match event {
        CacheWatchEvent::Event(event) => dto::CacheWatchEventDto::from(event),
        CacheWatchEvent::Lagged { dropped } => dto::CacheWatchEventDto::lagged(dropped),
        CacheWatchEvent::Reset => dto::CacheWatchEventDto::reset(),
        CacheWatchEvent::Closed(error) => dto::CacheWatchEventDto::closed(wire_error(error)),
        // `CacheWatchEvent` is `#[non_exhaustive]` and lives in another crate, so
        // this arm is required rather than chosen. `Reset` is the safe reading of
        // an event this build does not understand — "you may have missed
        // something, re-read" — which is the same reasoning that makes it the
        // wire enum's `_UNSPECIFIED = 0` default (§6.8). Dropping the event
        // silently would be the one unsafe option.
        _ => dto::CacheWatchEventDto::reset(),
    };
    stubs::CacheWatchEventDto::from(dto)
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod cache_tests;
