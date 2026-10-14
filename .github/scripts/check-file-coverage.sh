#!/usr/bin/env bash
set -euo pipefail

minimum="${1:-90}"
report="${2:-coverage.json}"
workspace_root="$(pwd -P)/"
# Every crate lives under `crates/<package>/src/`, so one prefix covers the
# whole workspace. Vendored submodules and `worktrees/` sit outside it and are
# excluded by the same test.
source_root="${workspace_root}crates/"
# The linker forked from mold (docs/adr/0009-fork-mold-into-a-stateful-linker.md)
# is tested by mold's shell suite, which drives real compilers and linkers and
# runs in its own CI job, not by unit tests; its sources are kept as close to
# upstream as possible, so they are held to that suite rather than to this
# gate. New tinymold code for the linker lives in crates that are not listed
# here and must meet the gate like any other.
forked='^crates/tinymold-link(-common|-elf|-elf-x86_64|-elf-arm64|-macho|-macho-arm64|-macho-x86_64|-tests)?/'

cargo llvm-cov \
  --locked \
  --workspace \
  --all-targets \
  --all-features \
  --json \
  --output-path "$report"

covered_files="$(jq --arg workspace_root "$workspace_root" --arg source_root "$source_root" --arg forked "$forked" '
  [
    .data[].files[]
    | select(.filename | startswith($source_root))
  | select(.filename | ltrimstr($workspace_root) | test($forked) | not)
    | select(.summary.lines.count > 0)
  ]
  | length
' "$report")"

if [[ "$covered_files" -eq 0 ]]; then
  echo "coverage report contains no files with executable lines under crates/" >&2
  exit 1
fi

summary="$(jq -r --arg workspace_root "$workspace_root" --arg source_root "$source_root" --arg forked "$forked" '
  .data[].files[]
  | select(.filename | startswith($source_root))
  | select(.filename | ltrimstr($workspace_root) | test($forked) | not)
  | select(.summary.lines.count > 0)
  | [
      (.filename | ltrimstr($workspace_root)),
      (.summary.lines.percent | tostring),
      (.summary.lines.covered | tostring),
      (.summary.lines.count | tostring)
    ]
  | @tsv
' "$report")"

printf 'File\tLine coverage\tCovered lines\tCoverable lines\n'
while IFS=$'\t' read -r file percent covered count; do
  printf '%s\t%.2f%%\t%s\t%s\n' "$file" "$percent" "$covered" "$count"
done <<< "$summary"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    printf '### Per-file line coverage\n\n'
    printf '| File | Coverage | Lines |\n'
    printf '| --- | ---: | ---: |\n'
    while IFS=$'\t' read -r file percent covered count; do
      printf '| %s | %.2f%% | %s/%s |\n' "$file" "$percent" "$covered" "$count"
    done <<< "$summary"
  } >> "$GITHUB_STEP_SUMMARY"
fi

failures="$(jq -r \
  --arg workspace_root "$workspace_root" \
  --arg source_root "$source_root" \
  --arg forked "$forked" \
  --argjson minimum "$minimum" '
    .data[].files[]
    | select(.filename | startswith($source_root))
  | select(.filename | ltrimstr($workspace_root) | test($forked) | not)
    | select(.summary.lines.count > 0)
    | select(.summary.lines.percent < $minimum)
    | "\(.filename | ltrimstr($workspace_root)): \(.summary.lines.percent)%"
  ' "$report")"

if [[ -n "$failures" ]]; then
  printf '\nFiles below %s%% line coverage:\n%s\n' "$minimum" "$failures" >&2
  exit 1
fi
