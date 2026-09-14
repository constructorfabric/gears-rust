use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use tracing::{debug, info};

use file_parser_sdk::FileParserClientV1;

use crate::config::FileParserConfig;
use crate::domain::local_client::FileParserLocalClient;
use crate::domain::service::{FileParserService, ServiceConfig};
use crate::infra::parsers::{
    DocxParser, ImageParser, KreuzbergParser, PlainTextParser, StubParser,
};

/// Main gear struct for file parsing
#[toolkit::gear(
    name = "file-parser",
    capabilities = [rest]
)]
pub struct FileParserGear {
    service: OnceLock<Arc<FileParserService>>,
}

impl Default for FileParserGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

/// Why the Magika content-type detector failed to initialize.
///
/// A dedicated type rather than `anyhow::Error` because two variants leave an
/// abandoned OS thread behind, which is only sound if the caller then terminates
/// the process. [`Self::leaked_init_thread`] makes that checkable in code rather
/// than documented in a comment.
#[cfg(feature = "magika")]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MagikaInitError {
    /// No result within the timeout — almost always the hang described on
    /// [`init_magika_detector`]. The thread is abandoned, not killed.
    #[error(
        "Magika content-type detector did not finish loading within {}s; the ONNX Runtime is \
         likely missing, the wrong architecture, or incompatible with the version `ort` was \
         built against — check ORT_DYLIB_PATH (the init thread is abandoned, not killed, and \
         will leak until the process exits)",
        timeout.as_secs()
    )]
    Timeout { timeout: std::time::Duration },

    /// `cancellation_token` fired before init finished. The init thread may
    /// still be running, and may be wedged.
    #[error("Magika content-type detector initialization cancelled by gear shutdown")]
    Cancelled,

    /// The init thread vanished without sending a result — it panicked, or the
    /// `oneshot` sender was dropped. The thread is gone, so nothing leaks.
    #[error("Magika detector init thread died unexpectedly: {0}")]
    InitThreadDied(#[source] tokio::sync::oneshot::error::RecvError),

    /// ONNX Runtime and the model loaded far enough to return a real error.
    /// The init thread completed, so nothing leaks.
    #[error("failed to load Magika content-type detector: {0}")]
    SessionLoad(#[source] magika::Error),
}

#[cfg(feature = "magika")]
impl MagikaInitError {
    /// Whether this failure left an OS thread that will never be joined.
    ///
    /// `true` means the caller must terminate the process for the leak to stay
    /// bounded. Note that exiting while that thread sits inside ONNX Runtime
    /// init can abort rather than exit cleanly (ORT's `cpuinfo` asserts if
    /// teardown races init), so the observable failure may be a signal rather
    /// than an exit code.
    #[must_use]
    pub const fn leaked_init_thread(&self) -> bool {
        match self {
            // `Timeout` is wedged by definition. `Cancelled` may just be a
            // thread still loading, but we cannot tell it apart from a wedged
            // one here, so assume the worse case.
            Self::Timeout { .. } | Self::Cancelled => true,
            // Both mean the thread ran to completion and is gone.
            Self::InitThreadDied(_) | Self::SessionLoad(_) => false,
        }
    }
}

/// Loads the Magika content-type detector eagerly at gear startup, so a
/// missing model/runtime fails gear startup rather than the first request.
///
/// # The `ort` 2.0.0-rc.12 startup hang
///
/// Bounded by a timeout because `ort` 2.0.0-rc.12 **hangs forever instead of
/// erroring** when the ONNX Runtime library it `dlopen`s via `ORT_DYLIB_PATH`
/// cannot be loaded. Reproduced: against a nonexistent path,
/// `magika::Session::new()` returns neither `Ok` nor `Err` nor panics in 45 s.
///
/// Before simplifying this away, two measured facts:
///
/// - **No pre-flight validation exists.** `ort::init_from(path)` looks like one
///   (takes a path, returns `Result`) but hangs on the same input, also at 45 s.
///   Every `ort` entry point funnels through the same lazy dylib init.
/// - **The internal mechanism is unknown.** An earlier version of this comment
///   blamed a reentrant `ort::api()` call, which the `init_from` result
///   disproves. Treat the hang as an observed property of a bad
///   `ORT_DYLIB_PATH`, not as something understood well enough to narrow.
///
/// Since the hang cannot be interrupted from inside, the blocked thread has to be
/// abandoned. A raw `std::thread` rather than `spawn_blocking`, because Tokio
/// joins blocking threads on shutdown, so a wedged one would hang the whole
/// runtime's shutdown. Only the `oneshot` receiver is awaited. See
/// [`MagikaInitError::leaked_init_thread`] for the caller's obligation.
///
/// TODO: drop this once `magika`/`ort` fix the hang or offer a safe way to
/// validate the dylib first. No upstream issue filed as of 2026-08-18; the
/// `init_from` reproduction above is what to report against `pykeio/ort`. Check
/// for a fix above `2.0.0-rc.12` before bumping the workspace pin.
///
/// # Visibility
///
/// `#[doc(hidden)] pub` only so `tests/magika_timeout_tests.rs` can drive this
/// exact path; an integration test cannot reach `pub(crate)`. Not public API.
///
/// # Errors
///
/// See [`MagikaInitError`] — two variants oblige the caller to terminate.
#[cfg(feature = "magika")]
#[doc(hidden)]
pub async fn init_magika_detector(
    parsers: &[Arc<dyn crate::domain::parser::FileParserBackend>],
    intra_op_threads: Option<std::num::NonZeroUsize>,
    cancellation_token: &tokio_util::sync::CancellationToken,
) -> Result<Arc<dyn crate::domain::detector::ContentTypeDetector>, MagikaInitError> {
    // Flat, not scaled — one session means one model load. A healthy one builds
    // in well under a second and the hang never completes, so a longer timeout
    // would only delay a legible startup failure.
    const MAGIKA_INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    // Check before spawning, not just in the `select!` below. Abandoning a
    // thread that is midway through ONNX Runtime init can abort the process on
    // exit (ORT's `cpuinfo` asserts if teardown races init), so if shutdown has
    // already begun, don't start work we know we are about to discard.
    if cancellation_token.is_cancelled() {
        return Err(MagikaInitError::Cancelled);
    }

    let supported_extensions: Vec<String> = parsers
        .iter()
        .flat_map(|p| p.supported_extensions().iter().map(|ext| (*ext).to_owned()))
        .collect();

    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        // The receiver may already be gone if we timed out and moved on; a
        // dropped receiver just means `send` fails harmlessly.
        drop(tx.send(crate::infra::MagikaDetector::with_config(
            supported_extensions,
            intra_op_threads,
        )));
    });

    // Race the bounded wait against shutdown, so it doesn't block graceful
    // shutdown for the full timeout.
    let detector = tokio::select! {
        result = tokio::time::timeout(MAGIKA_INIT_TIMEOUT, rx) => {
            result
                .map_err(|_elapsed| MagikaInitError::Timeout { timeout: MAGIKA_INIT_TIMEOUT })?
                .map_err(MagikaInitError::InitThreadDied)?
                .map_err(MagikaInitError::SessionLoad)?
        }
        () = cancellation_token.cancelled() => {
            return Err(MagikaInitError::Cancelled);
        }
    };

    info!("Magika content-type detection enabled");
    Ok(Arc::new(detector) as Arc<dyn crate::domain::detector::ContentTypeDetector>)
}

#[async_trait]
impl Gear for FileParserGear {
    #[allow(clippy::cast_possible_truncation)]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        const BYTES_IN_MB: u64 = 1024_u64 * 1024;

        // Load gear configuration
        let cfg: FileParserConfig = ctx.config()?;
        debug!(
            "Loaded file-parser config: max_file_size_mb={}",
            cfg.max_file_size_mb
        );

        let max_file_size_bytes = cfg.max_file_size_mb.saturating_mul(BYTES_IN_MB);

        // The two backends that read a whole local file into memory get the
        // configured ceiling, so their bounded reads agree with the
        // service-level check rather than a compiled-in default.
        let parsers: Vec<Arc<dyn crate::domain::parser::FileParserBackend>> = vec![
            Arc::new(PlainTextParser::new().with_max_bytes(max_file_size_bytes)),
            Arc::new(KreuzbergParser::new()),
            Arc::new(DocxParser::new()),
            Arc::new(ImageParser::new()),
            Arc::new(StubParser::new().with_max_bytes(max_file_size_bytes)),
        ];

        info!("Registered {} parser backends", parsers.len());

        // Canonicalize at startup so we only do it once.
        let allowed_local_base_dir = cfg.allowed_local_base_dir.canonicalize().map_err(|e| {
            anyhow::anyhow!(
                "allowed_local_base_dir '{}' cannot be resolved: {e}",
                cfg.allowed_local_base_dir.display()
            )
        })?;
        if !allowed_local_base_dir.is_dir() {
            return Err(anyhow::anyhow!(
                "allowed_local_base_dir '{}' is not a directory",
                allowed_local_base_dir.display()
            ));
        }
        info!(
            allowed_local_base_dir = %allowed_local_base_dir.display(),
            "Local file parsing restricted to base directory"
        );

        // Create service config from gear config
        let service_config = ServiceConfig {
            max_file_size_bytes: usize::try_from(max_file_size_bytes).unwrap_or(usize::MAX),
            allowed_local_base_dir,
        };

        // With the `magika` feature, load the content-type detector eagerly
        // so a missing model/runtime fails gear startup, not the first
        // request. See `init_magika_detector` for why this is bounded by a
        // timeout instead of a plain `.await`.
        // `Err` from `Gear::init` terminates the process, which is what
        // `leaked_init_thread` requires. Making this recoverable means dealing
        // with the leaked thread.
        #[cfg(feature = "magika")]
        let detector = match init_magika_detector(
            &parsers,
            cfg.magika_intra_op_threads,
            ctx.cancellation_token(),
        )
        .await
        {
            Ok(detector) => detector,
            Err(e) => {
                if e.leaked_init_thread() {
                    tracing::error!(
                        error = %e,
                        "Magika init left an abandoned OS thread; this is only bounded because \
                         gear init failure terminates the process"
                    );
                }
                return Err(e.into());
            }
        };

        // Fail startup on a bad threshold rather than clamping a value the
        // operator plainly did not mean (e.g. a percentage typed as `90`).
        let detection_confidence_threshold =
            crate::domain::detector::Confidence::new(cfg.detection_confidence_threshold)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "detection_confidence_threshold ({}) must be a number in [0.0, 1.0]",
                        cfg.detection_confidence_threshold
                    )
                })?;

        // Create file parser service
        #[allow(unused_mut)]
        let mut service = FileParserService::new(parsers, service_config)
            .with_detection_confidence_threshold(detection_confidence_threshold);
        #[cfg(feature = "magika")]
        {
            service = service.with_detector(detector);
        }
        let file_parser_service = Arc::new(service);

        // Register the in-process ClientHub client so other gears can call
        // file-parser without going over HTTP.
        let client: Arc<dyn FileParserClientV1> =
            Arc::new(FileParserLocalClient::new(file_parser_service.clone()));
        ctx.client_hub().register::<dyn FileParserClientV1>(client);

        // Store service for REST usage
        self.service
            .set(file_parser_service)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        Ok(())
    }
}

impl RestApiCapability for FileParserGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering file-parser REST routes");

        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        let router = crate::api::rest::routes::register_routes(router, openapi, service);

        info!("File parser REST routes registered successfully");
        Ok(router)
    }
}
