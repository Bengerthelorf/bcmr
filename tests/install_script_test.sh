#!/usr/bin/env bash
set -Eeuo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/bcmr-installer-test.XXXXXX")
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

case "$(uname -s):$(uname -m)" in
    Linux:x86_64 | Linux:amd64) asset=bcmr-x86_64-linux.tar.gz ;;
    Linux:aarch64 | Linux:arm64) asset=bcmr-aarch64-linux.tar.gz ;;
    Darwin:x86_64 | Darwin:amd64) asset=bcmr-x86_64-macos.tar.gz ;;
    Darwin:arm64 | Darwin:aarch64) asset=bcmr-aarch64-macos.tar.gz ;;
    FreeBSD:x86_64 | FreeBSD:amd64) asset=bcmr-x86_64-freebsd.tar.gz ;;
    *) echo "unsupported test host" >&2; exit 1 ;;
esac

mkdir -p "$work/release" "$work/install"
printf '#!/bin/sh\nprintf "bcmr 9.9.9-test\\n"\n' >"$work/bcmr"
chmod 0755 "$work/bcmr"
tar -czf "$work/release/$asset" -C "$work" bcmr

if command -v sha256sum >/dev/null 2>&1; then
    hash=$(sha256sum "$work/release/$asset" | awk '{print $1}')
else
    hash=$(shasum -a 256 "$work/release/$asset" | awk '{print $1}')
fi
printf '%s  %s\n' "$hash" "$asset" >"$work/release/sha256sums.txt"

NO_COLOR=1 \
BCMR_ALLOW_FILE_URL=1 \
BCMR_DOWNLOAD_BASE="file://$work/release" \
BCMR_INSTALL_DIR="$work/install" \
TMPDIR="$work" \
bash "$repo_root/install.sh"

test -x "$work/install/bcmr"
test "$("$work/install/bcmr" --version)" = "bcmr 9.9.9-test"
test -z "$(find "$work/install" -name '.bcmr.install.*' -print -quit)"

rm -f "$work/install/bcmr"
printf 'tampered\n' >>"$work/release/$asset"
if NO_COLOR=1 \
    BCMR_ALLOW_FILE_URL=1 \
    BCMR_DOWNLOAD_BASE="file://$work/release" \
    BCMR_INSTALL_DIR="$work/install" \
    TMPDIR="$work" \
    bash "$repo_root/install.sh" >"$work/tamper.out" 2>"$work/tamper.err"; then
    echo "tampered archive was accepted" >&2
    exit 1
fi
grep -q 'SHA-256 mismatch' "$work/tamper.err"
test ! -e "$work/install/bcmr"
