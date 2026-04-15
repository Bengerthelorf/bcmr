#!/usr/bin/env bash
# Remote copy benchmark via bcmr copy with --compress flag sweep.
# Requires SSH alias reachable without password (-o BatchMode=yes).
#
# Usage: run_remote.sh <ssh_alias> <kind>
set -eu

host="${1:-4090_j}"
kind="${2:-text}"
bcmr="${3:-/Users/snaix/Documents/bcmr/target/release/bcmr}"

bench_dir=/tmp/bcmr-bench-remote
rm -rf "$bench_dir"
mkdir -p "$bench_dir"

case "$kind" in
    text)
        python3 - > "$bench_dir/src.bin" <<'PY'
import random, sys
random.seed(1)
tokens = [b"function ", b"const ", b"return ", b"if (", b") {", b"} else {",
          b"    ", b"\n", b"// ", b"\n", b"Result<", b"Option<", b"Ok(",
          b"Err(", b"String", b"Vec<u8>", b"self.", b"Some("]
out = sys.stdout.buffer
total = 64 * 1024 * 1024  # 64 MiB
written = 0
while written < total:
    t = random.choice(tokens)
    out.write(t)
    written += len(t)
PY
        ;;
    random)
        head -c $((64 * 1024 * 1024)) /dev/urandom > "$bench_dir/src.bin"
        ;;
    mixed)
        head -c $((32 * 1024 * 1024)) /dev/urandom > "$bench_dir/src.bin"
        python3 - >> "$bench_dir/src.bin" <<'PY'
import random, sys
random.seed(1)
tokens = [b"function ", b"const ", b"return ", b"    ", b"\n"]
out = sys.stdout.buffer
total = 32 * 1024 * 1024
written = 0
while written < total:
    out.write(random.choice(tokens))
    written += 8
PY
        ;;
esac

src_size=$(stat -f%z "$bench_dir/src.bin" 2>/dev/null || stat -c%s "$bench_dir/src.bin")
echo "workload=$kind size=$((src_size / 1024 / 1024))MiB host=$host"

remote_dir="/tmp/bcmr-bench-recv"
ssh -o BatchMode=yes "$host" "rm -rf $remote_dir && mkdir -p $remote_dir"

hyperfine \
    --warmup 1 \
    --runs 3 \
    --prepare "ssh -o BatchMode=yes $host 'rm -f $remote_dir/dst.bin'" \
    --command-name "none " "$bcmr copy --compress=none $bench_dir/src.bin $host:$remote_dir/dst.bin" \
    --prepare "ssh -o BatchMode=yes $host 'rm -f $remote_dir/dst.bin'" \
    --command-name "lz4  " "$bcmr copy --compress=lz4  $bench_dir/src.bin $host:$remote_dir/dst.bin" \
    --prepare "ssh -o BatchMode=yes $host 'rm -f $remote_dir/dst.bin'" \
    --command-name "zstd " "$bcmr copy --compress=zstd $bench_dir/src.bin $host:$remote_dir/dst.bin"

ssh -o BatchMode=yes "$host" "rm -rf $remote_dir" 2>/dev/null || true
