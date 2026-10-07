# cf-gears-toolkit-onnx-runtime

What a gear needs to run an ONNX model in its own process, written once:

- **One `ort`.** The crate re-exports the workspace's pinned `ort`
  (`toolkit_onnx_runtime::ort`), loaded with `load-dynamic`: building needs no
  ONNX Runtime headers, running needs the shared library at `ORT_DYLIB_PATH`.
- **A session that cannot wedge the gear at boot.** `ort` 2.0.0-rc.12 hangs
  instead of erroring when the library at `ORT_DYLIB_PATH` cannot be loaded.
  `OnnxSession::open` builds the session on a dedicated `std::thread` and waits
  for it under a deadline; past it the thread is abandoned and the caller gets
  `OpenError::RuntimeHung`, after which the process should be restarted rather
  than retried.
- **Inference that does not stall the async runtime.** `ort`'s `Session::run`
  takes `&mut self`, so a session is used by one caller at a time.
  `OnnxSession::run` holds a fair async lock for the duration and runs the work
  under `block_in_place` on a multi-threaded runtime, so a call that takes
  hundreds of milliseconds does not hold a Tokio worker the rest of the gear
  needs. `OnnxSession::run_blocking` is the same from a blocking thread.

What a model's inputs and outputs mean -- tokenization, tensor names, pooling --
stays with the gear that runs it.

```rust,ignore
use toolkit_onnx_runtime::{OnnxSession, SessionOptions, ort};

let session = OnnxSession::open(&SessionOptions::new("model.onnx")).await?;
let width = session
    .run(|session| session.run(ort::inputs![/* ... */]).map(|out| out.len()))
    .await?;
```
