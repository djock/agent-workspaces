#!/bin/sh
set -eu

REPOSITORY="${WS_REPOSITORY:-djock/agent-workspaces}"
INSTALL_DIR="${WS_INSTALL_DIR:-$HOME/.local/bin}"
BUILD_FROM_SOURCE=0
RUN_SETUP=1
REQUESTED_VERSION=""
ALLOW_UNSIGNED=0

# The minisign public key releases are signed with.
#
# Empty means "no signing key has been published yet". Verification is then
# impossible rather than merely skipped, and the install SAYS SO on every run —
# see the authenticity block below. Silence was the old behaviour and it was the
# wrong one: a gate that cannot check must not read like a gate that passed.
# Fill this in and CI will start signing; see docs/releasing.md. Because this
# key lives in the repository, trust is established when you obtain the
# repository and is strong from then on: a compromised release host can replace
# the assets but cannot produce a signature that matches this key.
MINISIGN_PUBKEY="${WS_MINISIGN_PUBKEY:-}"

usage() {
    cat <<'EOF'
Install ws.

Usage: ./install.sh [options]

Options:
  --build-from-source  Build the checked-out source with Cargo
  --version <version>  Install a release such as 0.1.0 or v0.1.0
  --install-dir <dir>  Install directory (default: ~/.local/bin)
  --no-setup           Do not install agent hooks and prompts
  --allow-unsigned     Install even if the release signature is missing or
                       cannot be checked. Not recommended.
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
        --allow-unsigned)
            ALLOW_UNSIGNED=1
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
        --pattern 'SHA256SUMS.minisig' \
        --dir "$TEMP_DIR" || true

    if [ ! -f "$TEMP_DIR/$ASSET" ] || [ ! -f "$TEMP_DIR/SHA256SUMS" ]; then
        echo "install.sh: could not download $ASSET and SHA256SUMS from $TAG" >&2
        exit 1
    fi

    (
        cd "$TEMP_DIR"

        # 1. Authenticity, before integrity.
        #
        # SHA256SUMS is served from the same place as the binary, so on its own it
        # proves only that the download was not corrupted in transit — a host that
        # can replace the asset can replace the checksum to match. The signature
        # over SHA256SUMS is what makes the checksum mean anything.
        #
        # Fails CLOSED: if a public key is configured and the signature is missing
        # or does not verify, the install stops. `--allow-unsigned` is the only way
        # past, and it has to be typed. (A soft gate that silently skips
        # verification when the verifier is absent is worse than none, because it
        # reads as "verified" in the output.)
        if [ -z "$MINISIGN_PUBKEY" ]; then
            # No key is baked into this installer, so authenticity cannot be
            # established at all. That is not a clean bill of health and is never
            # passed over in silence: an install that prints nothing here reads
            # as "verified", and the reader has no way to learn otherwise.
            if [ -f SHA256SUMS.minisig ]; then
                # Worse than unsigned: the release IS signed and this copy of
                # install.sh holds no key to check it against. A signature nobody
                # verifies is precisely what this gate exists to refuse, and a
                # stripped key is what a tampered installer looks like.
                echo "install.sh: $TAG ships SHA256SUMS.minisig, but this installer carries no public key to check it with." >&2
                echo "Get install.sh from the repository (it embeds the key), rather than trusting this copy." >&2
                if [ "$ALLOW_UNSIGNED" -eq 1 ]; then
                    echo "install.sh: WARNING: continuing unverified because --allow-unsigned was given." >&2
                else
                    echo "Re-run with --allow-unsigned to install it anyway." >&2
                    exit 1
                fi
            else
                echo "install.sh: WARNING: release authenticity was NOT checked — no signing key is published for $REPOSITORY yet." >&2
                echo "The checksum verified below proves the download arrived intact, not that it came from the ws authors." >&2
            fi
        else
            if [ ! -f SHA256SUMS.minisig ]; then
                if [ "$ALLOW_UNSIGNED" -eq 1 ]; then
                    echo "install.sh: WARNING: release $TAG has no signature; continuing because --allow-unsigned was given." >&2
                else
                    echo "install.sh: release $TAG is not signed (no SHA256SUMS.minisig)." >&2
                    echo "Re-run with --allow-unsigned to install it anyway." >&2
                    exit 1
                fi
            elif command -v minisign >/dev/null 2>&1; then
                minisign -V -P "$MINISIGN_PUBKEY" -m SHA256SUMS -x SHA256SUMS.minisig || {
                    echo "install.sh: SIGNATURE VERIFICATION FAILED for $TAG." >&2
                    echo "Do not install this. Report it: the assets do not match the release key." >&2
                    exit 1
                }
                echo "install.sh: signature verified."
            elif [ "$ALLOW_UNSIGNED" -eq 1 ]; then
                echo "install.sh: WARNING: minisign is not installed, signature NOT checked (--allow-unsigned)." >&2
            else
                echo "install.sh: minisign is not installed, so the release signature cannot be checked." >&2
                echo "Install it (brew install minisign / apt-get install minisign)," >&2
                echo "or re-run with --allow-unsigned to skip the check." >&2
                exit 1
            fi
        fi

        # 2. Integrity. `sha256sum` on Linux, `shasum` on macOS — hardcoding
        #    `shasum` broke the Linux path this installer now supports. Absent
        #    both, refuse rather than install an unverified binary.
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum --ignore-missing -c SHA256SUMS
        elif command -v shasum >/dev/null 2>&1; then
            # BSD shasum has no --ignore-missing; check just our asset's line.
            grep " $ASSET\$" SHA256SUMS > SHA256SUMS.ours
            shasum -a 256 -c SHA256SUMS.ours
        else
            echo "install.sh: neither sha256sum nor shasum found; cannot verify the download." >&2
            exit 1
        fi

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
