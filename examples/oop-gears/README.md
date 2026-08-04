# OoP examples

Out-of-process (OoP) gears run as their own process and communicate over the
network. Per `cpt-cf-adr-rest-first-oop`, **REST is the default and primary
protocol** for OoP gear APIs; gRPC is an **opt-in** for performance-critical
internal paths and is never required for standard gear communication.

The examples below are organised along that split.

## REST-first (the default) — start here

- **`hello/` + `secure/`** — the canonical REST-first OoP demo. The built-in
  `api-gateway` (embedded edge, Mode A) discovers separate `hello-oop` /
  `secure-oop` pods via the `DirectoryService` and reverse-proxies their public
  REST routes (`cpt-cf-component-gateway-provider`). It also exercises two-plane
  auth (`cpt-cf-adr-two-plane-auth`) and platform-plane internal auth
  (`cpt-cf-adr-platform-plane-auth`).

  Run the whole thing (boots the host + both pods and asserts 9 scenarios):

  ```bash
  examples/oop-gears/hello/run-demo.sh
  ```

  See `hello/README.md` for the step-by-step breakdown.

## gRPC (opt-in / advanced)

- **`calculator/` + `calculator-gateway/`** — the **opt-in gRPC** OoP path
  (`.proto` + tonic client). This is *not* the default transport; it exists to
  show the performance-critical gear-to-gear path the ADR preserves. Prefer the
  REST-first examples above unless you specifically need gRPC.

  Two ways to run it:

  ### ALL IN

  ```bash
  # Build the calculator-oop binary
  cargo build --bin calculator-oop --features oop_gear -p calculator

  # Run the master with OoP gears enabled
  cargo run --bin cf-gears-server --features oop-example -- --config config/oop-example-master+follower.yaml
  ```

  ### SEPARATE

  ```bash
  # Run the master with OoP gears enabled
  cargo run --bin cf-gears-server --features oop-example -- --config config/oop-example-master.yaml

  export TOOLKIT_DIRECTORY_ENDPOINT=http://127.0.0.1:50051
  # Run the follower with OoP gears enabled
  cargo run --bin calculator-oop --features oop_gear -p calculator -- --config config/oop-example-follower.yaml
  ```
