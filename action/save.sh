#!/usr/bin/env bash
# The save half of the tinymold GitHub Action (../save/action.yml): push new
# entries to the remote, summarize the hit rate, and trim the store before
# actions/cache saves it. It also names the job's cache entry and trims the
# cargo registry before that is saved.
set -euo pipefail

# The SHA-256 of standard input, with whichever tool the runner has.
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum; else shasum -a 256; fi
}

# Print the name of every crate archive the lockfiles given as arguments
# still use, as cargo names it in registry/cache: `<name>-<version>.crate`.
locked_crates() {
  awk '
    /^\[\[package\]\]/ { name = ""; next }
    /^name = /    { name = $3; gsub(/"/, "", name); next }
    /^version = / { v = $3; gsub(/"/, "", v); if (name != "") print name "-" v ".crate" }
  ' "$@" | LC_ALL=C sort -u
}

# Delete crate archives no lockfile in the workspace uses any more, so the
# registry entry does not grow with every Cargo.lock it was restored across.
# Without a lockfile there is nothing to judge by, and nothing is deleted.
prune_registry() {
  local cache="$1/registry/cache" keep lockfiles=() dir
  [[ -d "${cache}" ]] || return 0
  while IFS= read -r -d '' lock; do lockfiles+=("${lock}"); done < <(
    find "${GITHUB_WORKSPACE:-.}" -name Cargo.lock -not -path '*/target/*' -print0
  )
  [[ "${#lockfiles[@]}" -gt 0 ]] || return 0
  keep="$(locked_crates "${lockfiles[@]}")"
  for dir in "${cache}"/*/; do
    [[ -d "${dir}" ]] || continue
    (cd "${dir}" && find . -maxdepth 1 -name '*.crate' -type f | sed 's|^\./||' | LC_ALL=C sort | LC_ALL=C comm -23 - <(printf '%s\n' "${keep}") |
      while IFS= read -r stale; do rm -f -- "${stale}"; done)
  done
}

# Every file under the directory given that this job wrote, sorted.
newer() {
  (cd "$1" && find . -type f -newer "${TINYMOLD_START_MARKER}" | LC_ALL=C sort)
}

backend="${TINYMOLD_BACKEND:-}"
[[ -n "${backend}" ]] || {
  echo "::warning title=tinymold::the setup step did not run; nothing to save"
  printf 'save-cache=false\nsave-registry=false\n' >> "${GITHUB_OUTPUT}"
  exit 0
}

# Stop the cache server first: its shutdown collects garbage to its budget,
# so what is saved below is already trimmed.
stop_server() {
  local pid
  pid="$(cat "${TINYMOLD_SERVE_PID_FILE:-/nonexistent}" 2>/dev/null || true)"
  [[ -n "${pid}" ]] || return 0
  kill -TERM "${pid}" 2>/dev/null || return 0
  for _ in $(seq 120); do
    kill -0 "${pid}" 2>/dev/null || return 0
    sleep 0.5
  done
  echo "::warning title=tinymold::tinymold serve did not stop within a minute; killing it"
  kill -KILL "${pid}" 2>/dev/null || true
}
serving=false
if [[ "${TINYMOLD_SERVE:-}" == true && -n "${TINYMOLD_SERVER_DIR:-}" ]]; then
  serving=true
  stop_server
fi

push="${INPUT_PUSH}"
if [[ "${push}" == auto ]]; then
  push=false
  [[ -n "${TINYMOLD_REMOTE_AUTH:-}" ]] && push=true
fi
if [[ "${push}" == true ]]; then
  if [[ "${backend}" == remote || "${backend}" == both ]]; then
    tinymold remote push --since "${TINYMOLD_SAVE_START}"
  else
    echo "::warning title=tinymold::push is set, but backend '${backend}' has no remote"
  fi
fi

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  tinymold stats --markdown >> "${GITHUB_STEP_SUMMARY}" || true
fi
tinymold stats || true

save=false
# Every backend gets gc: a self-hosted runner keeps its store between jobs
# whichever backend it uses, and this runs after the push.
gc_args=(--max-age "${INPUT_MAX_AGE}" --max-size "${INPUT_MAX_SIZE}")
# A store restored from GitHub's cache belongs to this job alone, and every
# object it stored is younger than gc's grace period (which guards against a
# concurrent build). Without --grace 0s, --max-size would drop the actions
# but keep their objects, and save a store far over budget. A remote or
# persistent store may be shared by jobs still running, so it keeps the grace.
if [[ "${backend}" == github || "${backend}" == both ]]; then
  gc_args+=(--grace 0s)
fi
tinymold gc "${gc_args[@]}"
if [[ "${INPUT_SAVE_CACHE}" == true ]] && [[ "${backend}" == github || "${backend}" == both ]]; then
  # A job that stored nothing new would only upload the store it restored.
  if grep -q " stored in " "${TINYMOLD_LOG}" 2>/dev/null; then
    save=true
  elif [[ "${serving}" == true ]] && [[ -n "$(newer "${TINYMOLD_SERVER_DIR}" | head -n 1)" ]]; then
    save=true
  else
    echo "tinymold: nothing new was stored; not saving the cache"
  fi
fi

# Every job of a run shares the setup step's key, and GitHub keeps only the
# first entry saved under a key. The job id and what this job wrote set the
# entry apart: matrix legs share a job id, but not their new entries.
key=""
if [[ "${save}" == true ]]; then
  stored="$({
    newer "${TINYMOLD_STORE}"
    if [[ "${serving}" == true ]]; then
      newer "${TINYMOLD_SERVER_DIR}" | sed 's|^|server:|'
    fi
  } | sha256 | cut -c1-12)"
  key="${TINYMOLD_CACHE_KEY}-${GITHUB_JOB:-job}-${stored}"
fi

# The registry entry is keyed by Cargo.lock alone, so it is saved only when no
# entry for this lockfile was restored. Jobs that race to save it would save
# the same crates.
save_registry=false
if [[ "${INPUT_SAVE_CACHE}" == true && -n "${TINYMOLD_REGISTRY_KEY:-}" &&
  "${TINYMOLD_REGISTRY_HIT:-}" != true && -d "${TINYMOLD_CARGO_HOME:-}/registry" ]]; then
  prune_registry "${TINYMOLD_CARGO_HOME}"
  save_registry=true
fi

{
  echo "save-cache=${save}"
  echo "key=${key}"
  echo "save-registry=${save_registry}"
} >> "${GITHUB_OUTPUT}"
