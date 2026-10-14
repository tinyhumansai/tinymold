#!/usr/bin/env bash
# A/B the openhuman-core link: replays one captured link line with mold and
# with tinymold-link, alternating, N times each. Runs in the tinymold-bench
# image with the benchmark volume at /work, after docker_openhuman.py run has
# filled it. From the repository root:
#
#   docker run --rm -v tinymold-openhuman:/work -v "$PWD/bench/docker:/ab:ro" \
#       tinymold-bench bash /ab/link-ab.sh 30
set -euo pipefail
N=${1:-15}
cd /work/oh
TARGET=/work/oh/target/ab
mkdir -p "$TARGET"
# Capture the link line of the binary, keeping the objects it names. The
# touch makes cargo rerun the binary's rustc, which a fresh target skips.
touch crates/openhuman-cli/src/main.rs
RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo rustc --locked -q --target-dir "$TARGET" \
    -p openhuman-cli --bin openhuman-core -- -C save-temps --print link-args > /tmp/link-args 2>/tmp/capture.log \
    || { tail -20 /tmp/capture.log; exit 1; }
line=$(grep -m1 '"-o"' /tmp/link-args)
run() { # run <label> <extra-arg>
    local out=/tmp/ab-$1
    local l=${line//\"-fuse-ld=mold\"/}
    l=$(sed -E "s#\"-o\" \"[^\"]*\"#\"-o\" \"$out\"#" <<<"$l")
    local s=$(date +%s.%N)
    eval "$l \"$2\"" >/dev/null 2>&1
    awk -v a="$s" -v b="$(date +%s.%N)" 'BEGIN {printf "%.4f\n", b - a}'
}
for i in 1 2; do run mold -fuse-ld=mold >/dev/null; run tml -B/usr/local/libexec/tinymold-link >/dev/null; done
: > /tmp/mold.t; : > /tmp/tml.t
for i in $(seq "$N"); do
    run mold -fuse-ld=mold >> /tmp/mold.t
    run tml -B/usr/local/libexec/tinymold-link >> /tmp/tml.t
done
cmp -s <(readelf -p .comment /tmp/ab-tml | grep -c tinymold-link) <(echo 1) && echo "tinymold-link output carries its .comment" || echo "WARNING: tinymold-link not used"
for f in mold tml; do sort -n /tmp/$f.t | awk -v f=$f '{a[NR]=$1} END {printf "%s: median %.3fs min %.3fs max %.3fs n=%d\n", f, a[int((NR+1)/2)], a[1], a[NR], NR}'; done
ls -l /tmp/ab-mold /tmp/ab-tml | awk '{print $5, $9}'
