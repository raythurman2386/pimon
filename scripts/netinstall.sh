#!/usr/bin/env bash
set -euo pipefail

REPO="raythurman2386/pimon"
BINARY="pimon"
DEFAULT_PREFIX="$HOME/.local"
# Base URL for release artifacts. Overridable so the installer can be tested
# against a local mirror (e.g. `python3 -m http.server`) without hitting GitHub.
DEFAULT_RELEASE_BASE_URL="https://github.com/$REPO/releases/download"
RELEASE_BASE_URL="${PIMON_RELEASE_BASE_URL:-$DEFAULT_RELEASE_BASE_URL}"
VERSION=""
PREFIX=""
KEEP_TARBALL=false

# Pinned Ed25519 public key (PEM) used to verify the release signature. This is
# the root of trust: it must match the key used by scripts/sign-release.sh.
# A release whose checksums.txt.sig does not verify against this key is refused.
SIGNING_PUBLIC_KEY='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAriPoTqkF8p0cn+xgDWJq1ndENMHare4iZnLw/QSQbow=
-----END PUBLIC KEY-----'

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Install pimon from a prebuilt GitHub Release tarball. No root; installs
into ~/.local via the tarball's bundled install.sh (binary, desktop entry,
icon).

Options:
  --version VERSION  Install a specific version (default: latest)
  --prefix DIR       Install prefix (default: \$HOME/.local)
  --url URL          Base URL for release artifacts (default: GitHub releases)
  --keep-tarball     Keep the downloaded tarball next to the install prefix
  -h, --help         Show this help message

Environment:
  PIMON_RELEASE_BASE_URL  Override the release artifact base URL (same as --url)

Examples:
  curl -fsSL https://raw.githubusercontent.com/$REPO/master/scripts/netinstall.sh | sh
  $0 --version 0.1.0
  $0 --prefix /usr/local
EOF
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            VERSION="$2"
            shift 2
            ;;
        --prefix)
            PREFIX="$2"
            shift 2
            ;;
        --url)
            RELEASE_BASE_URL="$2"
            shift 2
            ;;
        --keep-tarball)
            KEEP_TARBALL=true
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            ;;
    esac
done

PREFIX="${PREFIX:-$DEFAULT_PREFIX}"

detect_platform() {
    local os arch triple

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux) os="unknown-linux-gnu" ;;
        *)
            echo "Error: unsupported OS: $os (prebuilt binaries are Linux-only)" >&2
            exit 1
            ;;
    esac

    case "$arch" in
        x86_64|amd64)
            arch="x86_64"
            ;;
        aarch64|arm64)
            arch="aarch64"
            ;;
        *)
            echo "Error: unsupported architecture: $arch" >&2
            echo "Prebuilt binaries cover x86_64 and aarch64 (Pi 5 / Pi 500)." >&2
            exit 1
            ;;
    esac

    triple="${arch}-${os}"
    echo "$triple"
}

get_latest_version() {
    local tag
    # Follow the /releases/latest redirect instead of the API. The API endpoint
    # is rate-limited to 60 req/hr per IP for unauthenticated clients, which
    # breaks installs on shared/NAT'd networks; the redirect is not limited.
    tag="$(curl -fsSL -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest" 2>/dev/null \
        | sed -E 's#.*/tag/##')"
    if [[ -z "$tag" ]]; then
        echo "Error: could not determine latest version from GitHub" >&2
        exit 1
    fi
    echo "$tag"
}

# fetch SRC DEST
#
# Download SRC to DEST. When SRC is a local path (absolute, relative, or a
# file:// URL), copy it directly instead of using curl. This lets the installer
# run against a local mirror / offline directory, and makes it testable without
# network access.
fetch() {
    local src="$1" dst="$2"
    if [[ "$src" == file://* ]]; then
        cp "${src#file://}" "$dst"
    elif [[ "$src" == /* || "$src" == ./* || "$src" == ../* ]]; then
        cp "$src" "$dst"
    else
        curl -fsSL --retry 3 --retry-delay 2 -o "$dst" "$src"
    fi
}

main() {
    local triple version_tag tarball_url checksum_url signature_url tmp_dir

    triple="$(detect_platform)"

    if [[ -z "$VERSION" ]]; then
        version_tag="$(get_latest_version)"
    else
        version_tag="$VERSION"
        if [[ "$version_tag" != v* ]]; then
            version_tag="v$version_tag"
        fi
    fi

    local version_no_v="${version_tag#v}"
    local artifact="pimon-${version_no_v}-${triple}.tar.gz"

    tarball_url="${RELEASE_BASE_URL}/${version_tag}/${artifact}"
    checksum_url="${RELEASE_BASE_URL}/${version_tag}/checksums.txt"
    signature_url="${RELEASE_BASE_URL}/${version_tag}/checksums.txt.sig"

    echo "==> Platform:  $triple"
    echo "==> Version:   $version_tag"
    echo "==> Artifact:  $artifact"
    echo "==> Install:   $PREFIX"

    if [[ -f "$PREFIX/bin/$BINARY" ]] && [[ "$KEEP_TARBALL" != true ]]; then
        echo "==> $BINARY already exists at $PREFIX/bin/$BINARY"
        echo "    Re-running will overwrite it."
    fi

    tmp_dir="$(mktemp -d)"
    # tmp_dir is local to main(); the EXIT trap runs after main returns, so
    # reference it with ${tmp_dir:-} to avoid an "unbound variable" error
    # under `set -u` when the trap fires post-return.
    trap 'rm -rf "${tmp_dir:-}"' EXIT

    echo "==> Downloading $tarball_url ..."
    if ! fetch "$tarball_url" "$tmp_dir/$artifact"; then
        echo "Error: failed to download $tarball_url" >&2
        echo "Check that the release exists and the artifact name is correct." >&2
        exit 1
    fi

    # Fail closed on integrity: the checksum file and matching entry are
    # required. If they're missing, refuse to install rather than silently
    # shipping an unverified tarball.
    if ! fetch "$checksum_url" "$tmp_dir/checksums.txt" 2>/dev/null; then
        echo "Error: failed to download checksums.txt from $checksum_url" >&2
        echo "Refusing to install without a checksum. Verify the release is complete." >&2
        exit 1
    fi

    # Fail closed on authenticity: the checksums.txt signature is required and
    # must verify against the pinned Ed25519 public key. This proves the
    # checksums (and therefore the tarball) were produced by the pimon
    # maintainers, not tampered with in transit or on the release host.
    if ! fetch "$signature_url" "$tmp_dir/checksums.txt.sig" 2>/dev/null; then
        echo "Error: failed to download checksums.txt.sig from $signature_url" >&2
        echo "Refusing to install without a release signature." >&2
        exit 1
    fi

    if ! command -v openssl &>/dev/null; then
        echo "Error: openssl is required to verify the release signature" >&2
        exit 1
    fi

    local pubkey_file
    pubkey_file="$tmp_dir/pimon-signing-key.pub"
    printf '%s\n' "$SIGNING_PUBLIC_KEY" > "$pubkey_file"

    if ! openssl pkeyutl -verify -rawin -in "$tmp_dir/checksums.txt" \
        -sigfile "$tmp_dir/checksums.txt.sig" \
        -pubin -inkey "$pubkey_file" >/dev/null 2>&1; then
        echo "Error: release signature verification FAILED for checksums.txt" >&2
        echo "Refusing to install: the release could not be authenticated." >&2
        exit 1
    fi
    echo "==> Signature OK"

    local expected
    # Anchor to the exact artifact name (end of line) so an archive entry
    # cannot shadow the tarball's entry in checksums.txt.
    expected="$(grep -E "^[0-9a-f]{64}  ${artifact}$" "$tmp_dir/checksums.txt" | awk '{print $1}')"
    if [[ -z "$expected" ]]; then
        echo "Error: no checksum entry found for $artifact in checksums.txt" >&2
        echo "Refusing to install an unverified tarball." >&2
        exit 1
    fi

    local actual
    if command -v sha256sum &>/dev/null; then
        actual="$(sha256sum "$tmp_dir/$artifact" | awk '{print $1}')"
    elif command -v shasum &>/dev/null; then
        actual="$(shasum -a 256 "$tmp_dir/$artifact" | awk '{print $1}')"
    else
        echo "Error: neither sha256sum nor shasum is available to verify the download" >&2
        exit 1
    fi

    if [[ "$actual" != "$expected" ]]; then
        echo "Error: checksum mismatch!" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
    echo "==> Checksum OK"

    tar -xzf "$tmp_dir/$artifact" -C "$tmp_dir"
    local stage_dir="$tmp_dir/pimon-${version_no_v}-${triple}"
    if [[ ! -f "$stage_dir/install.sh" ]]; then
        echo "Error: $artifact does not contain the expected pimon-${version_no_v}-${triple}/install.sh" >&2
        exit 1
    fi

    if [[ "$KEEP_TARBALL" == true ]]; then
        cp "$tmp_dir/$artifact" "$PREFIX/$artifact" 2>/dev/null || true
    fi

    PREFIX="$PREFIX" bash "$stage_dir/install.sh"

    if [[ -x "$PREFIX/bin/$BINARY" ]]; then
        # Report the version of what we just installed, not whatever an older
        # PATH lookup would find.
        echo "==> Version: $("$PREFIX/bin/$BINARY" --version 2>/dev/null || echo "unknown")"
    fi

    if [[ ":$PATH:" != *":$PREFIX/bin:"* ]]; then
        echo ""
        echo "Note: $PREFIX/bin is not in your PATH."
        echo "Add it to your shell profile:"
        echo "  export PATH=\"$PREFIX/bin:\$PATH\""
    fi
}

main