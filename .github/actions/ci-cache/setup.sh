#!/usr/bin/env bash
# The ci-cache action's one shell step. Inputs arrive as IN_* (action.yml).
#
# Key: the read-write pair from ci-trusted when the job passed it, else the
# runner's read-only pair (CI_CACHE_RO_*, set in the runner's environment).
# Neither: a runner without the caches, so one notice and nothing exported.
# An endpoint that does not answer is treated the same way, with a warning.
#
# Writing needs trusted=true AND the read-write pair. Read mode is enforced
# here as well as by the key: SCCACHE_S3_RW_MODE=READ_ONLY,
# runs-on/cache/restore, no --cache-to.
set -euo pipefail

out() { printf '%s=%s\n' "$1" "$2" >>"$GITHUB_OUTPUT"; }
env_set() { printf '%s=%s\n' "$1" "$2" >>"$GITHUB_ENV"; }
# Mode none: nothing exported, so the downloads steps are skipped,
# ci-cache-buildx-args prints nothing and sccache keeps the local disk cache.
uncached() { echo "$1"; out mode none; exit 0; }

# Installed first, so a workflow can call it unconditionally: without a
# BuildKit cache it prints nothing.
mkdir -p "$RUNNER_TEMP/ci-cache/bin"
install -m 0755 "$GITHUB_ACTION_PATH/buildx-args.sh" "$RUNNER_TEMP/ci-cache/bin/ci-cache-buildx-args"
echo "$RUNNER_TEMP/ci-cache/bin" >>"$GITHUB_PATH"

key_id="" secret="" mode=""
if [ -n "$IN_RW_KEY_ID" ] && [ -n "$IN_RW_SECRET" ]; then
  key_id="$IN_RW_KEY_ID" secret="$IN_RW_SECRET"
  if [ "$IN_TRUSTED" = true ]; then mode="write"; else mode="read"; fi
elif [ -n "${CI_CACHE_RO_KEY_ID:-}" ] && [ -n "${CI_CACHE_RO_SECRET:-}" ]; then
  key_id="$CI_CACHE_RO_KEY_ID" secret="$CI_CACHE_RO_SECRET" mode=read
  if [ "$IN_TRUSTED" = true ]; then
    echo "::warning::ci-cache: trusted=true but no read-write key (is the job under ci-trusted?); caches are read only"
  fi
else
  uncached "::notice::ci-cache: no cache key on this runner; running without the S3 caches"
fi
echo "::add-mask::$key_id"
echo "::add-mask::$secret"

endpoint="${IN_ENDPOINT:-${CI_CACHE_ENDPOINT:-}}"
region="${IN_REGION:-${CI_CACHE_REGION:-}}"
project="${IN_PROJECT:-${CI_CACHE_PROJECT:-}}"
if [ -z "$project" ]; then
  case "${GITHUB_REPOSITORY#*/}" in
    insight) project=insight ;;
    gears-rust) project=gears ;;
    studio-web) project=studio ;;
  esac
fi
for v in endpoint region project; do
  if [ -z "${!v}" ]; then
    echo "::error::ci-cache: no $v (input, or CI_CACHE_$(tr '[:lower:]' '[:upper:]' <<<"$v") on the runner)"
    exit 1
  fi
done
endpoint="${endpoint%/}"
prefix="$IN_PREFIX"

# Composite steps have no timeout, and a hung S3 connection would stall the
# downloads and BuildKit steps. Any HTTP answer (403 and 404 included) means
# reachable; a connect error or timeout means build uncached.
if ! curl -sS -o /dev/null --connect-timeout 3 --max-time 5 "$endpoint" 2>/dev/null; then
  uncached "::warning::ci-cache: S3 endpoint unreachable; building without the shared caches"
fi

out mode "$mode"
out endpoint "$endpoint"
echo "ci-cache: project $project, $mode${prefix:+, prefix $prefix}"

# Every consumer (sccache, runs-on/cache, buildx) takes the standard AWS_*
# variables; buildx forwards them into a docker-container builder itself.
env_set AWS_ACCESS_KEY_ID "$key_id"
env_set AWS_SECRET_ACCESS_KEY "$secret"
env_set AWS_REGION "$region"
export AWS_ACCESS_KEY_ID="$key_id" AWS_SECRET_ACCESS_KEY="$secret" AWS_REGION="$region"

# --- Rust: sccache -----------------------------------------------------------

if [ "$IN_RUST" = true ]; then
  if ! command -v sccache >/dev/null; then
    echo "::warning::ci-cache: rust=true but sccache is not installed; skipped"
  else
    # The post step (stats/post.js) runs this exact binary, not a PATH lookup.
    env_set CI_CACHE_SCCACHE "$(command -v sccache)"
    rw=READ_WRITE
    [ "$mode" = write ] || rw=READ_ONLY
    sc=(
      SCCACHE_BUCKET="sccache-$project"
      SCCACHE_ENDPOINT="$endpoint"
      SCCACHE_REGION="$region"
      SCCACHE_S3_USE_SSL=true
      SCCACHE_S3_RW_MODE="$rw"
      SCCACHE_IDLE_TIMEOUT=0
    )
    [ -z "$prefix" ] || sc+=(SCCACHE_S3_KEY_PREFIX="$prefix")
    # sccache refuses to start when its startup read of the bucket fails, and
    # RUSTC_WRAPPER=sccache would then fail every rustc. So start it here
    # first, and export the bucket only if that worked; otherwise fall back to
    # the local disk cache the image configures.
    sccache --stop-server >/dev/null 2>&1 || true
    if env "${sc[@]}" sccache --start-server >/dev/null 2>"$RUNNER_TEMP/sccache-start.log"; then
      for kv in "${sc[@]}"; do env_set "${kv%%=*}" "${kv#*=}"; done
      echo "ci-cache: sccache on sccache-$project ($rw)"
    else
      echo "::warning::ci-cache: sccache could not use S3 ($(head -c 300 "$RUNNER_TEMP/sccache-start.log" | tr '\n' ' ')); local cache only"
      sccache --start-server >/dev/null 2>&1 || true
    fi
    env_set RUSTC_WRAPPER sccache
    env_set CARGO_INCREMENTAL 0
    out rust true
  fi
fi

# --- downloads: runs-on/cache ------------------------------------------------

if [ -n "$IN_DOWNLOADS" ]; then
  if [ -z "$IN_DOWNLOADS_KEY" ]; then
    echo "::error::ci-cache: downloads needs downloads-key"
    exit 1
  fi
  out downloads "$mode"
  out downloads-bucket "actions-cache-$project"
  # The object key is cache/<owner>/<repo>/<version>/<key> (runs-on/cache
  # src/custom/backend.ts), so the prefix goes into the key.
  {
    echo "downloads-restore-keys<<CI_CACHE_EOF"
    printf '%s\n' "${IN_DOWNLOADS_RESTORE_KEYS:-}" | sed '/^$/d' | sed "s|^|$prefix|"
    echo "CI_CACHE_EOF"
  } >>"$GITHUB_OUTPUT"
fi

# --- images: BuildKit's S3 cache ---------------------------------------------

if [ "$IN_BUILDKIT" = true ]; then
  if ! docker buildx version >/dev/null 2>&1; then
    echo "::warning::ci-cache: buildkit=true but docker buildx is missing; skipped"
  else
    # The S3 cache needs the docker-container driver. A buildkitd.toml on the
    # runner is passed on, so the builder keeps the runner's registry
    # settings; buildx copies the CA files it names into the builder.
    if [ "$(docker buildx inspect 2>/dev/null | awk '/^Driver:/ {print $2; exit}')" != docker-container ]; then
      args=(--name ci-cache --driver docker-container --use
            --driver-opt "image=moby/buildkit:v0.34.1@sha256:99368f3e46177c6405b9740e4461d91cee004128daeecca78d4ceac50ec1da5c")
      [ ! -r /etc/buildkit/buildkitd.toml ] || args+=(--buildkitd-config /etc/buildkit/buildkitd.toml)
      docker buildx create "${args[@]}" >/dev/null
      docker buildx inspect --bootstrap >/dev/null
    fi
    env_set CI_CACHE_BUILDKIT "type=s3,endpoint_url=$endpoint,region=$region,bucket=buildcache-$project,use_path_style=true${prefix:+,prefix=$prefix}"
    env_set CI_CACHE_BUILDKIT_WRITE "$([ "$mode" = write ] && echo true || echo false)"
    echo "ci-cache: BuildKit cache buildcache-$project ($mode)"
  fi
fi
