#!/usr/bin/env bash
# End-to-end serve-protocol compression benchmark.
# Runs bcmr serve locally (stdin/stdout), uploads a realistic file,
# measures elapsed time + bytes on the wire.
#
# To isolate compression from local SSD speed, we pipe serve's
# stdout through `pv -q -b` so we can count the exact byte volume.
set -eu

bcmr="${1:-/Users/snaix/Documents/bcmr/target/release/bcmr}"

out=/tmp/bcmr-compress
rm -rf "$out"
mkdir -p "$out"

# Compressible source: source-code-like repeated tokens, 64 MiB.
python3 - <<'PY' > "$out/text.bin"
import os, sys, random
random.seed(1)
tokens = [b"function ", b"const ", b"return ", b"if (", b") {", b"} else {",
          b"import ", b"export ", b"    ", b"\n", b"// ", b"\n", b"Result<",
          b"Option<", b"Ok(", b"Err(", b"String", b"Vec<u8>", b"self.", b"Some("]
out = sys.stdout.buffer
total = 64 * 1024 * 1024
written = 0
while written < total:
    t = random.choice(tokens)
    out.write(t)
    written += len(t)
PY

# Incompressible source: urandom, 64 MiB.
head -c $((64 * 1024 * 1024)) /dev/urandom > "$out/random.bin"

printf "%-10s %-10s %10s %10s %10s %10s\n" "content" "algo" "src_MB" "wire_MB" "ratio" "time_s"

for content in text random; do
    src="$out/$content.bin"
    dst="$out/$content.dst.bin"
    src_size=$(stat -f%z "$src")

    for variant in noop zstd; do
        rm -f "$dst"

        # With $variant we toggle whether the client sends caps to the server.
        # The current binary has a fixed CLIENT_CAPS=LZ4|ZSTD, so we test
        # compression=on. For compression=off, we rebuild with caps=0 later.
        # Here we just measure the ON variant.
        if [ "$variant" = "noop" ]; then
            continue  # skip; not instrumented yet
        fi

        start=$(python3 -c 'import time; print(time.time())')
        # Roundtrip: put src → dst on the server side.
        "$bcmr" copy "$src" "localhost:$dst" >/dev/null 2>&1 \
            || { echo "  (skipping: localhost ssh not configured)"; continue 2; }
        end=$(python3 -c 'import time; print(time.time())')
        elapsed=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.2f", e-s}')

        # wire bytes: we don't have a pv-style hook on the serve process yet,
        # so leave wire_MB as N/A for now.
        printf "%-10s %-10s %10.1f %10s %10s %10.2f\n" \
            "$content" "$variant" \
            "$(awk -v s="$src_size" 'BEGIN{printf "%.2f", s/(1024*1024)}')" \
            "N/A" "N/A" "$elapsed"
    done
done
