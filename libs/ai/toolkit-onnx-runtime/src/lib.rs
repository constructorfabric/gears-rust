//! ONNX Runtime support for gears: one pinned `ort`, a session opened under a
//! deadline on a thread that can be abandoned, and inference that does not
//! stall the async runtime.
//!
//! What a model's inputs and outputs mean -- tokenization, tensor names,
//! pooling -- stays with the gear that runs it. This crate owns only what is
//! the same for every in-process model: how the runtime is reached, how a
//! session is opened without risking a boot that never finishes, and how one
//! session is shared by async callers.

use std::path::PathBuf;
use std::time::Duration;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;

/// The workspace's pinned `ort`, so a gear reaches the runtime through one
/// version and one feature set (`load-dynamic`, `api-24`).
pub use ort;

/// How long [`OnnxSession::open`] waits for the runtime by default.
pub const DEFAULT_LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// What to open, and how long to wait for it.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// The model file. Read by the runtime; this crate fetches nothing.
    pub model_path: PathBuf,
    /// ONNX Runtime intra-op threads. `None` leaves the runtime's default.
    pub intra_op_threads: Option<usize>,
    /// How long to wait for the runtime and the model before deciding it has
    /// hung (see [`OpenError::RuntimeHung`]).
    pub load_timeout: Duration,
    /// Name of the thread the session is built on, so an abandoned one is
    /// recognisable in a thread dump.
    pub thread_name: String,
}

impl SessionOptions {
    /// Defaults: the runtime's thread count, [`DEFAULT_LOAD_TIMEOUT`], and a
    /// thread named `onnx-init`.
    #[must_use]
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            intra_op_threads: None,
            load_timeout: DEFAULT_LOAD_TIMEOUT,
            thread_name: "onnx-init".to_owned(),
        }
    }
}

/// Why a session did not open.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The runtime refused the model or the options, or the thread that was
    /// to build the session could not be started or ended without a result.
    #[error("ONNX session could not be created: {0}")]
    Session(String),
    /// The one failure a caller cannot recover from in-process.
    #[error(
        "ONNX Runtime did not load within {seconds}s. `ort` 2.0.0-rc.12 hangs \
         instead of erroring on an unloadable library, so the thread that \
         tried is abandoned rather than killed: check ORT_DYLIB_PATH and \
         restart the process"
    )]
    RuntimeHung {
        /// The deadline that passed.
        seconds: u64,
    },
}

/// One ONNX Runtime session, shared by async callers one at a time.
///
/// `Session::run` takes `&mut self`, so inference is serialized whatever the
/// sharing. A fair lock over one session is then the honest shape: extra
/// sessions would each hold a resident copy of the weights and their own
/// intra-op thread pool.
pub struct OnnxSession {
    inner: Exclusive<Session>,
}

impl OnnxSession {
    /// Build the session on an abandonable thread and wait for it under
    /// `options.load_timeout`.
    ///
    /// # The hang this guards against
    ///
    /// `ort` 2.0.0-rc.12 **hangs forever instead of erroring** when the
    /// library at `ORT_DYLIB_PATH` cannot be loaded -- measured against a
    /// nonexistent path, where neither `Session::builder` nor `ort::init_from`
    /// returns in 45 seconds. No pre-flight validation exists; every entry
    /// point funnels through the same lazy init. Since the hang cannot be
    /// interrupted from inside, the blocked thread is **abandoned**: a raw
    /// `std::thread` rather than `spawn_blocking`, because Tokio joins
    /// blocking threads at shutdown and a wedged one would hang that too. A
    /// caller receiving [`OpenError::RuntimeHung`] has leaked one thread and
    /// must terminate the process rather than retry.
    ///
    /// # Errors
    ///
    /// [`OpenError::Session`] when the runtime refuses, [`OpenError::RuntimeHung`]
    /// when it does not answer in time.
    pub async fn open(options: &SessionOptions) -> Result<Self, OpenError> {
        let model_path = options.model_path.clone();
        let threads = options.intra_op_threads;
        let session =
            on_abandonable_thread(&options.thread_name, options.load_timeout, move || {
                build_session(&model_path, threads)
            })
            .await
            .inspect_err(|error| {
                if matches!(error, OpenError::RuntimeHung { .. }) {
                    warn!(
                        path = %options.model_path.display(),
                        "ONNX Runtime did not load in time; leaking the init thread deliberately"
                    );
                }
            })?;
        Ok(Self {
            inner: Exclusive::new(session),
        })
    }

    /// Run `work` with the session, from async code.
    ///
    /// Waits for the session in arrival order, then runs `work` under
    /// `block_in_place` on a multi-threaded runtime: `Session::run` is
    /// synchronous CPU work measured in tens to hundreds of milliseconds, and
    /// called directly it would hold a Tokio worker that the rest of the gear
    /// -- database round trips, mostly -- is waiting for. On a current-thread
    /// runtime there is no other worker to starve and `block_in_place` would
    /// panic, so `work` runs directly.
    ///
    /// Anything to check once the wait is over -- a deadline, a cancellation
    /// -- belongs at the start of `work`, which runs only after the session
    /// has been acquired.
    pub async fn run<R>(&self, work: impl FnOnce(&mut Session) -> R) -> R {
        self.inner.run(work).await
    }

    /// Run `work` with the session from a blocking thread (`spawn_blocking`,
    /// a raw thread), where awaiting is not available.
    ///
    /// # Panics
    ///
    /// When called from async code: use [`run`](Self::run) there.
    pub fn run_blocking<R>(&self, work: impl FnOnce(&mut Session) -> R) -> R {
        self.inner.run_blocking(work)
    }

    /// Wait until no caller holds the session. What a health check can say
    /// without running inference of its own: the session is not wedged
    /// behind a call that never returned.
    pub async fn wait_until_idle(&self) {
        self.inner.wait_until_idle().await;
    }
}

/// A runtime refusal, whichever builder stage raised it (`ort` types the
/// errors of the session builder apart from the rest).
fn refused(error: impl std::fmt::Display) -> OpenError {
    OpenError::Session(error.to_string())
}

fn build_session(
    model_path: &std::path::Path,
    intra_op_threads: Option<usize>,
) -> Result<Session, OpenError> {
    let mut builder = Session::builder()
        .map_err(refused)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(refused)?;
    if let Some(threads) = intra_op_threads {
        builder = builder.with_intra_threads(threads).map_err(refused)?;
    }
    builder.commit_from_file(model_path).map_err(refused)
}

/// Run `build` on a dedicated thread and wait for it at most `timeout`.
///
/// Past the deadline the thread is left running and [`OpenError::RuntimeHung`]
/// is returned; its result, should it ever arrive, is dropped.
async fn on_abandonable_thread<T: Send + 'static>(
    name: &str,
    timeout: Duration,
    build: impl FnOnce() -> Result<T, OpenError> + Send + 'static,
) -> Result<T, OpenError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // The receiver is gone when the timeout already fired; that is the
            // abandoned case, and dropping the result here is correct.
            drop(tx.send(build()));
        })
        .map_err(|error| OpenError::Session(error.to_string()))?;

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(result)) => result,
        // The sender was dropped without sending: the thread panicked.
        Ok(Err(_)) => Err(OpenError::Session(
            "the ONNX init thread ended without a result".to_owned(),
        )),
        Err(_) => Err(OpenError::RuntimeHung {
            seconds: timeout.as_secs(),
        }),
    }
}

/// The sharing rule of [`OnnxSession`], over any value, so it can be held to
/// its contract without a runtime library to load.
struct Exclusive<T> {
    value: Mutex<T>,
}

impl<T> Exclusive<T> {
    fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
        }
    }

    async fn run<R>(&self, work: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.value.lock().await;
        if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
        }) {
            tokio::task::block_in_place(|| work(&mut guard))
        } else {
            work(&mut guard)
        }
    }

    fn run_blocking<R>(&self, work: impl FnOnce(&mut T) -> R) -> R {
        work(&mut self.value.blocking_lock())
    }

    async fn wait_until_idle(&self) {
        drop(self.value.lock().await);
    }
}

#[cfg(test)]
mod lib_tests;
