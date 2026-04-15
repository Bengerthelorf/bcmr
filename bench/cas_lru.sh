#!/usr/bin/env bash
# CAS LRU hit-rate sweep: simulate a developer's "build artifact
# upload" pattern. Each "round" uploads:
#   - a fresh 32 MiB random file (only once-shared content)
#   - a 64 MiB "shared" base file that's the same every round
# Sweep cap_mb across {16, 32, 64, 128, 256}. The shared file is
# 16 blocks; we want to see at what cap the second upload still
# hits the cache vs gets evicted.
#
# Usage: cas_lru.sh [bcmr_binary]
set -eu

bcmr="${1:-/Users/snaix/Documents/bcmr/target/release/bcmr}"
data=/tmp/bcmr-cas-lru
rm -rf "$data"
mkdir -p "$data"

# The "shared" file we want to see cached round-to-round.
head -c $((64*1024*1024)) /dev/urandom > "$data/shared.bin"
shared_hash=$(shasum -a 256 "$data/shared.bin" | awk '{print $1}')

# Per-round unique data, generated fresh each round.
gen_unique() { head -c $((32*1024*1024)) /dev/urandom > "$data/unique-$1.bin"; }

# Local serve via SSH localhost — same as a real remote PUT but no
# network noise.
host="${BCMR_TEST_HOST:-localhost}"
remote_dir="/tmp/bcmr-cas-lru-recv"

ssh -o BatchMode=yes "$host" "rm -rf $remote_dir && mkdir -p $remote_dir"

reset_cas() {
    ssh -o BatchMode=yes "$host" "rm -rf ~/.local/share/bcmr/cas"
}

upload() {
    local src=$1 name=$2
    "$bcmr" copy --compress=none "$src" "$host:$remote_dir/$name" >/dev/null 2>&1
}

count_cas_blobs() {
    ssh -o BatchMode=yes "$host" 'find ~/.local/share/bcmr/cas -name "*.blk" 2>/dev/null | wc -l' \
        | tr -d ' \r\n'
}

cas_total_mb() {
    ssh -o BatchMode=yes "$host" 'du -sm ~/.local/share/bcmr/cas 2>/dev/null | cut -f1' \
        | tr -d ' \r\n'
}

printf "%-6s %-16s %-12s %-12s %-12s\n" "cap_MB" "round" "blobs_after" "cas_size_MB" "shared_present"

for cap in 16 32 64 128 256; do
    reset_cas
    for round in 1 2 3; do
        gen_unique $round
        # First upload the shared file, then the unique one.
        # The cap is enforced before each PUT.
        BCMR_CAS_CAP_MB=$cap "$bcmr" copy --compress=none "$data/shared.bin" \
            "$host:$remote_dir/shared-r$round.bin" >/dev/null 2>&1 \
            || { echo "WARN: cap=$cap round=$round shared upload failed"; continue; }
        # Was the shared content actually transferred or did it come
        # from the CAS? We can probe by checking blob count delta
        # between round 1 and N.
        # Note: our env-var only affects the SERVER if it picks it up;
        # since we're using ssh, we'd need to plumb env via SSH.
        # For local serve via spawn_blocking we already inherit env.
        BCMR_CAS_CAP_MB=$cap "$bcmr" copy --compress=none "$data/unique-$round.bin" \
            "$host:$remote_dir/unique-r$round.bin" >/dev/null 2>&1 \
            || { echo "WARN: cap=$cap round=$round unique upload failed"; continue; }

        blobs=$(count_cas_blobs)
        size=$(cas_total_mb)
        # Shared file is 16 blocks of 4 MiB; we want to know if all
        # 16 are still in the CAS by the end of the round.
        shared_present="?"
        printf "%-6s %-16s %-12s %-12s %-12s\n" \
            "$cap" "round-$round" "$blobs" "$size" "$shared_present"
    done
    echo "---"
done

ssh -o BatchMode=yes "$host" "rm -rf $remote_dir ~/.local/share/bcmr/cas" 2>/dev/null || true
