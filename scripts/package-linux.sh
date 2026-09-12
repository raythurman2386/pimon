#!/usr/bin/env bash
# Build a Linux x86_64 release tarball: binary, desktop entry, icon, install script.
set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}"
ARCH="${ARCH:-x86_64}"
TARGET="${TARGET:-${ARCH}-unknown-linux-gnu}"
# target-dir may be overridden (e.g. shared cache in ~/.cargo/config.toml),
# so ask cargo where the build landed; fall back for cargo-less setups that
# pass PREBUILT_BIN.
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" 2>/dev/null | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' || true)"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"
# With an explicit TARGET, cargo build --target places the binary under
# <target-dir>/<triple>/release/; only the host-native build lands directly
# in <target-dir>/release/.
if [[ -n "${TARGET:-}" && -x "$TARGET_DIR/$TARGET/release/pimon" ]]; then
  BIN="${PREBUILT_BIN:-$TARGET_DIR/$TARGET/release/pimon}"
else
  BIN="${PREBUILT_BIN:-$TARGET_DIR/release/pimon}"
fi
OUT_DIR="${OUT_DIR:-$ROOT/target/package}"
NAME="pimon-${VERSION}-${TARGET}"
STAGE="$OUT_DIR/$NAME"

if [[ ! -x "$BIN" ]]; then
  echo "package-linux: missing binary at $BIN (build --release first)" >&2
  exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"

install -m 755 "$BIN" "$STAGE/pimon"
install -m 644 "$ROOT/dist/pimon.desktop" "$STAGE/pimon.desktop"
install -m 644 "$ROOT/dist/pimon.svg" "$STAGE/pimon.svg"
install -m 644 "$ROOT/LICENSE" "$STAGE/LICENSE"
install -m 644 "$ROOT/fonts/OFL.txt" "$STAGE/OFL.txt"

cat >"$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install a prebuilt Pimon release into ~/.local. No root, no compiler.
set -euo pipefail
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"

install -Dm755 "$HERE/pimon" "$PREFIX/bin/pimon"
install -Dm644 "$HERE/pimon.desktop" "$PREFIX/share/applications/pimon.desktop"
install -Dm644 "$HERE/pimon.svg" "$PREFIX/share/icons/hicolor/scalable/apps/pimon.svg"
install -Dm644 "$HERE/LICENSE" "$PREFIX/share/licenses/pimon/LICENSE"
install -Dm644 "$HERE/OFL.txt" "$PREFIX/share/licenses/pimon/OFL.txt"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed pimon to $PREFIX/bin/pimon"
INSTALL
chmod 755 "$STAGE/install.sh"

mkdir -p "$OUT_DIR"
TARBALL="$OUT_DIR/${NAME}.tar.gz"
tar -czf "$TARBALL" -C "$OUT_DIR" "$NAME"
echo "packaged: $TARBALL"