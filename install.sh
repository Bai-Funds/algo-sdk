#!/bin/sh
# Sequence CLI installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Sequence-Markets/algo-sdk/main/install.sh | sh
#
# Installs the latest `sequence` binary to ~/.local/bin (or /usr/local/bin with sudo).
# Verifies SHA-256 checksum before installing.
#
# Environment:
#   SEQUENCE_INSTALL_DIR  — override install directory (default: ~/.local/bin)
#   GITHUB_TOKEN          — required if downloading from a private repo

set -e

# When releases move to the public repo, update this:
REPO="${SEQUENCE_CLI_REPO:-Sequence-Markets/algo-sdk}"
BINARY="sequence"
INSTALL_DIR="${SEQUENCE_INSTALL_DIR:-$HOME/.local/bin}"

# --- Detect platform ---

detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin) os="apple-darwin" ;;
        Linux)  os="unknown-linux-gnu" ;;
        *)
            echo "Error: Unsupported OS: $OS" >&2
            exit 1
            ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *)
            echo "Error: Unsupported architecture: $ARCH" >&2
            exit 1
            ;;
    esac

    echo "${arch}-${os}"
}

# --- GitHub auth header (for private repos) ---

auth_header() {
    if [ -n "$GITHUB_TOKEN" ]; then
        echo "Authorization: token ${GITHUB_TOKEN}"
    else
        echo "X-No-Auth: true"
    fi
}

# --- Find latest release ---

latest_tag() {
    curl -fsSL -H "$(auth_header)" \
        "https://api.github.com/repos/${REPO}/releases" \
        | grep -o '"tag_name": *"cli/v[^"]*"' \
        | head -1 \
        | sed 's/"tag_name": *"//;s/"//'
}

# --- Main ---

main() {
    TARGET="$(detect_target)"
    ARCHIVE="sequence-${TARGET}.tar.gz"

    echo "Detecting platform... ${TARGET}"

    TAG="$(latest_tag)"
    if [ -z "$TAG" ]; then
        echo "Error: No CLI release found on ${REPO}" >&2
        exit 1
    fi

    VERSION="${TAG#cli/v}"
    echo "Latest version: ${VERSION} (${TAG})"

    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"
    CHECKSUM_URL="https://github.com/${REPO}/releases/download/${TAG}/checksums.txt"

    TMPDIR="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR"' EXIT

    echo "Downloading ${ARCHIVE}..."
    curl -fSL --progress-bar -H "$(auth_header)" -o "${TMPDIR}/${ARCHIVE}" "$DOWNLOAD_URL"
    curl -fsSL -H "$(auth_header)" -o "${TMPDIR}/checksums.txt" "$CHECKSUM_URL"

    # Verify checksum
    echo "Verifying checksum..."
    EXPECTED="$(grep "${ARCHIVE}" "${TMPDIR}/checksums.txt" | awk '{print $1}')"
    if [ -z "$EXPECTED" ]; then
        echo "Error: No checksum found for ${ARCHIVE}" >&2
        exit 1
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "${TMPDIR}/${ARCHIVE}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "${TMPDIR}/${ARCHIVE}" | awk '{print $1}')"
    else
        echo "Warning: No sha256sum or shasum found, skipping verification" >&2
        ACTUAL="$EXPECTED"
    fi

    if [ "$ACTUAL" != "$EXPECTED" ]; then
        echo "Error: Checksum mismatch!" >&2
        echo "  Expected: ${EXPECTED}" >&2
        echo "  Actual:   ${ACTUAL}" >&2
        exit 1
    fi
    echo "Checksum OK"

    # Extract
    tar xzf "${TMPDIR}/${ARCHIVE}" -C "${TMPDIR}"

    # Install
    mkdir -p "$INSTALL_DIR"
    mv "${TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    chmod +x "${INSTALL_DIR}/${BINARY}"

    echo ""
    echo "Installed ${BINARY} v${VERSION} to ${INSTALL_DIR}/${BINARY}"

    # Check if install dir is in PATH
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            echo ""
            echo "Add to your PATH:"
            echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo ""
            echo "Or add to your shell profile (~/.zshrc or ~/.bashrc)."
            ;;
    esac
}

main
