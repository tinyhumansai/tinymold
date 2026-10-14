#!/usr/bin/env bash
# Builds OpenHuman's `openhuman-core` binary under several configurations and
# times the build states a developer or CI runner meets. Runs inside the
# tinymold-bench image; bench/docker_openhuman.py starts it and reads its
# output.
#
# Configurations:
#   ld        plain cargo, the platform's default linker (GNU ld via cc)
#   mold      plain cargo, linked with mold
#   tinymold  RUSTC_WRAPPER=tinymold, linked with mold
#   tinymold-link  plain cargo, linked with tinymold-link (the mold fork in
#             crates/tinymold-link), through gcc's -B and an `ld` symlink
#   tinymold-full  RUSTC_WRAPPER=tinymold, linked with tinymold-link: no mold
#             anywhere. It keeps its own store, apart from tinymold's.
#   sccache   RUSTC_WRAPPER=sccache (local disk cache), linked with mold
#   sccache-noinc  the same with CARGO_INCREMENTAL=0, so sccache can cache the
#             workspace crates it otherwise passes through, at the cost of
#             rustc's incremental reuse in the edit loop
#
# Cases, in order, per configuration:
#   cold         clean target/, and for tinymold an empty store
#   noop         nothing changed
#   edit_leaf    comment appended to the binary's main.rs
#   edit_core    comment appended to the core library's lib.rs
#   warm         clean target/ again, with the cache from the builds above
#   second_path  a copy of the sources at another path, clean target/
#   snapshot     that copy again, starting from `tinymold snapshot restore`
#
# ld, mold and tinymold-link have no cache, so their warm and second-path
# builds are cold builds; they run only `cold`, `noop`, and the edits. The
# sccache configs run every case but `snapshot`, which the tinymold configs
# alone run.
#
# Every build appends one JSON object per line to $OUT/results.jsonl and keeps
# cargo's --timings report as $OUT/timings/<config>-<case>-<run>.html.

set -euo pipefail

SRC=${SRC:-/work/oh}
SECOND=${SECOND:-/work/oh-second}
STORE=${STORE:-/work/store}
OUT=${OUT:-/out}
RUNS=${RUNS:-3}
CONFIGS=${CONFIGS:-ld mold tinymold sccache sccache-noinc tinymold-link tinymold-full}
# What gcc's -B points at: a directory whose `ld` is tinymold-link.
TML_DIR=/usr/local/libexec/tinymold-link
# ONLY limits a run to some cases (space-separated), appending to the results
# of an earlier run instead of starting them over. The cases it skips must
# have left their state behind: `snapshot` needs the store, the saved snapshot
# and the second checkout.
ONLY=${ONLY:-}
JOBS=${JOBS:-$(nproc)}
PACKAGE=(-p openhuman-cli --bin openhuman-core)
LEAF=crates/openhuman-cli/src/main.rs
CORE=crates/openhuman-core/src/lib.rs
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
TRACER=/opt/bench/link-tracer

mkdir -p "$OUT/timings" "$OUT/logs"
# APPEND keeps the results already there: docker_openhuman.py seeds them with
# the committed samples of the configurations a partial run leaves out.
[[ -n $ONLY || -n ${APPEND:-} ]] || : > "$OUT/results.jsonl"
want() { [[ -z $ONLY || " $ONLY " == *" $1 "* ]]; }

# Times every link and logs it, like bench/profile_build.py's link tracer.
cat > "$TRACER" <<'PERL'
#!/usr/bin/env perl
use strict;
use Time::HiRes qw(time);
my $start = time;
my $status = system('cc', @ARGV);
my $end = time;
if (my $log = $ENV{BENCH_LINK_LOG}) {
    open(my $fh, '>>', $log) or die "cannot append to $log: $!";
    printf $fh "%.6f %.6f\n", $start, $end;
    close($fh);
}
exit($status == -1 ? 127 : $status >> 8);
PERL
chmod +x "$TRACER"

now() { date +%s.%N; }

# The tinymold configs, which run every case, and the store each one uses.
is_tinymold() { [[ $1 == tinymold || $1 == tinymold-full ]]; }
store_of() { if [[ $1 == tinymold ]]; then echo "$STORE"; else echo "$STORE-$1"; fi; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN {printf "%.4f", b - a}'; }

# build <config> <case> <run> <dir>
build() {
    local config=$1 case=$2 run=$3 dir=$4
    local target="$dir/target/$config" links="$OUT/logs/links-$config-$case-$run.log"
    local cachelog="$OUT/logs/tinymold-$config-$case-$run.log"
    local env=(CARGO_TERM_COLOR=never BENCH_LINK_LOG="$links")
    case $config in
        ld) env+=(RUSTFLAGS="") ;;
        mold) env+=(RUSTFLAGS="-C link-arg=-fuse-ld=mold") ;;
        tinymold) env+=(RUSTFLAGS="-C link-arg=-fuse-ld=mold" RUSTC_WRAPPER=/usr/local/bin/tinymold
                        TINYMOLD_STORE="$STORE" TINYMOLD_LOG="$cachelog") ;;
        tinymold-link) env+=(RUSTFLAGS="-C link-arg=-B$TML_DIR") ;;
        tinymold-full) env+=(RUSTFLAGS="-C link-arg=-B$TML_DIR" RUSTC_WRAPPER=/usr/local/bin/tinymold
                             TINYMOLD_STORE="$(store_of "$config")" TINYMOLD_LOG="$cachelog") ;;
        sccache) env+=(RUSTFLAGS="-C link-arg=-fuse-ld=mold" RUSTC_WRAPPER=/usr/local/bin/sccache)
                 sccache --zero-stats > /dev/null ;;
        sccache-noinc) env+=(RUSTFLAGS="-C link-arg=-fuse-ld=mold" RUSTC_WRAPPER=/usr/local/bin/sccache
                             CARGO_INCREMENTAL=0)
                       sccache --zero-stats > /dev/null ;;
    esac
    rm -f "$links" "$cachelog"
    rm -rf "$target/cargo-timings"
    local start wall
    start=$(now)
    if ! (cd "$dir" && env "${env[@]}" cargo build --locked -v --timings -j "$JOBS" \
            --config "target.$TRIPLE.linker=\"$TRACER\"" --target-dir "$target" "${PACKAGE[@]}") \
            > "$OUT/logs/cargo-$config-$case-$run.log" 2>&1; then
        tail -50 "$OUT/logs/cargo-$config-$case-$run.log" >&2
        echo "build failed: $config $case $run" >&2
        exit 1
    fi
    wall=$(since "$start")
    cp "$(ls -t "$target"/cargo-timings/cargo-timing-*.html | head -1)" \
        "$OUT/timings/$config-$case-$run.html"
    local log="$OUT/logs/cargo-$config-$case-$run.log"
    local compiled fresh link_s link_n hits misses
    compiled=$(grep -c '^ *Compiling ' "$log" || true)
    fresh=$(grep -c '^ *Fresh ' "$log" || true)
    link_n=0; link_s=0
    if [[ -f $links ]]; then
        link_n=$(wc -l < "$links")
        link_s=$(awk '{s += $2 - $1} END {printf "%.4f", s}' "$links")
    fi
    hits=0; misses=0
    if [[ $config == sccache* ]]; then
        read -r hits misses < <(sccache --show-stats --stats-format json | python3 -c '
import json, sys
stats = json.load(sys.stdin)["stats"]
count = lambda key: sum(stats.get(key, {}).get("counts", {}).values())
print(count("cache_hits"), count("cache_misses"))')
    fi
    if [[ -f $cachelog ]]; then
        hits=$(grep -c '^hit ' "$cachelog" || true)
        misses=$(grep -c '^miss ' "$cachelog" || true)
    fi
    printf '{"config":"%s","case":"%s","run":%d,"wall":%s,"compiled":%d,"fresh":%d,"links":%d,"link":%s,"hits":%d,"misses":%d%s}\n' \
        "$config" "$case" "$run" "$wall" "$compiled" "$fresh" \
        "$link_n" "$link_s" "$hits" "$misses" "${EXTRA:-}" | tee -a "$OUT/results.jsonl"
}

# edit <dir> <file> <n>: the original file plus one unique comment.
declare -A ORIGINAL
edit() {
    local path="$1/$2"
    [[ -n ${ORIGINAL[$path]:-} ]] || ORIGINAL[$path]=$(cat "$path"; echo x)
    printf '%s\n// tinymold-bench edit %s\n' "${ORIGINAL[$path]%x}" "$3" > "$path"
}
restore() {
    local path
    for path in "${!ORIGINAL[@]}"; do printf '%s' "${ORIGINAL[$path]%x}" > "$path"; done
}
trap restore EXIT

# Download every dependency first, so no build's time includes the network.
(cd "$SRC" && cargo fetch --locked > "$OUT/logs/fetch.log" 2>&1)

store_bytes() { du -sb "$1" 2>/dev/null | cut -f1 || echo 0; }

# Each sccache config gets its own server on a private cache directory, so
# one never hits entries the other stored. The cache size is set high enough
# that nothing is evicted mid-run.
export SCCACHE_CACHE_SIZE=100G SCCACHE_IDLE_TIMEOUT=0

for config in $CONFIGS; do
    if [[ $config == sccache* ]]; then
        export SCCACHE_DIR="/work/$config"
        sccache --stop-server > /dev/null 2>&1 || true
        want cold && rm -rf "$SCCACHE_DIR"
        sccache --start-server > /dev/null
    fi
    if want cold; then
        rm -rf "$SRC/target/$config"
        is_tinymold "$config" && rm -rf "$(store_of "$config")"
        build "$config" cold 1 "$SRC"
    fi
    if want noop; then
        for run in $(seq "$RUNS"); do build "$config" noop "$run" "$SRC"; done
    fi
    if want edit_leaf; then
        for run in $(seq "$RUNS"); do
            edit "$SRC" "$LEAF" "$config-leaf-$run-$RANDOM"
            build "$config" edit_leaf "$run" "$SRC"
        done
        restore
    fi
    if want edit_core; then
        for run in $(seq "$RUNS"); do
            edit "$SRC" "$CORE" "$config-core-$run-$RANDOM"
            build "$config" edit_core "$run" "$SRC"
        done
        restore
    fi
    if ! is_tinymold "$config" && [[ $config != sccache* ]]; then
        continue
    fi
    if want warm; then
        # Back to the committed sources, so the store holds every unit as shipped.
        build "$config" settle 1 "$SRC"
        for run in $(seq "$RUNS"); do
            rm -rf "$SRC/target/$config"
            cache=$(store_of "$config"); [[ $config == sccache* ]] && cache=$SCCACHE_DIR
            EXTRA=",\"store_bytes\":$(store_bytes "$cache")" build "$config" warm "$run" "$SRC"
        done
    fi
    if want second_path; then
        for run in $(seq "$RUNS"); do
            rm -rf "$SECOND"
            mkdir -p "$SECOND"
            (cd "$SRC" && tar --exclude=./target -cf - .) | tar -xf - -C "$SECOND"
            build "$config" second_path "$run" "$SECOND"
        done
    fi
    if want snapshot && is_tinymold "$config"; then
        tinymold --store "$(store_of "$config")" snapshot save openhuman --workspace "$SRC" \
            --target-dir "$SRC/target/$config" > /dev/null
        for run in $(seq "$RUNS"); do
            rm -rf "$SECOND/target"
            mkdir -p "$SECOND/target"
            start=$(now)
            tinymold --store "$(store_of "$config")" snapshot restore openhuman --workspace "$SECOND" \
                --target-dir "$SECOND/target/$config" > /dev/null
            restore_s=$(since "$start")
            EXTRA=",\"restore\":$restore_s" build "$config" snapshot "$run" "$SECOND"
        done
    fi
done
echo done >&2
