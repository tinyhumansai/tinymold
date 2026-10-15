#!/usr/bin/env bash
# The `serve: true` step of the tinymold GitHub Action (../action.yml): start
# `tinymold serve` on loopback for the rest of the job, on the server store
# the restore step just brought back, and point other tools' remote caches at
# it. The save step (save.sh) stops it.
#
# The server is open to this runner only: it listens on 127.0.0.1, so it
# needs no token file. A variable the workflow already set is left alone,
# so a job can still point a tool at a different cache.
set -euo pipefail

fail() {
  echo "::error title=tinymold::$*" >&2
  exit 1
}

port="${TINYMOLD_SERVE_PORT:-9090}"
url="http://127.0.0.1:${port}"
log="${RUNNER_TEMP}/tinymold-serve.log"
pid_file="${RUNNER_TEMP}/tinymold-serve.pid"

# Not a compile: the wrapper variable must not apply to the server. Output
# goes to a file so the step does not wait on the server's pipes.
env -u RUSTC_WRAPPER nohup tinymold serve --dir "${TINYMOLD_SERVER_DIR}" \
  --listen "127.0.0.1:${port}" --max-size "${INPUT_SERVE_MAX_SIZE:-4G}" \
  > "${log}" 2>&1 &
echo "$!" > "${pid_file}"

for _ in $(seq 50); do
  if curl -fsS -o /dev/null "${url}/healthz" 2>/dev/null; then
    break
  fi
  if ! kill -0 "$(cat "${pid_file}")" 2>/dev/null; then
    cat "${log}" >&2
    fail "tinymold serve exited during startup"
  fi
  sleep 0.2
done
curl -fsS -o /dev/null "${url}/healthz" || {
  cat "${log}" >&2
  fail "tinymold serve did not answer on ${url}"
}

team="${INPUT_TURBO_TEAM:-${GITHUB_REPOSITORY_OWNER:-tinymold}}"
set_unset() {
  if [[ -z "${!1:-}" ]]; then
    echo "$1=$2" >> "${GITHUB_ENV}"
  else
    echo "tinymold: $1 is already set; leaving it"
  fi
}
{
  echo "TINYMOLD_SERVE_URL=${url}"
  echo "TINYMOLD_SERVE_PID_FILE=${pid_file}"
  echo "TINYMOLD_SERVE_LOG=${log}"
} >> "${GITHUB_ENV}"
set_unset TURBO_API "${url}"
# Turbo enables a remote cache only with a token; the server ignores it.
set_unset TURBO_TOKEN tinymold
set_unset TURBO_TEAM "${team}"
set_unset NX_SELF_HOSTED_REMOTE_CACHE_SERVER "${url}"
set_unset CCACHE_REMOTE_STORAGE "${url}|layout=bazel"
echo "serve-url=${url}" >> "${GITHUB_OUTPUT}"
echo "tinymold: serving ${TINYMOLD_SERVER_DIR} on ${url}"
