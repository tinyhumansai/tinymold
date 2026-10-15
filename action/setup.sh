#!/usr/bin/env bash
# The setup half of the tinymold GitHub Action (../action.yml): install the
# binary, point cargo at it, and tell the restore and save steps where the
# store is and what to call its cache entry.
#
# Inputs arrive as INPUT_* variables; everything later steps need is written
# to $GITHUB_ENV, $GITHUB_PATH and $GITHUB_OUTPUT.
set -euo pipefail

fail() {
  echo "::error title=tinymold::$*" >&2
  exit 1
}

case "${INPUT_BACKEND}" in
  github | remote | both | none) ;;
  *) fail "backend must be github, remote, both or none, not '${INPUT_BACKEND}'" ;;
esac
if [[ "${INPUT_BACKEND}" == remote || "${INPUT_BACKEND}" == both ]] && [[ -z "${INPUT_REMOTE_URL}" ]]; then
  fail "backend '${INPUT_BACKEND}' needs remote-url"
fi

bin_dir="${RUNNER_TEMP}/tinymold-bin"
mkdir -p "${bin_dir}"
exe=""
[[ "${RUNNER_OS}" == Windows ]] && exe=".exe"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

case "${INPUT_INSTALL}" in
  release)
    case "${RUNNER_OS}-${RUNNER_ARCH}" in
      Linux-X64) target=x86_64-unknown-linux-gnu archive=tar.gz ;;
      Linux-ARM64) target=aarch64-unknown-linux-gnu archive=tar.gz ;;
      macOS-X64) target=x86_64-apple-darwin archive=tar.gz ;;
      macOS-ARM64) target=aarch64-apple-darwin archive=tar.gz ;;
      Windows-X64) target=x86_64-pc-windows-msvc archive=zip ;;
      Windows-ARM64) target=aarch64-pc-windows-msvc archive=zip ;;
      *) fail "no tinymold release for ${RUNNER_OS} ${RUNNER_ARCH}; use install: source" ;;
    esac
    tag="${INPUT_VERSION}"
    if [[ "${tag}" == latest ]]; then
      # The latest release's URL redirects to its tag.
      tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/${ACTION_REPOSITORY}/releases/latest")"
      tag="${tag##*/}"
      [[ "${tag}" == v* ]] || fail "no tinymold release found; use install: source"
    fi
    name="tinymold-${tag#v}-${target}"
    download="${RUNNER_TEMP}/tinymold-download"
    mkdir -p "${download}"
    base="https://github.com/${ACTION_REPOSITORY}/releases/download/${tag}"
    curl -fsSL -o "${download}/${name}.${archive}" "${base}/${name}.${archive}"
    curl -fsSL -o "${download}/SHA256SUMS" "${base}/SHA256SUMS"
    (cd "${download}" && grep " ${name}.${archive}\$" SHA256SUMS | sha256 -c -) ||
      fail "${name}.${archive} does not match SHA256SUMS"
    if [[ "${archive}" == zip ]]; then
      unzip -q -o "${download}/${name}.zip" -d "${download}"
    else
      tar -xzf "${download}/${name}.tar.gz" -C "${download}"
    fi
    cp "${download}/${name}/tinymold${exe}" "${bin_dir}/"
    ;;
  source)
    # Built without the wrapper, which is not set up yet.
    env -u RUSTC_WRAPPER cargo install --locked --quiet \
      --path "${INPUT_SOURCE_PATH}/crates/tinymold" --root "${RUNNER_TEMP}/tinymold-install"
    cp "${RUNNER_TEMP}/tinymold-install/bin/tinymold${exe}" "${bin_dir}/"
    ;;
  none)
    command -v tinymold >/dev/null || fail "install: none, but tinymold is not on PATH"
    cp "$(command -v tinymold)" "${bin_dir}/tinymold${exe}"
    ;;
  *) fail "install must be release, source or none, not '${INPUT_INSTALL}'" ;;
esac
echo "${bin_dir}" >> "${GITHUB_PATH}"
binary="${bin_dir}/tinymold${exe}"
"${binary}" --version

store="${INPUT_STORE:-${HOME}/.cache/tinymold}"
mkdir -p "${store}"
# Anything in the store newer than this file was written by this job. The
# restore that follows keeps the archive's mtimes, which are older.
start_marker="${RUNNER_TEMP}/tinymold-start"
touch "${start_marker}"

{
  echo "RUSTC_WRAPPER=${binary}"
  echo "TINYMOLD_STORE=${store}"
  echo "TINYMOLD_LOG=${RUNNER_TEMP}/tinymold.log"
  echo "TINYMOLD_BACKEND=${INPUT_BACKEND}"
  echo "TINYMOLD_SAVE_START=$(date +%s)"
  echo "TINYMOLD_START_MARKER=${start_marker}"
  # Incremental artifacts only help the job that wrote them.
  echo "CARGO_INCREMENTAL=0"
} >> "${GITHUB_ENV}"
if [[ "${INPUT_BACKEND}" == remote || "${INPUT_BACKEND}" == both ]]; then
  echo "TINYMOLD_REMOTE_URL=${INPUT_REMOTE_URL}" >> "${GITHUB_ENV}"
  [[ -n "${INPUT_REMOTE_INSTANCE}" ]] && echo "TINYMOLD_REMOTE_INSTANCE=${INPUT_REMOTE_INSTANCE}" >> "${GITHUB_ENV}"
  if [[ -n "${INPUT_REMOTE_AUTH}" ]]; then
    echo "::add-mask::${INPUT_REMOTE_AUTH}"
    echo "TINYMOLD_REMOTE_AUTH=${INPUT_REMOTE_AUTH}" >> "${GITHUB_ENV}"
  fi
fi

# The cache key: the store format, the platform, the caller's text and the
# compiler. The lockfile hash, run and attempt are appended in action.yml, and
# the job and what it stored in save.sh.
rustc_hash="$(rustc -vV | sha256 | cut -c1-16)"
prefix="tinymold-v1-${RUNNER_OS}-${RUNNER_ARCH}"
[[ -n "${INPUT_CACHE_KEY}" ]] && prefix="${prefix}-${INPUT_CACHE_KEY}"
{
  echo "store=${store}"
  echo "key-prefix=${prefix}-${rustc_hash}"
  echo "registry-key-prefix=tinymold-cargo-v1-${RUNNER_OS}-${RUNNER_ARCH}"
  echo "cargo-home=${CARGO_HOME:-${HOME}/.cargo}"
} >> "${GITHUB_OUTPUT}"
