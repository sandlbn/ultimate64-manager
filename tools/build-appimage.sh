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

echo "==> bundling shared libraries (ldd)"
mkdir -p AppDir/usr/lib

# Libraries that MUST come from the host system, not bundled. Bundling these
# either breaks ABI compat with the host's kernel/glibc (the glibc family,
# gcc runtime) or breaks rendering (the GL/X11 base — bundled copies
# wouldn't match the user's installed Mesa/Nvidia driver).
EXCLUDE_LIBS="
    ld-linux.so.2 ld-linux-x86-64.so.2
    libc.so.6 libdl.so.2 libm.so.6 libpthread.so.0 librt.so.1
    libutil.so.1 libnsl.so.1 libresolv.so.2 libcrypt.so.1
    libgcc_s.so.1 libstdc++.so.6
    libGL.so.1 libEGL.so.1 libGLX.so.0 libGLdispatch.so.0 libOpenGL.so.0
    libdrm.so.2 libgbm.so.1
    libX11.so.6 libxcb.so.1 libXext.so.6
"

bundled_count=0
while IFS= read -r line; do
    libname=$(awk '{print $1}' <<<"$line")
    libpath=$(awk '{print $3}' <<<"$line")
    [[ -z "$libpath" || ! -f "$libpath" ]] && continue
    case " $EXCLUDE_LIBS " in
        *" $libname "*) continue ;;
    esac
    cp -L "$libpath" AppDir/usr/lib/
    bundled_count=$((bundled_count + 1))
done < <(ldd AppDir/usr/bin/ultimate64-manager | grep '=>')

echo "  bundled $bundled_count libraries"

cp ultimate64-manager.desktop AppDir/
cp ultimate64-manager.desktop AppDir/usr/share/applications/
cp assets/icon.png AppDir/ultimate64-manager.png
cp assets/icon.png AppDir/usr/share/icons/hicolor/512x512/apps/ultimate64-manager.png
ln -sf ultimate64-manager.png AppDir/.DirIcon

cat > AppDir/AppRun <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "${0}")")"
export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib:${LD_LIBRARY_PATH}"
exec "${HERE}/usr/bin/ultimate64-manager" "$@"
APPRUN
chmod +x AppDir/AppRun

echo "==> mksquashfs payload"
PAYLOAD=$(mktemp --suffix=.squashfs)
trap 'rm -f "$PAYLOAD"' EXIT
# NOTE: -comp must be zstd or gzip — the AppImage type-2 runtime
# bundles only those two decompressors. xz produces a smaller payload
# but the runtime errors out with "this version supports only zlib,
# zstd" when users try to launch it.
mksquashfs AppDir "$PAYLOAD" \
    -root-owned -noappend -comp zstd -Xcompression-level 19 -no-progress >/dev/null

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
