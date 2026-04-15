#!/usr/bin/env bash
# Local copy benchmark: bcmr vs cp vs rsync vs tar pipe.
#
# Usage: run_local.sh <kind> [bcmr_binary]
set -eu

kind="${1:-many-small}"
bcmr="${2:-/Users/snaix/Documents/bcmr/target/release/bcmr}"

if [ ! -x "$bcmr" ]; then
    echo "bcmr binary not found or not executable: $bcmr" >&2
    exit 1
fi

bench_dir=/tmp/bcmr-bench
src="$bench_dir/src"
dst="$bench_dir/dst"

"$(dirname "$0")/gen_workload.sh" "$src" "$kind"

echo
echo "=== local copy bench: $kind ==="
echo "bcmr: $("$bcmr" --version)"
echo

# Warmup once to prime the OS cache in the same way for all tools.
"$bcmr" copy -r "$src/" "$dst-warmup/" >/dev/null 2>&1 || true
rm -rf "$dst-warmup"

hyperfine \
    --warmup 1 \
    --runs 5 \
    --export-markdown "$bench_dir/local-$kind.md" \
    --export-json "$bench_dir/local-$kind.json" \
    --prepare "rm -rf $dst" \
    --command-name "bcmr copy -r" "$bcmr copy -r $src/ $dst/" \
    --prepare "rm -rf $dst" \
    --command-name "cp -R" "cp -R $src/ $dst/" \
    --prepare "rm -rf $dst" \
    --command-name "rsync -a" "rsync -a $src/ $dst/" \
    --prepare "rm -rf $dst && mkdir -p $dst" \
    --command-name "tar | tar" "bash -c 'tar -C $src -cf - . | tar -C $dst -xf -'"

echo
cat "$bench_dir/local-$kind.md"
