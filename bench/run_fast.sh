#!/usr/bin/env bash
# Compare default GET vs --fast (skip server hash; splice on Linux)
# for a 1 GiB random file pulled from a remote SSH peer.
#
# Usage: run_fast.sh <ssh_alias> [bcmr_binary]
set -eu

host="${1:-host-L}"
bcmr="${2:-/Users/snaix/Documents/bcmr/target/release/bcmr}"

bench_dir=/tmp/bcmr-fast-bench
rm -rf "$bench_dir"
mkdir -p "$bench_dir"

remote_dir=/tmp/bcmr-fast-bench-remote
ssh -o BatchMode=yes "$host" "rm -rf $remote_dir && mkdir -p $remote_dir && head -c $((1024*1024*1024)) /dev/urandom > $remote_dir/src.bin"

echo "Pulling 1 GiB from $host with default vs --fast (no compression)..."
hyperfine \
    --warmup 1 \
    --runs 3 \
    --prepare "rm -f $bench_dir/dst.bin" \
    --command-name "default" "$bcmr copy --compress=none $host:$remote_dir/src.bin $bench_dir/dst.bin" \
    --prepare "rm -f $bench_dir/dst.bin" \
    --command-name "fast" "$bcmr copy --compress=none --fast $host:$remote_dir/src.bin $bench_dir/dst.bin"

ssh -o BatchMode=yes "$host" "rm -rf $remote_dir" 2>/dev/null || true
