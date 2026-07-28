#!/usr/bin/env bash
set -Eeuo pipefail

dist=${1:-dist}
[ -d "$dist" ] || {
    echo "release artifact directory does not exist: $dist" >&2
    exit 1
}
dist=$(cd "$dist" && pwd -P)

expected=(
    bcmr-x86_64-linux
    bcmr-aarch64-linux
    bcmr-x86_64-macos
    bcmr-aarch64-macos
    bcmr-x86_64-windows
    bcmr-aarch64-windows
    bcmr-x86_64-freebsd
)

verify_root=$(mktemp -d "${TMPDIR:-/tmp}/bcmr-release-assets.XXXXXX")
cleanup() {
    rm -rf -- "$verify_root"
}
trap cleanup EXIT HUP INT TERM

for name in "${expected[@]}"; do
    [ -d "$dist/$name" ] || {
        echo "missing build artifact $name" >&2
        exit 1
    }

    if [[ "$name" == *-windows ]]; then
        [ -f "$dist/$name/bcmr.exe" ] || {
            echo "$name does not contain bcmr.exe" >&2
            exit 1
        }
        (cd "$dist/$name" && zip -q "$dist/${name}.zip" bcmr.exe)
    else
        [ -f "$dist/$name/bcmr" ] || {
            echo "$name does not contain bcmr" >&2
            exit 1
        }

        # GitHub Artifact downloads normalize file modes to 0644. Restore the
        # executable bit before creating the archive users download directly.
        chmod 0755 "$dist/$name/bcmr"
        tar -C "$dist/$name" -czf "$dist/${name}.tar.gz" bcmr

        mkdir "$verify_root/$name"
        tar -xzf "$dist/${name}.tar.gz" -C "$verify_root/$name"
        [ -x "$verify_root/$name/bcmr" ] || {
            echo "$name archive does not preserve an executable bcmr" >&2
            exit 1
        }
    fi
done

(
    cd "$dist"
    sha256sum *.tar.gz *.zip >sha256sums.txt
    [ "$(wc -l <sha256sums.txt)" -eq 7 ]
)
