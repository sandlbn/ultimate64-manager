#!/bin/bash
# Build the Ultimate64 Manager AppImage from raw parts.
#
# Designed to run inside the Ubuntu 20.04 build container (see
# Dockerfile.linux-build). Uses only `cargo`, `strip`, `mksquashfs`,
# and the pre-baked AppImage runtime at /opt/appimage-runtime — no
# linuxdeploy, no appimagetool, no AppImage execution required.
#
# Usage (from container): tools/build-appimage.sh <output-path>
#
# The `glibc-floor` summary at the end is the highest GLIBC_x.y symbol
# the binary references; the produced AppImage will run on any distro
# whose libc.so.6 exports at least that version.

set -euo pipefail

OUT="${1:?usage: build-appimage.sh <output-appimage-path>}"
RUNTIME="${APPIMAGE_RUNTIME:-/opt/appimage-runtime}"

if [[ ! -x "$RUNTIME" ]]; then
    echo "ERROR: AppImage runtime not found at $RUNTIME" >&2
    exit 1
fi

echo "==> cargo build --release"
cargo build --release

BIN="${CARGO_TARGET_DIR:-target}/release/ultimate64-manager"
if [[ ! -x "$BIN" ]]; then
    echo "ERROR: built binary not found at $BIN" >&2
    exit 1
fi

echo "==> assembling AppDir"
rm -rf AppDir
mkdir -p AppDir/usr/bin \
         AppDir/usr/share/applications \
         AppDir/usr/share/icons/hicolor/512x512/apps

cp "$BIN" AppDir/usr/bin/ultimate64-manager
strip AppDir/usr/bin/ultimate64-manager || true

cp ultimate64-manager.desktop AppDir/
cp ultimate64-manager.desktop AppDir/usr/share/applications/
cp assets/icon.png AppDir/ultimate64-manager.png
cp assets/icon.png AppDir/usr/share/icons/hicolor/512x512/apps/ultimate64-manager.png
ln -sf ultimate64-manager.png AppDir/.DirIcon

cat > AppDir/AppRun <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "${0}")")"
export PATH="${HERE}/usr/bin:${PATH}"
exec "${HERE}/usr/bin/ultimate64-manager" "$@"
APPRUN
chmod +x AppDir/AppRun

echo "==> mksquashfs payload"
PAYLOAD=$(mktemp --suffix=.squashfs)
trap 'rm -f "$PAYLOAD"' EXIT
mksquashfs AppDir "$PAYLOAD" \
    -root-owned -noappend -comp xz -no-progress >/dev/null

echo "==> writing $OUT"
mkdir -p "$(dirname "$OUT")"
cat "$RUNTIME" "$PAYLOAD" > "$OUT"
chmod +x "$OUT"

rm -rf AppDir

SIZE=$(du -h "$OUT" | cut -f1)
echo ""
echo "Created: $OUT ($SIZE)"
echo ""
echo "GLIBC symbols required by the binary:"
objdump -T "$BIN" | grep -oP 'GLIBC_[0-9]+\.[0-9]+' | sort -u | sed 's/^/  /'
