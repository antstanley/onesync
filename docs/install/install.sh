#!/usr/bin/env bash
# onesync installer — `curl -fsSL https://onesync.example.com/install.sh | bash`
#
# Fetches the latest notarised release tarball from GitHub Releases, verifies the
# sha256, and installs `onesync` + `onesyncd` to a system bin directory. Idempotent:
# re-running upgrades in place.
#
# Environment overrides:
#   ONESYNC_REPO    — `<owner>/onesync` (default below; edit on fork)
#   ONESYNC_VERSION — explicit tag (default: latest release)
#   ONESYNC_PREFIX  — install prefix (default: /usr/local; falls back to ~/.local
#                     if /usr/local is not writable and sudo is unavailable)
#
# Exit codes:
#   0  installed
#   1  generic failure (network, checksum, etc.)
#   2  unsupported platform (non-macOS)

set -euo pipefail

ONESYNC_REPO="${ONESYNC_REPO:-<owner>/onesync}"
ONESYNC_VERSION="${ONESYNC_VERSION:-}"
ONESYNC_PREFIX="${ONESYNC_PREFIX:-/usr/local}"

err() { printf 'error: %s\n' "$*" >&2; }
info() { printf '==> %s\n' "$*"; }

trap 'rm -rf "${WORK:-}"' EXIT

require_macos() {
    case "$(uname -s)" in
        Darwin) ;;
        *) err "onesync only ships macOS binaries today (uname -s: $(uname -s))"; exit 2 ;;
    esac
}

resolve_version() {
    if [ -n "$ONESYNC_VERSION" ]; then
        echo "$ONESYNC_VERSION"
        return
    fi
    info "Resolving latest version from github.com/$ONESYNC_REPO/releases/latest"
    local tag
    tag=$(curl -fsSL "https://api.github.com/repos/$ONESYNC_REPO/releases/latest" \
        | awk -F'"' '/"tag_name":/{print $4; exit}')
    if [ -z "$tag" ]; then
        err "could not determine latest release tag"
        exit 1
    fi
    echo "$tag"
}

download_and_verify() {
    local tag="$1" tarball="$2" sha_file="$3"
    local base="https://github.com/$ONESYNC_REPO/releases/download/$tag"
    info "Downloading $tarball"
    curl -fsSL --retry 3 -o "$WORK/$tarball" "$base/$tarball"
    info "Downloading $sha_file"
    curl -fsSL --retry 3 -o "$WORK/$sha_file" "$base/$sha_file"
    local expected actual
    expected=$(awk '{print $1}' < "$WORK/$sha_file")
    actual=$(shasum -a 256 "$WORK/$tarball" | awk '{print $1}')
    if [ "$expected" != "$actual" ]; then
        err "sha256 mismatch: expected $expected, got $actual"
        exit 1
    fi
    info "sha256 verified: $actual"
}

pick_install_dir() {
    local desired="$ONESYNC_PREFIX/bin"
    if [ -w "$ONESYNC_PREFIX" ] || [ -w "$desired" ]; then
        echo "$desired"
        return
    fi
    if command -v sudo > /dev/null 2>&1; then
        # We will sudo install into the system prefix.
        echo "$desired"
        return
    fi
    # Fall back to a user-writable location.
    info "$desired is not writable and sudo is unavailable; falling back to \$HOME/.local/bin"
    echo "$HOME/.local/bin"
}

place_binary() {
    local src="$1" dest_dir="$2" name="$3"
    mkdir -p "$dest_dir" 2>/dev/null || true
    if [ -w "$dest_dir" ]; then
        install -m 0755 "$src" "$dest_dir/$name"
    else
        sudo install -m 0755 "$src" "$dest_dir/$name"
    fi
}

WORK="$(mktemp -d 2> /dev/null || mktemp -d -t onesync-install)"

require_macos
TAG="$(resolve_version)"
VERSION="${TAG#v}"
TARBALL="onesync-${VERSION}-macos-universal.tar.gz"
SHA_FILE="${TARBALL}.sha256"

download_and_verify "$TAG" "$TARBALL" "$SHA_FILE"

tar -xzf "$WORK/$TARBALL" -C "$WORK"
DEST="$(pick_install_dir)"
info "Installing onesync + onesyncd to $DEST"
place_binary "$WORK/onesync" "$DEST" "onesync"
place_binary "$WORK/onesyncd" "$DEST" "onesyncd"

cat <<EOF

onesync $VERSION installed.

Next steps:
    1. Register an Azure AD app and grab its client id — see
       https://github.com/$ONESYNC_REPO/blob/main/docs/install/README.md

    2. Tell onesync about the client id, then install the LaunchAgent:

        onesync config set --azure-ad-client-id <YOUR-CLIENT-ID>
        onesync account login         # opens browser for OAuth
        onesync pair add ...
        onesync service install       # writes the LaunchAgent + starts the daemon

If $DEST is not on your \$PATH yet, add it:

    echo 'export PATH="$DEST:\$PATH"' >> ~/.zshrc

EOF
