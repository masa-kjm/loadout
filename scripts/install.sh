#!/usr/bin/env bash
# Download and install a verified Loadout release archive.
# Usage: curl -fsSL https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.sh | bash
#        bash install.sh [--version vX.Y.Z] [--prefix ~/.local]

set -euo pipefail

REPOSITORY="masa-kjm/loadout"
PREFIX="${HOME}/.local"
VERSION=""

fail() {
    echo "error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ $# -ge 2 ]] || fail "--version requires a value"
            VERSION="$2"
            shift 2
            ;;
        --prefix)
            [[ $# -ge 2 ]] || fail "--prefix requires a value"
            PREFIX="$2"
            shift 2
            ;;
        *)
            fail "unknown option: $1"
            ;;
    esac
done

detect_target() {
    local os arch

    case "$(uname -s)" in
        Linux) os="linux" ;;
        Darwin) os="apple-darwin" ;;
        *) fail "unsupported OS: $(uname -s); use scripts/install.ps1 on Windows" ;;
    esac

    case "$(uname -m)" in
        x86_64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) fail "unsupported architecture: $(uname -m)" ;;
    esac

    if [[ "${os}" == "linux" ]]; then
        echo "${arch}-unknown-linux-gnu"
    else
        echo "${arch}-${os}"
    fi
}

resolve_latest_version() {
    curl -fsSL \
        -H "Accept: application/vnd.github+json" \
        -H "User-Agent: loadout-installer" \
        "https://api.github.com/repos/${REPOSITORY}/releases/latest" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1
}

verify_checksum() {
    local asset_path checksum_path expected actual

    asset_path="$1"
    checksum_path="$2"
    expected="$(awk 'NF { print $1; exit }' "${checksum_path}" | tr '[:upper:]' '[:lower:]')"
    [[ "${expected}" =~ ^[0-9a-fA-F]{64}$ ]] || fail "invalid SHA-256 checksum file"

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "${asset_path}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "${asset_path}" | awk '{print $1}')"
    else
        fail "required command not found: sha256sum or shasum"
    fi

    [[ "${actual}" == "${expected}" ]] || fail "SHA-256 verification failed"
}

require_command curl
require_command tar
require_command mktemp
require_command awk
require_command cp
require_command chmod
require_command tr

TARGET="$(detect_target)"

if [[ -z "${VERSION}" ]]; then
    echo "Fetching the latest release..."
    VERSION="$(resolve_latest_version)"
fi

[[ "${VERSION}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "version must be an exact vX.Y.Z release tag"

ARCHIVE_ROOT="loadout-${VERSION}-${TARGET}"
ARCHIVE="${ARCHIVE_ROOT}.tar.gz"
RELEASE_URL="https://github.com/${REPOSITORY}/releases/download/${VERSION}/${ARCHIVE}"
CHECKSUM_URL="${RELEASE_URL}.sha256"
TEMPORARY_DIRECTORY="$(mktemp -d)"
trap 'rm -rf "${TEMPORARY_DIRECTORY}"' EXIT

echo "Installing loadout ${VERSION} (${TARGET})..."
echo "Downloading ${RELEASE_URL}..."
curl -fsSL --output "${TEMPORARY_DIRECTORY}/${ARCHIVE}" "${RELEASE_URL}"
curl -fsSL --output "${TEMPORARY_DIRECTORY}/${ARCHIVE}.sha256" "${CHECKSUM_URL}"
verify_checksum "${TEMPORARY_DIRECTORY}/${ARCHIVE}" "${TEMPORARY_DIRECTORY}/${ARCHIVE}.sha256"

tar -tzf "${TEMPORARY_DIRECTORY}/${ARCHIVE}" \
    | awk -v root="${ARCHIVE_ROOT}/" 'index($0, root) != 1 || $0 ~ /(^|\/)\.\.($|\/)/ { exit 1 }' \
    || fail "archive has an unexpected layout"

EXTRACT_DIRECTORY="${TEMPORARY_DIRECTORY}/extract"
mkdir -p "${EXTRACT_DIRECTORY}"
tar -xzf "${TEMPORARY_DIRECTORY}/${ARCHIVE}" -C "${EXTRACT_DIRECTORY}"

BINARY_SOURCE="${EXTRACT_DIRECTORY}/${ARCHIVE_ROOT}/loadout"
[[ -f "${BINARY_SOURCE}" ]] || fail "loadout binary not found in the archive"

BINARY_DIRECTORY="${PREFIX}/bin"
BINARY_DESTINATION="${BINARY_DIRECTORY}/loadout"
mkdir -p "${BINARY_DIRECTORY}"
cp "${BINARY_SOURCE}" "${BINARY_DIRECTORY}/.loadout.$$"
chmod 755 "${BINARY_DIRECTORY}/.loadout.$$"
mv -f "${BINARY_DIRECTORY}/.loadout.$$" "${BINARY_DESTINATION}"

echo
echo "Installed loadout to ${BINARY_DESTINATION}"

if [[ ":${PATH}:" != *":${BINARY_DIRECTORY}:"* ]]; then
    echo "NOTE: ${BINARY_DIRECTORY} is not in your PATH."
    echo "      Add this to your shell profile: export PATH=\"${BINARY_DIRECTORY}:\$PATH\""
fi
