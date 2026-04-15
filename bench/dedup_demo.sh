#!/usr/bin/env bash
# End-to-end dedup demo: upload the same 64 MiB file twice to a remote
# host. The first run uploads the bytes; the second should hit the
# remote CAS for every block and complete in seconds even over a slow
# WAN link.
#
# Usage: dedup_demo.sh <ssh_alias> [bcmr_binary]
set -eu

host="${1:-4090_j}"
bcmr="${2:-/Users/snaix/Documents/bcmr/target/release/bcmr}"

bench_dir=/tmp/bcmr-dedup-demo
rm -rf "$bench_dir"
mkdir -p "$bench_dir"

# Generate 64 MiB of pseudo-random bytes (deterministic so reruns hit CAS).
head -c $((64 * 1024 * 1024)) /dev/urandom > "$bench_dir/payload.bin"
src_size=$(stat -f%z "$bench_dir/payload.bin" 2>/dev/null || stat -c%s "$bench_dir/payload.bin")
echo "src=$bench_dir/payload.bin size=$((src_size / 1024 / 1024))MiB host=$host"

remote_dir=/tmp/bcmr-dedup-recv
ssh -o BatchMode=yes "$host" "rm -rf $remote_dir ~/.local/share/bcmr/cas && mkdir -p $remote_dir"

echo
echo "=== Run 1: cold cache (no CAS hits) ==="
time "$bcmr" copy --compress=none "$bench_dir/payload.bin" "$host:$remote_dir/dst1.bin"

echo
echo "=== Run 2: warm cache (every block should be a CAS hit) ==="
time "$bcmr" copy --compress=none "$bench_dir/payload.bin" "$host:$remote_dir/dst2.bin"

echo
echo "=== Sanity check: both dst hashes equal local ==="
local_h=$(shasum -a 256 "$bench_dir/payload.bin" | awk '{print $1}')
remote_h1=$(ssh -o BatchMode=yes "$host" "sha256sum $remote_dir/dst1.bin | awk '{print \$1}'")
remote_h2=$(ssh -o BatchMode=yes "$host" "sha256sum $remote_dir/dst2.bin | awk '{print \$1}'")
echo "local : $local_h"
echo "dst1  : $remote_h1"
echo "dst2  : $remote_h2"
[ "$local_h" = "$remote_h1" ] && [ "$local_h" = "$remote_h2" ] && echo "PASS" || { echo "FAIL"; exit 1; }
