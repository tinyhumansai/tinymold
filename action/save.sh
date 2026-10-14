#!/usr/bin/env bash
# The save half of the tinymold GitHub Action (../save/action.yml): push new
# entries to the remote, summarize the hit rate, and trim the store before
# actions/cache saves it.
set -euo pipefail

backend="${TINYMOLD_BACKEND:-}"
[[ -n "${backend}" ]] || {
  echo "::warning title=tinymold::the setup step did not run; nothing to save"
  echo "save-cache=false" >> "${GITHUB_OUTPUT}"
  exit 0
}

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
tinymold gc --max-age "${INPUT_MAX_AGE}" --max-size "${INPUT_MAX_SIZE}"
if [[ "${INPUT_SAVE_CACHE}" == true ]] && [[ "${backend}" == github || "${backend}" == both ]]; then
  # A job that stored nothing new would only upload the store it restored.
  if grep -q " stored in " "${TINYMOLD_LOG}" 2>/dev/null; then
    save=true
  else
    echo "tinymold: nothing new was stored; not saving the cache"
  fi
fi
echo "save-cache=${save}" >> "${GITHUB_OUTPUT}"
