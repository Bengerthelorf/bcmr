#!/usr/bin/env bash
# Sweep --jobs to find the local-copy concurrency knee.
set -eu

kind="${1:-many-medium}"
bcmr="${2:-/Users/snaix/Documents/bcmr/target/release/bcmr}"

bench_dir=/tmp/bcmr-bench
src="$bench_dir/src"
dst="$bench_dir/dst"

"$(dirname "$0")/gen_workload.sh" "$src" "$kind" >/dev/null

echo "=== jobs sweep: $kind ==="
echo "bcmr: $("$bcmr" --version)"
echo

args=()
for j in 1 2 4 8 16 32; do
    args+=(--prepare "rm -rf $dst" --command-name "bcmr -j$j" "$bcmr copy -r -j $j $src/ $dst/")
done

hyperfine \
    --warmup 1 \
    --runs 5 \
    --export-markdown "$bench_dir/sweep-$kind.md" \
    --export-json "$bench_dir/sweep-$kind.json" \
    "${args[@]}"

echo
cat "$bench_dir/sweep-$kind.md"
