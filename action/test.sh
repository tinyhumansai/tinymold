#!/usr/bin/env bash
# Tests for the save half of the GitHub Action (save.sh): how it names a
# job's cache entry and which crate archives it keeps in the cargo registry.
# A stub stands in for the tinymold binary, so this needs only bash and the
# tools save.sh itself uses. Run it from anywhere: `bash action/test.sh`.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failures=0
roots=()
cleanup() { [[ "${#roots[@]}" -eq 0 ]] || rm -rf "${roots[@]}"; }
trap cleanup EXIT

fail() {
  echo "FAIL ${current}: $*" >&2
  failures=$((failures + 1))
}

# A fresh runner: a stub tinymold, an empty store with its start marker, a
# workspace with one lockfile and a cargo home with an empty registry.
new_runner() {
  current="$1"
  root="$(mktemp -d)"
  roots+=("${root}")
  mkdir -p "${root}/bin" "${root}/store" "${root}/workspace" "${root}/cargo/registry/cache/index.crates.io-0"
  printf '#!/usr/bin/env bash\nexit 0\n' > "${root}/bin/tinymold"
  chmod +x "${root}/bin/tinymold"
  cat > "${root}/workspace/Cargo.lock" <<'LOCK'
version = 4

[[package]]
name = "demo"
version = "0.1.0"
dependencies = [
 "serde",
]

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
LOCK
  # Older than the marker: what the restore step unpacked.
  touch -t 202001010000 "${root}/store/restored"
  touch "${root}/marker"
  : > "${root}/log"
  : > "${root}/output"
}

# Record that this job stored the store files named as arguments.
store() {
  sleep 1 # -newer compares whole seconds on some filesystems.
  local file
  for file in "$@"; do echo "${file}" > "${root}/store/${file}"; done
  echo "miss demo 900.0ms stored in 1.0ms" >> "${root}/log"
}

run_save() {
  (
    cd "${root}/workspace"
    PATH="${root}/bin:${PATH}" \
      GITHUB_OUTPUT="${root}/output" GITHUB_WORKSPACE="${root}/workspace" GITHUB_JOB=test \
      TINYMOLD_BACKEND=github TINYMOLD_STORE="${root}/store" TINYMOLD_LOG="${root}/log" \
      TINYMOLD_START_MARKER="${root}/marker" TINYMOLD_SAVE_START=0 \
      TINYMOLD_CACHE_KEY=tinymold-v1-Linux-X64-rustc-lock-1-1 \
      INPUT_PUSH=false INPUT_MAX_AGE=48h INPUT_MAX_SIZE=4G INPUT_SAVE_CACHE=true \
      "$@" bash "${here}/save.sh" > /dev/null
  )
}

output() {
  sed -n "s/^$1=//p" "${root}/output" | tail -n 1
}

# Two legs of one matrix job share a job id and the setup key; GitHub keeps
# only the first entry saved under a key, so the second leg's units were lost.
new_runner "matrix legs that store different units save under different keys"
store entry-a
run_save env
first="$(output key)"
new_runner "${current}"
store entry-b
run_save env
second="$(output key)"
[[ -n "${first}" && -n "${second}" ]] || fail "no key was named ('${first}', '${second}')"
[[ "${first}" != "${second}" ]] || fail "both legs saved under ${first}"
[[ "${first}" == tinymold-v1-Linux-X64-rustc-lock-1-1-test-* ]] ||
  fail "key '${first}' does not extend the setup key with the job"

new_runner "a job that stores the same units names the same key"
store entry-a
run_save env
again="$(output key)"
[[ "${again}" == "${first}" ]] || fail "'${again}' differs from '${first}'"

new_runner "a job that stored nothing saves no store"
run_save env
[[ "$(output save-cache)" == false ]] || fail "save-cache is $(output save-cache)"
[[ -z "$(output key)" ]] || fail "named key '$(output key)'"

new_runner "the registry drops crates no lockfile uses before saving"
cache="${root}/cargo/registry/cache/index.crates.io-0"
touch "${cache}/serde-1.0.200.crate" "${cache}/serde-1.0.199.crate" "${cache}/notes.txt"
run_save env TINYMOLD_REGISTRY_KEY=tinymold-cargo-v1-Linux-X64-lock TINYMOLD_CARGO_HOME="${root}/cargo"
[[ "$(output save-registry)" == true ]] || fail "save-registry is $(output save-registry)"
[[ -f "${cache}/serde-1.0.200.crate" ]] || fail "deleted the locked serde-1.0.200.crate"
[[ ! -e "${cache}/serde-1.0.199.crate" ]] || fail "kept the unlocked serde-1.0.199.crate"
[[ -f "${cache}/notes.txt" ]] || fail "deleted a file that is not a crate archive"

new_runner "without a lockfile the registry keeps every crate"
rm "${root}/workspace/Cargo.lock"
cache="${root}/cargo/registry/cache/index.crates.io-0"
touch "${cache}/serde-1.0.199.crate"
run_save env TINYMOLD_REGISTRY_KEY=tinymold-cargo-v1-Linux-X64-lock TINYMOLD_CARGO_HOME="${root}/cargo"
[[ -f "${cache}/serde-1.0.199.crate" ]] || fail "deleted serde-1.0.199.crate"

new_runner "a registry restored for this exact lockfile is not saved again"
run_save env TINYMOLD_REGISTRY_KEY=tinymold-cargo-v1-Linux-X64-lock TINYMOLD_REGISTRY_HIT=true \
  TINYMOLD_CARGO_HOME="${root}/cargo"
[[ "$(output save-registry)" == false ]] || fail "save-registry is $(output save-registry)"

new_runner "the registry is not saved when save-cache is off"
run_save env TINYMOLD_REGISTRY_KEY=tinymold-cargo-v1-Linux-X64-lock TINYMOLD_CARGO_HOME="${root}/cargo" \
  INPUT_SAVE_CACHE=false
[[ "$(output save-registry)" == false ]] || fail "save-registry is $(output save-registry)"

# With `serve: true`, a server store beside the wrapper store.
serve_runner() {
  new_runner "$1"
  mkdir -p "${root}/server"
  touch -t 202001010000 "${root}/server/restored"
}

new_runner "a job that serves nothing keeps the key it had before"
store entry-a
run_save env
plain="$(output key)"
serve_runner "${current}"
store entry-a
run_save env TINYMOLD_SERVE=false TINYMOLD_SERVER_DIR="${root}/server"
[[ "$(output key)" == "${plain}" ]] || fail "'$(output key)' differs from '${plain}'"

serve_runner "a job whose server stored entries saves the cache"
sleep 1
echo entry > "${root}/server/written"
run_save env TINYMOLD_SERVE=true TINYMOLD_SERVER_DIR="${root}/server"
[[ "$(output save-cache)" == true ]] || fail "save-cache is $(output save-cache)"
served="$(output key)"
[[ -n "${served}" ]] || fail "no key was named"

serve_runner "server entries set the key apart"
sleep 1
echo other > "${root}/server/written-elsewhere"
run_save env TINYMOLD_SERVE=true TINYMOLD_SERVER_DIR="${root}/server"
[[ "$(output key)" != "${served}" ]] || fail "different server entries share ${served}"

serve_runner "a server that stored nothing saves nothing"
run_save env TINYMOLD_SERVE=true TINYMOLD_SERVER_DIR="${root}/server"
[[ "$(output save-cache)" == false ]] || fail "save-cache is $(output save-cache)"

serve_runner "the server is stopped before the cache is saved"
sleep 300 &
server_pid=$!
echo "${server_pid}" > "${root}/serve.pid"
run_save env TINYMOLD_SERVE=true TINYMOLD_SERVER_DIR="${root}/server" \
  TINYMOLD_SERVE_PID_FILE="${root}/serve.pid"
sleep 0.2
if kill -0 "${server_pid}" 2>/dev/null; then
  fail "the server process is still running"
  kill "${server_pid}"
fi
wait "${server_pid}" 2>/dev/null || true

if [[ "${failures}" -gt 0 ]]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi
echo "action/test.sh: all passed"
