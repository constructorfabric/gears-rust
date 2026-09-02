//! The remote cache backend — DESIGN.md, the simple case.
//!
//! Ten of its twelve methods are one RPC each. The two that are not are the two
//! where the wire's shape and the trait's genuinely differ, and they differ in
//! opposite directions:
//!
//! - **`scan_prefix`** is paginated on the wire and an unbounded `Vec` on the
//!   trait, so the client loops pages back together (§6.4). The cursor is the
//!   server's `next_page_token`, not an offset.
//! - **`watch` / `watch_prefix`** are server-push streams and a channel on the
//!   trait, so each spawns one pump (§6.8).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;

use super::{RemoteProfile, provider};
use crate::cache::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheTerminalPermit, CacheWatch, CacheWatchSender,
    CacheWatchTrySendError, ClusterCacheBackend, PutRequest, Ttl,
};
use crate::client::remote::{CacheStub, decode};
use crate::convert::{from_status, to_cache_watch_event};
use crate::descriptors::DescriptorCache;
use crate::dto;
use crate::error::ClusterError;
use crate::grpc::stubs::cache as stubs;

/// How many events one remote watch buffers before it starts dropping and owing
/// a `Lagged`.
///
/// The same size the gear's stream pump uses, and for the same reason: a wedged
/// consumer must never apply backpressure to a shared subscription. There are now
/// two such buffers in series on a remote watch — the server's per-subscriber one
/// and this one — and a consumer can observe a `Lagged` from either. Both mean
/// the same thing to it (§6.8).
const WATCH_BUFFER: usize = 256;

/// [`ClusterCacheBackend`] over the wire (§12.10).
#[derive(Debug, Clone)]
pub struct RemoteCacheBackend {
    stub: CacheStub,
    profile: RemoteProfile,
}

impl RemoteCacheBackend {
    /// Binds a handle to `profile` over `stub`.
    pub fn new(stub: CacheStub, profile: &str, descriptors: Arc<DescriptorCache>) -> Self {
        Self {
            stub,
            profile: RemoteProfile::new(profile, descriptors),
        }
    }

    /// The cached cache descriptor, if one has been fetched.
    fn describe(&self) -> Option<dto::CacheDescriptor> {
        self.profile.descriptor().map(|profile| profile.cache)
    }

    /// A stub to issue one call on. Cloning is a refcount over the shared
    /// channel; the generated client does the same, because every RPC needs
    /// `&mut self` and a backend method has only `&self`.
    fn stub(&self) -> CacheStub {
        self.stub.clone()
    }

    /// Opens a watch stream and pumps it into a [`CacheWatch`].
    ///
    /// Shared by [`watch`](ClusterCacheBackend::watch) and
    /// [`watch_prefix`](ClusterCacheBackend::watch_prefix): the two differ only in
    /// which RPC opens the stream, and the pump is the whole of the rest.
    ///
    /// The subscription is established **before** returning, so a caller that
    /// gets `Ok` has a stream the server has accepted -- an error opening it is
    /// reported as an error from `watch`, not as an immediate `Closed` event.
    fn pump<S>(stream: S) -> CacheWatch
    where
        S: futures_util::Stream<Item = Result<stubs::WireCacheWatchEvent, tonic::Status>>
            + Send
            + 'static,
    {
        // The terminal slot is reserved here, at stream open, where the channel is
        // empty and the reservation cannot fail. Reserving it lazily inside the
        // pump would make the reservation itself subject to the back-pressure it
        // exists to survive, which is the failure it is here to prevent.
        let (sender, terminal, watch) = CacheWatch::channel_with_terminal_headroom(WATCH_BUFFER);
        tokio::spawn(drain(stream, sender, terminal));
        watch
    }
}

/// Forwards `stream` into `sender` until either end goes away.
///
/// `try_send` and never `send`: this task owns one HTTP/2 stream, and blocking it
/// on a consumer that has stopped draining would leave the server writing into a
/// flow-control window that never opens. Dropped events are coalesced into a
/// `Lagged`, which is exactly what the in-process sender does, so the
/// consumer-visible signal is identical in both deployment profiles (§6.8).
///
/// It also selects on the consumer's departure. Waiting only for the next frame
/// to arrive would park this task — and hold its HTTP/2 stream open — forever on
/// a quiet key, so the server would never see the cancellation either. The
/// scoped backend's forwarding task (`ScopedCacheBackend::strip_watch`) carries
/// the same arm for the same reason.
///
/// **The terminal `Closed` is the one event that is never dropped.** It travels
/// through `terminal`, a slot reserved at stream open, because `try_send` and a
/// full buffer used to mean the consumer saw *end of stream* where a `Closed`
/// was sent — and those are not the same event to `RestartingWatch`, which
/// reconnects on the first and stops on the second. A `Closed(Shutdown)` lost to
/// a full buffer therefore became an unbounded reconnect loop against a gear
/// that was shutting down (`RetryPolicy::default` caps nothing), and a full
/// buffer is most likely during exactly that drain.
async fn drain<S>(stream: S, sender: CacheWatchSender, terminal: CacheTerminalPermit)
where
    S: futures_util::Stream<Item = Result<stubs::WireCacheWatchEvent, tonic::Status>> + Send,
{
    let mut stream = std::pin::pin!(stream);
    let mut owed: u64 = 0;

    loop {
        // Both arms are cancellation-safe: a `StreamExt::next` that loses its
        // race was never handed an item to lose, and `owed` lives on this task's
        // stack rather than inside either future.
        let frame = tokio::select! {
            frame = stream.next() => frame,
            // The consumer dropped the watch: stop pumping promptly and release
            // the gRPC stream, even if the server would never send again.
            () = sender.closed() => return,
        };
        // The server closed the stream without a terminal event. That is an end
        // of stream rather than a failure, and dropping the sender is how the
        // consumer observes it - the same shape an in-process backend's dropped
        // sender has.
        let Some(frame) = frame else { return };

        let event = match frame {
            Ok(proto) => match decode::<dto::WireCacheWatchEvent, _>(proto) {
                Ok(dto) => to_cache_watch_event(dto),
                // A frame this build cannot decode is one missed mutation, not a
                // dead subscription: report it as a gap and keep the stream.
                Err(err) => {
                    tracing::warn!(%err, "cluster: undecodable cache watch frame");
                    crate::cache::CacheWatchEvent::Reset
                }
            },
            // Transport loss is terminal for *this* stream and retryable, so
            // `RestartingWatch` resubscribes rather than propagating (§6.9).
            // Through the reserved slot: a retryable close that the consumer never
            // sees is a reconnect it never makes.
            Err(status) => {
                terminal.close(from_status(&status));
                return;
            }
        };

        // The terminal event, out of the ordinary send path entirely. Flushing an
        // owed `Lagged` first keeps the "re-read before you see the next event"
        // ordering the drop-then-`Lagged` rule asks for; it stays best-effort,
        // because a lag notice lost behind a close costs nothing (the close is
        // itself a re-read instruction) while a lost close costs a reconnect loop.
        if let crate::cache::CacheWatchEvent::Closed(err) = event {
            if owed > 0 {
                let _dropped =
                    sender.try_send(crate::cache::CacheWatchEvent::Lagged { dropped: owed });
            }
            terminal.close(err);
            return;
        }

        if owed > 0
            && sender
                .try_send(crate::cache::CacheWatchEvent::Lagged { dropped: owed })
                .is_ok()
        {
            owed = 0;
        }
        match sender.try_send(event) {
            Ok(()) => {}
            // The consumer is behind. Drop the event and owe it a `Lagged`; the
            // loop continues either way now that the terminal event has its own
            // exit above, so there is nothing left for a `continue` to skip.
            Err(CacheWatchTrySendError::Full) => owed = owed.saturating_add(1),
            Err(CacheWatchTrySendError::Closed) => return,
        }
    }
}

#[async_trait]
impl ClusterCacheBackend for RemoteCacheBackend {
    /// From the descriptor cache, never a call (§5.5).
    ///
    /// An unfetched descriptor answers `EventuallyConsistent`: it is the weaker
    /// guarantee, so a `require(Linearizable)` fails rather than being falsely
    /// satisfied. That is the same reading the wire enum's `_UNSPECIFIED = 0`
    /// takes, for the same reason.
    fn consistency(&self) -> CacheConsistency {
        self.describe()
            .map_or(CacheConsistency::EventuallyConsistent, |cache| {
                cache.consistency.into()
            })
    }

    /// From the descriptor cache. An unfetched descriptor declares nothing, so a
    /// caller polyfills a prefix watch rather than assuming native support.
    fn features(&self) -> CacheFeatures {
        self.describe()
            .map_or_else(|| CacheFeatures::new(false), |cache| cache.features.into())
    }

    /// The **server-side** provider (§5.5) — `"postgres"`, not
    /// `RemoteCacheBackend`. It is what an operator reading a capability failure
    /// needs: which real backend could not satisfy the requirement.
    fn provider_name(&self) -> &'static str {
        provider(self.describe().map(|cache| cache.provider))
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let request = stubs::GetRequest::from(dto::GetRequest {
            profile: self.profile.name(),
            key: key.to_owned(),
        });
        let response = self
            .stub()
            .get(request)
            .await
            .map_err(|status| from_status(&status))?;
        let decoded = decode::<dto::GetResponse, _>(response.into_inner())?;
        Ok(decoded.entry.map(Into::into))
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        let request = stubs::PutRequest::from(dto::PutRequest {
            profile: self.profile.name(),
            key: req.key.to_owned(),
            value: req.value.to_vec(),
            ttl_ms: ttl_ms(req.ttl),
            client_request_id: None,
        });
        self.stub()
            .put(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        let request = stubs::DeleteRequest::from(dto::DeleteRequest {
            profile: self.profile.name(),
            key: key.to_owned(),
        });
        let response = self
            .stub()
            .delete(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(decode::<dto::DeleteResponse, _>(response.into_inner())?.existed)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let request = stubs::ContainsRequest::from(dto::ContainsRequest {
            profile: self.profile.name(),
            key: key.to_owned(),
        });
        let response = self
            .stub()
            .contains(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(decode::<dto::ContainsResponse, _>(response.into_inner())?.present)
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        let request = stubs::PutRequest::from(dto::PutRequest {
            profile: self.profile.name(),
            key: req.key.to_owned(),
            value: req.value.to_vec(),
            ttl_ms: ttl_ms(req.ttl),
            client_request_id: None,
        });
        let response = self
            .stub()
            .put_if_absent(request)
            .await
            .map_err(|status| from_status(&status))?;
        let decoded = decode::<dto::PutIfAbsentResponse, _>(response.into_inner())?;
        Ok(decoded.created.map(Into::into))
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        let request = stubs::CasRequest::from(dto::CasRequest {
            profile: self.profile.name(),
            key: key.to_owned(),
            expected_version,
            new_value: new_value.to_vec(),
            ttl_ms: ttl_ms(ttl),
        });
        let response = self
            .stub()
            .compare_and_swap(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(decode::<dto::CasResponse, _>(response.into_inner())?
            .entry
            .into())
    }

    /// Overridden rather than inherited, and that is load-bearing.
    ///
    /// The trait's default is a non-atomic `get`-then-`delete`, which is a real
    /// race over a network — and the CAS-based lock and leader release depend on
    /// this being atomic (§6.3). The wire carries the operation for exactly that
    /// reason, so a remote backend must issue it rather than fall back.
    async fn compare_and_delete(
        &self,
        key: &str,
        expected_value: &[u8],
    ) -> Result<bool, ClusterError> {
        let request = stubs::CadRequest::from(dto::CadRequest {
            profile: self.profile.name(),
            key: key.to_owned(),
            expected_value: expected_value.to_vec(),
        });
        let response = self
            .stub()
            .compare_and_delete(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(decode::<dto::CadResponse, _>(response.into_inner())?.deleted)
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        let request = stubs::WatchRequest::from(dto::WatchRequest {
            profile: self.profile.name(),
            key: key.to_owned(),
        });
        let stream = self
            .stub()
            .watch(request)
            .await
            .map_err(|status| from_status(&status))?
            .into_inner();
        Ok(Self::pump(stream))
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        let request = stubs::WatchPrefixRequest::from(dto::WatchPrefixRequest {
            profile: self.profile.name(),
            prefix: prefix.to_owned(),
        });
        let stream = self
            .stub()
            .watch_prefix(request)
            .await
            .map_err(|status| from_status(&status))?
            .into_inner();
        Ok(Self::pump(stream))
    }

    /// The trait's unbounded `Vec`, reassembled from the wire's pages (§6.4).
    ///
    /// The cursor is the server's `next_page_token` — the last key it returned —
    /// rather than an offset, so a concurrent insert cannot make the scan skip a
    /// key it has not yet reached. The loop ends when the server stops issuing
    /// one, which it does on the last page.
    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let mut keys = Vec::new();
        let mut page_token = None;
        loop {
            let request = stubs::ScanRequest::from(dto::ScanRequest {
                profile: self.profile.name(),
                prefix: prefix.to_owned(),
                page_size: None,
                page_token,
            });
            let response = self
                .stub()
                .scan_prefix(request)
                .await
                .map_err(|status| from_status(&status))?;
            let page = decode::<dto::ScanResponse, _>(response.into_inner())?;
            keys.extend(page.keys);
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(keys),
            }
        }
    }

    /// Left at the trait's `Ok(())` default, deliberately.
    ///
    /// `probe()` reports on a backend's **own** resources for the serving gear's
    /// readiness check (§4.4), and this handle has none: its resource is the
    /// channel, whose far side runs that check over the real backends and
    /// publishes the verdict as per-profile health on the descriptor. A consumer
    /// learns a profile is degraded by reading that health — `K5`'s gate — not by
    /// probing across the wire, which would only ask the server whether it can
    /// reach itself.
    async fn probe(&self) -> Result<(), ClusterError> {
        Ok(())
    }
}

/// The wire's optional millisecond TTL.
///
/// [`Ttl::Indefinite`] is `None` rather than a sentinel: the field is optional on
/// the wire precisely so "no expiry" needs no magic number, and the server's
/// `Ttl::from(Option<Duration>)` reads it back the same way.
fn ttl_ms(ttl: Ttl) -> Option<u64> {
    ttl.as_duration().map(duration_ms)
}

/// Milliseconds, saturating. A TTL that overflows `u64` milliseconds is 584
/// million years; saturation is here so the conversion is total, not because it
/// is reachable.
pub fn duration_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod audit_tests {
    use std::time::Duration;

    use super::{CacheWatch, drain};
    use crate::grpc::stubs::cache as stubs;

    /// A stream that never yields — the client-side shape of a quiet key, and
    /// the only interesting input: it is exactly the case where waiting for the
    /// next frame is waiting forever.
    fn quiet() -> impl futures_util::Stream<Item = Result<stubs::WireCacheWatchEvent, tonic::Status>>
    + Send
    + 'static {
        futures_util::stream::pending()
    }

    #[tokio::test]
    async fn audit_drain_notices_a_dropped_cache_watch() {
        // `WATCH-1`, client side. `ScopedCache::strip_watch` reaches this through
        // the ordinary facade: it exits on `tx.closed()` and drops the inner
        // remote `CacheWatch`, which must end this pump and release its gRPC
        // stream. Until it does, the server never sees the cancellation either,
        // so the server-side leak is unreachable without this half.
        let (sender, terminal, watch) = CacheWatch::channel_with_terminal_headroom(8);
        let pump = tokio::spawn(drain(quiet(), sender, terminal));

        drop(watch);

        let ended = tokio::time::timeout(Duration::from_secs(5), pump).await;
        assert!(
            ended.is_ok(),
            "dropping a remote `CacheWatch` must end its `drain` pump and release the gRPC \
             stream; the pump is still parked on `stream.next()` 5 s later"
        );
    }

    #[tokio::test]
    async fn drain_still_ends_when_the_server_ends_the_stream() {
        // The other exit from the same loop, so the added arm cannot have
        // swallowed it: an ended stream with no terminal event ends the pump and
        // drops the sender, which is the consumer's end-of-stream.
        let (sender, terminal, mut watch) = CacheWatch::channel_with_terminal_headroom(8);
        let pump = tokio::spawn(drain(futures_util::stream::empty(), sender, terminal));

        assert!(
            tokio::time::timeout(Duration::from_secs(5), pump)
                .await
                .is_ok(),
            "an ended stream must end the pump"
        );
        assert!(
            watch.recv().await.is_none(),
            "and the consumer observes end-of-stream, not a hang"
        );
    }
}

/// B3 — the terminal `Closed` under back-pressure, at the layer the consequence
/// shows up in.
///
/// The pump's own accounting is asserted on the gear side
/// (`api/grpc/cache.rs`'s `terminal_close_under_backpressure`). What this covers
/// is why it matters: [`RestartingWatch`](crate::restart::RestartingWatch) makes
/// opposite decisions on `Closed(Shutdown)` and on end of stream, so a terminal
/// event lost to a full buffer is not a missed notification — it is a reconnect
/// loop against a gear that is shutting down.
#[cfg(test)]
mod terminal_close_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::{WATCH_BUFFER, drain};
    use crate::cache::{CacheWatch, CacheWatchEvent};
    use crate::dto;
    use crate::error::ClusterError;
    use crate::grpc::stubs::cache as stubs;
    use crate::restart::RetryPolicy;

    /// Enough ordinary frames to overrun the buffer before the close arrives.
    const OVERFLOW: usize = WATCH_BUFFER + 50;

    /// The frame sequence a draining gear sends a subscriber that has stopped
    /// reading: a burst it cannot keep up with, then the terminal close.
    fn frames() -> Vec<Result<stubs::WireCacheWatchEvent, tonic::Status>> {
        let mut frames: Vec<Result<stubs::WireCacheWatchEvent, tonic::Status>> = (0..OVERFLOW)
            .map(|index| {
                Ok(stubs::WireCacheWatchEvent::from(
                    dto::WireCacheWatchEvent::key_event(
                        dto::CacheWatchEventKind::Changed,
                        format!("ledger-{index}"),
                    ),
                ))
            })
            .collect();
        frames.push(Ok(stubs::WireCacheWatchEvent::from(
            dto::WireCacheWatchEvent::closed(dto::WireError::from(crate::ClusterWireError::from(
                ClusterError::Shutdown,
            ))),
        )));
        frames
    }

    /// **The test that matters.** The same scenario, one layer up: a subscriber
    /// that fell behind must observe the shutdown as terminal, not as an end of
    /// stream that `RestartingWatch` reconnects on — and `RetryPolicy::default`
    /// caps nothing, so "reconnects" means forever.
    #[tokio::test(start_paused = true)]
    async fn a_close_delivered_on_a_full_buffer_triggers_no_reconnect() {
        let (sender, terminal, mut watch) =
            CacheWatch::channel_with_terminal_headroom(WATCH_BUFFER);
        // A resubscribe seam, because without one `RestartingWatch` cannot
        // reconnect at all and the test would pass for the wrong reason. It counts
        // attempts and hands back a fresh subscription, so a reconnect succeeds
        // and is visible rather than being an error.
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&attempts);
        watch.set_resubscribe(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                let (_fresh_tx, fresh) = CacheWatch::channel(8);
                Ok(fresh)
            })
        });

        // Await the pump rather than sleeping: the consumer must not read (and so
        // free room) until every frame has been pumped into a full buffer.
        let pump = tokio::spawn(drain(
            futures_util::stream::iter(frames()),
            sender,
            terminal,
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(5), pump)
                .await
                .is_ok(),
            "the pump ends when the frame stream does"
        );

        let mut restarting = watch.auto_restart(RetryPolicy::default());
        let mut ordinary = 0_usize;
        let mut closed = None;
        while let Some(event) = restarting.recv().await {
            match event {
                CacheWatchEvent::Event(_) => ordinary += 1,
                CacheWatchEvent::Closed(err) => closed = Some(err),
                other => {
                    panic!("unexpected event on a stream that only burst and closed: {other:?}")
                }
            }
        }

        assert!(
            matches!(closed, Some(ClusterError::Shutdown)),
            "the consumer must see the gear's `Closed(Shutdown)`, got {closed:?}"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "a terminal shutdown must not be reconnected to; a dropped close reaches this layer \
             as an end of stream, which is the canonical reconnect trigger"
        );
        assert_eq!(
            ordinary, WATCH_BUFFER,
            "the reserved slot must not have cost the consumer a buffer slot"
        );
    }
}
