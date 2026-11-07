#!/usr/bin/env bash
# Usage: cargo_load.sh <dir> <minutes> <report.json>
# Builds public crates only (no project code) in parallel, runs the produced
# binaries, churns the disk, and counts I/O-error signatures.
set -u

DIR=$1
MINUTES=$2
REPORT=$3

mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd)
mkdir -p "$(dirname "$REPORT")"
LOGS="$DIR/logs"
mkdir -p "$LOGS"
SIG='Illegal byte sequence|Bus error|disk I/O error|Input/output error|os error 92|os error 5'

cd "$DIR" || exit 1
cargo new --bin probe >/dev/null 2>&1 || cargo new --bin probe
cd probe || exit 1
cat > Cargo.toml <<'TOML'
[package]
name = "probe"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
sqlx = { version = "0.8", default-features = false, features = ["sqlite", "runtime-tokio"] }
rusqlite = { version = "0.32", features = ["bundled"] }
reqwest = "0.12"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
zerocopy = "0.8"
syn = { version = "2", features = ["full"] }

[dev-dependencies]
trybuild = "1"
TOML
cat > src/main.rs <<'RS'
fn main() {
    println!("OK");
}
RS

iterations=0
failed_builds=0
exec_failures=0
signature_hits=0
failures_json=""
end=$(( $(date +%s) + MINUTES * 60 ))
N=6
churn=0

json_escape() {
  python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()[:2048]))'
}

add_failure() { # kind index file
  local snippet
  snippet=$(json_escape < "$3")
  failures_json="${failures_json}${failures_json:+,}{\"kind\":\"$1\",\"target\":$2,\"iteration\":$iterations,\"stderr_head\":$snippet}"
}

while :; do
  iterations=$((iterations + 1))
  pids=()
  for i in $(seq 1 $N); do
    CARGO_TARGET_DIR="$DIR/t$i" cargo build >"$LOGS/build$i.log" 2>&1 &
    pids+=($!)
  done
  for i in $(seq 1 $N); do
    if ! wait "${pids[$((i - 1))]}"; then
      failed_builds=$((failed_builds + 1))
      add_failure build "$i" "$LOGS/build$i.log"
    fi
    hits=$(grep -cE "$SIG" "$LOGS/build$i.log" || true)
    signature_hits=$((signature_hits + hits))
  done

  for i in $(seq 1 $N); do
    bin="$DIR/t$i/debug/probe"
    out=$("$bin" 2>"$LOGS/exec$i.log")
    rc=$?
    if [ "$rc" -ne 0 ] || [ "$out" != "OK" ]; then
      exec_failures=$((exec_failures + 1))
      { echo "exit=$rc stdout=$out"; cat "$LOGS/exec$i.log"; } > "$LOGS/exec$i.fail"
      add_failure exec "$i" "$LOGS/exec$i.fail"
    fi
    hits=$(grep -cE "$SIG" "$LOGS/exec$i.log" || true)
    signature_hits=$((signature_hits + hits))
  done

  [ "$(date +%s)" -ge "$end" ] && break

  for _ in 1 2; do
    churn=$((churn % N + 1))
    rm -rf "$DIR/t$churn"
  done
done

cat > "$REPORT" <<JSON
{"iterations": $iterations, "failed_builds": $failed_builds, "exec_failures": $exec_failures, "signature_hits": $signature_hits, "failures": [${failures_json}]}
JSON
echo "cargo_load done: iterations=$iterations failed_builds=$failed_builds exec_failures=$exec_failures signature_hits=$signature_hits"
