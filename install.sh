#!/usr/bin/env bash
set -Eeuo pipefail

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BLUE=$'\033[0;34m'
    GREEN=$'\033[0;32m'
    RED=$'\033[0;31m'
    RESET=$'\033[0m'
else
    BLUE=""
    GREEN=""
    RED=""
    RESET=""
fi

info() {
    printf '%s>>> %s%s\n' "$BLUE" "$1" "$RESET"
}

fail() {
    printf '%sError: %s%s\n' "$RED" "$1" "$RESET" >&2
    exit 1
}

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS:$ARCH" in
    Linux:x86_64 | Linux:amd64)
        ASSET_NAME="bcmr-x86_64-linux.tar.gz"
        ;;
    Linux:aarch64 | Linux:arm64)
        ASSET_NAME="bcmr-aarch64-linux.tar.gz"
        ;;
    Darwin:x86_64 | Darwin:amd64)
        ASSET_NAME="bcmr-x86_64-macos.tar.gz"
        ;;
    Darwin:arm64 | Darwin:aarch64)
        ASSET_NAME="bcmr-aarch64-macos.tar.gz"
        ;;
    FreeBSD:x86_64 | FreeBSD:amd64)
        ASSET_NAME="bcmr-x86_64-freebsd.tar.gz"
        ;;
    *)
        fail "unsupported platform ${OS}/${ARCH}; use cargo install bcmr --locked"
        ;;
esac

if [ -n "${BCMR_INSTALL_DIR:-}" ]; then
    INSTALL_DIR=$BCMR_INSTALL_DIR
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
    INSTALL_DIR=/usr/local/bin
elif [ "$(id -u)" -eq 0 ]; then
    INSTALL_DIR=/usr/local/bin
else
    [ -n "${HOME:-}" ] || fail "HOME is unset; set BCMR_INSTALL_DIR explicitly"
    INSTALL_DIR="${HOME}/.local/bin"
fi

if [ -n "${BCMR_DOWNLOAD_BASE:-}" ]; then
    DOWNLOAD_BASE=${BCMR_DOWNLOAD_BASE%/}
elif [ -n "${BCMR_VERSION:-}" ]; then
    DOWNLOAD_BASE="https://github.com/Bengerthelorf/bcmr/releases/download/${BCMR_VERSION}"
else
    DOWNLOAD_BASE="https://github.com/Bengerthelorf/bcmr/releases/latest/download"
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bcmr-install.XXXXXX")
STAGE_PATH=""
cleanup() {
    if [ -n "$STAGE_PATH" ]; then
        rm -f -- "$STAGE_PATH"
    fi
    rm -rf -- "$TMP_DIR"
}
trap cleanup EXIT HUP INT TERM

download() {
    local url=$1
    local destination=$2
    local partial="${destination}.part"
    local secure_https=0
    local first_attempt_failed=0

    case "$url" in
        https://*)
            secure_https=1
            ;;
        file://*)
            [ "${BCMR_ALLOW_FILE_URL:-}" = "1" ] ||
                fail "file downloads require BCMR_ALLOW_FILE_URL=1"
            ;;
        *)
            fail "refusing non-HTTPS download URL: ${url}"
            ;;
    esac

    rm -f -- "$partial"
    if [ "$secure_https" -eq 1 ]; then
        curl --fail --location --retry 5 --retry-delay 2 --retry-connrefused \
            --connect-timeout 15 --max-time 1800 --proto '=https' --tlsv1.2 \
            --output "$partial" "$url" || first_attempt_failed=1
    else
        curl --fail --location --retry 5 --retry-delay 2 --retry-connrefused \
            --connect-timeout 15 --max-time 1800 \
            --output "$partial" "$url" || first_attempt_failed=1
    fi

    if [ "$first_attempt_failed" -eq 1 ]; then
        rm -f -- "$partial"
        info "Retrying through HTTP/1.1 for a restrictive proxy"
        if [ "$secure_https" -eq 1 ]; then
            curl --fail --location --retry 5 --retry-delay 2 --retry-connrefused \
                --connect-timeout 15 --max-time 1800 --http1.1 \
                --proto '=https' --tlsv1.2 --output "$partial" "$url"
        else
            curl --fail --location --retry 5 --retry-delay 2 --retry-connrefused \
                --connect-timeout 15 --max-time 1800 --http1.1 \
                --output "$partial" "$url"
        fi
    fi
    mv -f -- "$partial" "$destination"
}

checksum_file="${TMP_DIR}/sha256sums.txt"
archive="${TMP_DIR}/${ASSET_NAME}"

info "BCMR installer (${OS}/${ARCH})"
printf 'Asset: %s\nInstall directory: %s\n' "$ASSET_NAME" "$INSTALL_DIR"

info "Downloading release and checksum manifest"
download "${DOWNLOAD_BASE}/${ASSET_NAME}" "$archive"
download "${DOWNLOAD_BASE}/sha256sums.txt" "$checksum_file"

expected=$(
    awk -v asset="$ASSET_NAME" '
        $2 == asset || $2 == "*" asset {
            print $1
            exit
        }
    ' "$checksum_file"
)
[ -n "$expected" ] || fail "${ASSET_NAME} is missing from sha256sums.txt"

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive" | awk '{print $1}')
else
    fail "sha256sum or shasum is required to verify the download"
fi
[ "$actual" = "$expected" ] || fail "SHA-256 mismatch for ${ASSET_NAME}"
printf '%sChecksum verified.%s\n' "$GREEN" "$RESET"

info "Extracting verified binary"
tar -xzf "$archive" -C "$TMP_DIR" bcmr
BINARY_PATH="${TMP_DIR}/bcmr"
[ -f "$BINARY_PATH" ] || fail "archive does not contain bcmr"
chmod 0755 "$BINARY_PATH"

mkdir -p -- "$INSTALL_DIR"
[ -w "$INSTALL_DIR" ] ||
    fail "${INSTALL_DIR} is not writable; set BCMR_INSTALL_DIR to a user-writable directory"

TARGET_PATH="${INSTALL_DIR}/bcmr"
STAGE_PATH="${INSTALL_DIR}/.bcmr.install.$$"
cp -- "$BINARY_PATH" "$STAGE_PATH"
chmod 0755 "$STAGE_PATH"
"$STAGE_PATH" --version >/dev/null ||
    fail "downloaded binary could not execute on ${OS}/${ARCH}"
mv -f -- "$STAGE_PATH" "$TARGET_PATH"
STAGE_PATH=""

printf '%sInstalled BCMR atomically to %s%s\n' "$GREEN" "$TARGET_PATH" "$RESET"
case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        printf 'Add %s to PATH, then run: bcmr --version\n' "$INSTALL_DIR"
        ;;
esac
