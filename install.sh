#!/bin/sh
set -eu

REPOSITORY="${WS_REPOSITORY:-djock/agent-workspaces}"
INSTALL_DIR="${WS_INSTALL_DIR:-$HOME/.local/bin}"
BUILD_FROM_SOURCE=0
RUN_SETUP=1
REQUESTED_VERSION=""

usage() {
    cat <<'EOF'
Install ws.

Usage: ./install.sh [options]

Options:
  --build-from-source  Build the checked-out source with Cargo
  --version <version>  Install a release such as 0.1.0 or v0.1.0
  --install-dir <dir>  Install directory (default: ~/.local/bin)
  --no-setup           Do not install agent hooks and prompts
  -h, --help           Show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --build-from-source)
            BUILD_FROM_SOURCE=1
            ;;
        --version)
            shift
            [ "$#" -gt 0 ] || { echo "install.sh: --version needs a value" >&2; exit 2; }
            REQUESTED_VERSION="$1"
            ;;
        --install-dir)
            shift
            [ "$#" -gt 0 ] || { echo "install.sh: --install-dir needs a value" >&2; exit 2; }
            INSTALL_DIR="$1"
            ;;
        --no-setup)
            RUN_SETUP=0
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "install.sh: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

mkdir -p "$INSTALL_DIR"
DESTINATION="$INSTALL_DIR/ws"

if [ "$BUILD_FROM_SOURCE" -eq 1 ]; then
    command -v cargo >/dev/null 2>&1 || {
        echo "install.sh: Cargo is required for --build-from-source" >&2
        exit 1
    }
    cargo build --release --locked
    install -m 0755 target/release/ws "$DESTINATION"
else
    command -v gh >/dev/null 2>&1 || {
        echo "install.sh: GitHub CLI is required to download the private release" >&2
        echo "Install gh and run: gh auth login" >&2
        exit 1
    }
    gh auth status >/dev/null 2>&1 || {
        echo "install.sh: GitHub CLI is not authenticated; run: gh auth login" >&2
        exit 1
    }

    # Resolve the release target from the host rather than hard-refusing
    # anything that is not Darwin/arm64. The release workflow now publishes a
    # statically linked Linux binary alongside the Apple Silicon one, so the
    # installer has to be able to ask for it.
    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "$OS/$ARCH" in
        Darwin/arm64)        TARGET="aarch64-apple-darwin" ;;
        Linux/x86_64)        TARGET="x86_64-unknown-linux-musl" ;;
        *)
            echo "install.sh: no prebuilt binary for $OS/$ARCH" >&2
            echo "Supported: Darwin/arm64, Linux/x86_64." >&2
            echo "Use: ./install.sh --build-from-source" >&2
            exit 1
            ;;
    esac

    if [ -n "$REQUESTED_VERSION" ]; then
        case "$REQUESTED_VERSION" in
            v*) TAG="$REQUESTED_VERSION" ;;
            *) TAG="v$REQUESTED_VERSION" ;;
        esac
    else
        TAG="$(gh release view --repo "$REPOSITORY" --json tagName --jq .tagName)"
    fi

    VERSION="${TAG#v}"
    ASSET="ws-v${VERSION}-${TARGET}.tar.gz"
    TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ws-install.XXXXXX")"
    trap 'rm -rf "$TEMP_DIR"' EXIT HUP INT TERM

    gh release download "$TAG" \
        --repo "$REPOSITORY" \
        --pattern "$ASSET" \
        --pattern SHA256SUMS \
        --dir "$TEMP_DIR"

    (
        cd "$TEMP_DIR"
        shasum -a 256 -c SHA256SUMS
        tar -xzf "$ASSET"
    )
    install -m 0755 "$TEMP_DIR/ws" "$DESTINATION"
fi

echo "Installed $("$DESTINATION" --version) at $DESTINATION"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo "Add this directory to PATH:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

if [ "$RUN_SETUP" -eq 1 ]; then
    "$DESTINATION" setup
    "$DESTINATION" -doctor || {
        echo "Installation completed, but the doctor reported an issue." >&2
        echo "Fix the message above, then run: ws setup && ws -doctor" >&2
    }
fi
