#!/usr/bin/env bash
set -Eeuo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/bcmr-release-assets-test.XXXXXX")
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

unix_assets=(
    bcmr-x86_64-linux
    bcmr-aarch64-linux
    bcmr-x86_64-macos
    bcmr-aarch64-macos
    bcmr-x86_64-freebsd
)
windows_assets=(
    bcmr-x86_64-windows
    bcmr-aarch64-windows
)

for name in "${unix_assets[@]}"; do
    mkdir -p "$work/dist/$name"
    printf 'test binary\n' >"$work/dist/$name/bcmr"
    chmod 0644 "$work/dist/$name/bcmr"
done
for name in "${windows_assets[@]}"; do
    mkdir -p "$work/dist/$name"
    printf 'test binary\n' >"$work/dist/$name/bcmr.exe"
    chmod 0644 "$work/dist/$name/bcmr.exe"
done

TMPDIR="$work" bash "$repo_root/scripts/prepare-release-assets.sh" "$work/dist"

for name in "${unix_assets[@]}"; do
    mkdir "$work/check-$name"
    tar -xzf "$work/dist/${name}.tar.gz" -C "$work/check-$name"
    test -x "$work/check-$name/bcmr"
    test "$(tar -tzf "$work/dist/${name}.tar.gz")" = "bcmr"
done
for name in "${windows_assets[@]}"; do
    test "$(unzip -Z1 "$work/dist/${name}.zip")" = "bcmr.exe"
done

(
    cd "$work/dist"
    test "$(wc -l <sha256sums.txt)" -eq 7
    test -z "$(awk '$2 ~ /^\.\// { print; exit }' sha256sums.txt)"
    sha256sum -c sha256sums.txt
)
