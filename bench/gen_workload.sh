#!/usr/bin/env bash
# Workload generator for bcmr benchmarks.
#
# Usage: gen_workload.sh <out_dir> <kind>
# Kinds:
#   many-small      100000 files × 1 KiB random bytes, 1000 dirs
#   many-medium     10000 files × 64 KiB random bytes, 100 dirs
#   single-large    1 file × 2 GiB random bytes
#   codebase-like   mixed structure resembling a large node_modules tree
set -eu

out="$1"
kind="$2"

if [ -z "$out" ] || [ -z "$kind" ]; then
    echo "usage: $0 <out_dir> <kind>" >&2
    exit 1
fi

rm -rf "$out"
mkdir -p "$out"

case "$kind" in
    many-small)
        # 100 dirs × 1000 files × 1 KiB = 100k files, ~100 MiB total.
        for i in $(seq 1 100); do
            mkdir -p "$out/d$i"
            for j in $(seq 1 1000); do
                head -c 1024 /dev/urandom > "$out/d$i/f$j.bin"
            done
        done
        ;;
    many-medium)
        # 100 dirs × 100 files × 64 KiB = 10k files, ~640 MiB total.
        for i in $(seq 1 100); do
            mkdir -p "$out/d$i"
            for j in $(seq 1 100); do
                head -c 65536 /dev/urandom > "$out/d$i/f$j.bin"
            done
        done
        ;;
    single-large)
        head -c $((2 * 1024 * 1024 * 1024)) /dev/urandom > "$out/big.bin"
        ;;
    codebase-like)
        # Mimic a node_modules-ish tree: deep nesting, small files, some symlinks.
        for i in $(seq 1 50); do
            pkg="$out/pkg$i"
            mkdir -p "$pkg/src" "$pkg/lib" "$pkg/test"
            for j in $(seq 1 20); do
                head -c 4096 /dev/urandom > "$pkg/src/mod$j.js"
                head -c 2048 /dev/urandom > "$pkg/test/spec$j.js"
            done
            head -c 8192 /dev/urandom > "$pkg/package.json"
            head -c 16384 /dev/urandom > "$pkg/lib/index.js"
            ln -s "../pkg$((((i % 50) + 1)))/lib/index.js" "$pkg/linked.js" 2>/dev/null || true
        done
        ;;
    *)
        echo "unknown kind: $kind" >&2
        exit 1
        ;;
esac

# Print a summary so callers can sanity-check.
count=$(find "$out" -type f | wc -l | tr -d ' ')
size=$(du -sk "$out" | awk '{print $1}')
echo "workload: $kind | files=$count | size=${size}KiB"
