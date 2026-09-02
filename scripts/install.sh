#!/usr/bin/env bash
# install.sh — Download and install loadout from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/massa-kj/loadout/main/install.sh | bash
#   bash install.sh [--version v0.1.0] [--prefix ~/.local]
#
# Layout after install:
#   <prefix>/bin/loadout   (binary)

set -euo pipefail

REPO="massa-kj/loadout"
PREFIX="${HOME}/.local"
VERSION=""

# ── Argument parsing ──────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --prefix)  PREFIX="$2";  shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# ── Platform detection ────────────────────────────────────────────────────────

detect_target() {
    local os arch

    case "$(uname -s)" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)
            echo "error: unsupported OS: $(uname -s)" >&2
            echo "For Windows, use install.ps1 instead:" >&2
            echo "  irm https://raw.githubusercontent.com/massa-kj/loadout/main/install.ps1 | iex" >&2
            exit 1
            ;;
    esac

    case "$(uname -m)" in
        x86_64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)
            echo "error: unsupported architecture: $(uname -m)" >&2
            exit 1
            ;;
    esac

    echo "${os}-${arch}"
}

TARGET="$(detect_target)"

# ── Version resolution ────────────────────────────────────────────────────────

if [[ -z "$VERSION" ]]; then
    echo "Fetching latest release..."
    VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    [[ -z "$VERSION" ]] && { echo "error: failed to fetch latest version" >&2; exit 1; }
fi

echo "Installing loadout ${VERSION} (${TARGET})..."

# ── Download ──────────────────────────────────────────────────────────────────

TARBALL="loadout-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${URL}..."
curl -fsSL --output "${TMPDIR}/${TARBALL}" "${URL}"

# ── Install ───────────────────────────────────────────────────────────────────

BIN_DIR="${PREFIX}/bin"

# Create bin directory if needed
mkdir -p "${BIN_DIR}"

# Extract tarball to temporary location
EXTRACT_DIR="${TMPDIR}/extract"
mkdir -p "${EXTRACT_DIR}"
tar -xzf "${TMPDIR}/${TARBALL}" -C "${EXTRACT_DIR}" --strip-components=1

# Install binary
cp "${EXTRACT_DIR}/loadout" "${BIN_DIR}/loadout"
chmod +x "${BIN_DIR}/loadout"

# ── Done ──────────────────────────────────────────────────────────────────────

echo ""
echo "Installed loadout to ${BIN_DIR}/loadout"
echo ""

if ! echo ":${PATH}:" | grep -q ":${BIN_DIR}:"; then
    echo "NOTE: ${BIN_DIR} is not in your PATH."
    echo "      Add the following to your shell profile:"
    echo "        export PATH=\"${BIN_DIR}:\$PATH\""
fi
