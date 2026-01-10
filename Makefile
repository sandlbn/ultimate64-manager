SHELL := /bin/bash

# =========================
# Ultimate64 Manager Unified Makefile
# Builds: Linux (AppImage), macOS (.app zip), Windows (.exe zip)
# =========================

# --- Helpers to read package name/version (prefer cargo metadata + jq) ---
HAVE_JQ := $(shell command -v jq >/dev/null 2>&1 && echo 1 || echo 0)

ifeq ($(HAVE_JQ),1)
NAME    := $(shell cargo metadata --no-deps --format-version=1 | jq -r '.packages[0].name')
VERSION := $(shell cargo metadata --no-deps --format-version=1 | jq -r '.packages[0].version')
else
# Fallback: simple sed (works for basic Cargo.toml layouts)
NAME    := $(shell sed -n 's/^name *= *"\(.*\)"/\1/p' Cargo.toml | head -n1 | tr -d '\r')
VERSION := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1 | tr -d '\r')
endif

# Binary name (default to package name; override if your [[bin]] name differs)
BIN ?= $(NAME)

# --- Common paths/tools ---
CARGO      ?= cargo
RUSTC      ?= rustc
PATCHELF   ?= patchelf

# --- Remap build paths in panic messages/backtraces (helps avoid /home/... in panics) ---
CARGO_HOME_FALLBACK := $(HOME)/.cargo
CARGO_HOME_USED     := $(or $(CARGO_HOME),$(CARGO_HOME_FALLBACK))
SYSROOT             := $(shell $(RUSTC) --print sysroot)

export RUSTFLAGS = \
  --remap-path-prefix=$(HOME)=/home/build \
  --remap-path-prefix=$(SYSROOT)=/rust/sysroot \
  --remap-path-prefix=$(CARGO_HOME_USED)=/cargo \
  --remap-path-prefix=$(CURDIR)=/src

# =========================
# Linux (AppImage)
# =========================
BINNAME    := ultimate64-manager
APPDIR     := $(CURDIR)/AppDir
ICON_PNG   ?= icons/icon.png

LINUXDEPLOY ?= ./linuxdeploy-x86_64.AppImage

# AppImage exclusions (keep host ALSA + OpenSSL)
EXCLUDE_LIBS := \
  --exclude-library 'libasound.so.2' \
  --exclude-library 'libssl.so.3' \
  --exclude-library 'libcrypto.so.3'

# =========================
# macOS + Windows packaging
# =========================
WINDOWS_TARGET := x86_64-pc-windows-msvc

DIST_DIR := dist
LINUX_APPIMAGE := $(DIST_DIR)/$(NAME)-v$(VERSION)-linux-x86_64.AppImage
MAC_ZIP  := $(DIST_DIR)/$(NAME)-v$(VERSION)-macos-app.zip
WIN_ZIP  := $(DIST_DIR)/$(NAME)-v$(VERSION)-windows-x86_64.zip

CARGO_BUNDLE := $(shell command -v cargo-bundle 2>/dev/null)
CARGO_XWIN   := $(shell command -v cargo-xwin 2>/dev/null)

.PHONY: all dist version clean \
        release \
        linux linux-appdir linux-appimage \
        macos windows \
        zip zip-linux zip-macos zip-windows

all: zip

dist:
	@mkdir -p $(DIST_DIR)

version:
	@echo "$(NAME) v$(VERSION)"

# --- Build only (host OS) ---
release:
	$(CARGO) clean
	$(CARGO) build --release

# =========================
# Linux targets
# =========================
linux: linux-appimage

linux-appdir: release
	rm -rf "$(APPDIR)"
	mkdir -p "$(APPDIR)/usr/bin" \
	         "$(APPDIR)/usr/lib" \
	         "$(APPDIR)/usr/share/applications" \
	         "$(APPDIR)/usr/share/icons/hicolor/256x256/apps" \
	         "$(APPDIR)/usr/share"

	# Binary
	install -Dm755 target/release/$(BINNAME) "$(APPDIR)/usr/bin/$(BINNAME)"

	# Icon (optional but recommended)
	@if [ -f "$(ICON_PNG)" ]; then \
	  install -Dm644 "$(ICON_PNG)" "$(APPDIR)/usr/share/icons/hicolor/256x256/apps/$(APP).png"; \
	else \
	  echo "WARN: Icon not found at $(ICON_PNG) (continuing)"; \
	fi

	# Desktop file (generated via printf; avoids Makefile here-doc indentation issues)
	printf '%s\n' \
		'[Desktop Entry]' \
		'Type=Application' \
		'Name=Ultimate64 Manager' \
		'Exec=$(BINNAME)' \
		'Icon=$(APP)' \
		'Categories=Utility;' \
		'Terminal=false' \
	> "$(APPDIR)/usr/share/applications/$(APP).desktop"

	# AppRun (generated via printf)
	printf '%s\n' \
		'#!/bin/sh' \
		'set -eu' \
		'' \
		'APPDIR="$$(dirname "$$(readlink -f "$$0")")"' \
		'BIN="$$APPDIR/usr/bin/$(BINNAME)"' \
		'' \
		'DEBUG="$${U64M_DEBUG:-0}"' \
		'CHOICE="$${U64M_LAUNCH:-}"' \
		'' \
		'try() {' \
		'  name="$$1"; shift' \
		'  if [ "$$DEBUG" = "1" ]; then' \
		'    echo "==> trying: $$name" >&2' \
		'    echo "    $$*" >&2' \
		'  fi' \
		'  "$$@" && exit 0' \
		'  return 0' \
		'}' \
		'' \
		'attempt_1() { env WGPU_POWER_PREF=high "$$BIN" "$$@"; }' \
		'attempt_2() { env WGPU_BACKEND=vulkan WGPU_POWER_PREF=high "$$BIN" "$$@"; }' \
		'attempt_3() { env WGPU_BACKEND=gl     WGPU_POWER_PREF=high "$$BIN" "$$@"; }' \
		'attempt_4() { env WINIT_UNIX_BACKEND=x11 WGPU_POWER_PREF=high "$$BIN" "$$@"; }' \
		'attempt_5() { env WINIT_UNIX_BACKEND=x11 WGPU_BACKEND=vulkan WGPU_POWER_PREF=high "$$BIN" "$$@"; }' \
		'attempt_6() { env WINIT_UNIX_BACKEND=x11 WGPU_BACKEND=gl     WGPU_POWER_PREF=high "$$BIN" "$$@"; }' \
		'' \
		'case "$${CHOICE:-}" in' \
		'  1) attempt_1 "$$@"; exit $$?;;' \
		'  2) attempt_2 "$$@"; exit $$?;;' \
		'  3) attempt_3 "$$@"; exit $$?;;' \
		'  4) attempt_4 "$$@"; exit $$?;;' \
		'  5) attempt_5 "$$@"; exit $$?;;' \
		'  6) attempt_6 "$$@"; exit $$?;;' \
		'  "") ;;' \
		'  *) echo "U64M_LAUNCH must be 1..6" >&2; exit 2;;' \
		'esac' \
		'' \
		'try "default (auto)"     attempt_1 "$$@"' \
		'try "force Vulkan"       attempt_2 "$$@"' \
		'try "force GL"           attempt_3 "$$@"' \
		'try "X11 backend (auto)" attempt_4 "$$@"' \
		'try "X11 + Vulkan"       attempt_5 "$$@"' \
		'try "X11 + GL"           attempt_6 "$$@"' \
		'' \
		'echo "Ultimate64 Manager failed to start with all fallback modes." >&2' \
		'echo "Tip: run with U64M_DEBUG=1 for attempt logs." >&2' \
		'echo "Try forcing: U64M_LAUNCH=4 (X11) or U64M_LAUNCH=6 (X11+GL)." >&2' \
		'exit 1' \
	> "$(APPDIR)/AppRun"
	chmod +x "$(APPDIR)/AppRun"

	# Ensure binary loads bundled libs from AppDir/usr/lib (if any)
	@command -v $(PATCHELF) >/dev/null 2>&1 || (echo "ERROR: patchelf not found" && exit 1)
	$(PATCHELF) --set-rpath '$$ORIGIN/../lib' "$(APPDIR)/usr/bin/$(BINNAME)"

	# Some tools prefer these at AppDir root
	cp "$(APPDIR)/usr/share/applications/$(APP).desktop" "$(APPDIR)/$(APP).desktop"
	@if [ -f "$(APPDIR)/usr/share/icons/hicolor/256x256/apps/$(APP).png" ]; then \
	  cp "$(APPDIR)/usr/share/icons/hicolor/256x256/apps/$(APP).png" "$(APPDIR)/$(APP).png"; \
	fi

linux-appimage: linux-appdir dist
	@if [ ! -x "$(LINUXDEPLOY)" ]; then \
	  echo "ERROR: linuxdeploy not found/executable at: $(LINUXDEPLOY)"; \
	  echo "Tip: copy linuxdeploy-x86_64.AppImage into repo root, chmod +x it, or run:"; \
	  echo "  make linux-appimage LINUXDEPLOY=/full/path/to/linuxdeploy-x86_64.AppImage"; \
	  exit 1; \
	fi
	NO_STRIP=1 "$(LINUXDEPLOY)" --appdir "$(APPDIR)" $(EXCLUDE_LIBS) --output appimage
	@# linuxdeploy names output; find newest .AppImage and copy to dist with stable name
	@NEW=$$(ls -1t *.AppImage 2>/dev/null | head -n1); \
	  if [ -z "$$NEW" ]; then echo "ERROR: No .AppImage produced"; exit 1; fi; \
	  echo "Created $$NEW"; \
	  cp -f "$$NEW" "$(LINUX_APPIMAGE)"; \
	  echo "Copied -> $(LINUX_APPIMAGE)"

zip-linux: linux-appimage
	@echo "Linux AppImage in: $(LINUX_APPIMAGE)"

# =========================
# macOS targets
# =========================
macos:
	@if [ -z "$(CARGO_BUNDLE)" ]; then \
	  echo "ERROR: cargo-bundle not found. Install: cargo install cargo-bundle"; \
	  exit 1; \
	fi
	cargo bundle --release

zip-macos: macos dist
	@BUNDLE_DIR="target/release/bundle/osx"; \
	if [ ! -d "$$BUNDLE_DIR" ]; then \
	  echo "ERROR: Bundle dir not found: $$BUNDLE_DIR"; exit 1; \
	fi; \
	APP_CANDIDATE=""; \
	if [ -d "$$BUNDLE_DIR/$(BIN).app" ]; then \
	  APP_CANDIDATE="$$BUNDLE_DIR/$(BIN).app"; \
	elif [ -d "$$BUNDLE_DIR/$(NAME).app" ]; then \
	  APP_CANDIDATE="$$BUNDLE_DIR/$(NAME).app"; \
	else \
	  APP_CANDIDATE=$$(ls -1 "$$BUNDLE_DIR"/*.app 2>/dev/null | head -n1); \
	fi; \
	if [ -z "$$APP_CANDIDATE" ]; then \
	  echo "ERROR: No .app found in $$BUNDLE_DIR"; \
	  echo "Contents:"; ls -la "$$BUNDLE_DIR"; exit 1; \
	fi; \
	echo "Zipping macOS app -> $(MAC_ZIP)"; \
	ditto -c -k --sequesterRsrc --keepParent "$$APP_CANDIDATE" "$(MAC_ZIP)"

# =========================
# Windows targets
# =========================
windows:
	@if [ -z "$(CARGO_XWIN)" ]; then \
	  echo "ERROR: cargo-xwin not found. Install: cargo install cargo-xwin"; \
	  exit 1; \
	fi
	rustup target add $(WINDOWS_TARGET) >/dev/null 2>&1 || true
	cargo xwin build --release --target $(WINDOWS_TARGET)

zip-windows: windows dist
	@WIN_DIR="target/$(WINDOWS_TARGET)/release"; \
	if [ ! -d "$$WIN_DIR" ]; then \
	  echo "ERROR: Windows release dir not found: $$WIN_DIR"; exit 1; \
	fi; \
	EXE=""; \
	if [ -f "$$WIN_DIR/$(BIN).exe" ]; then \
	  EXE="$$WIN_DIR/$(BIN).exe"; \
	elif [ -f "$$WIN_DIR/$(NAME).exe" ]; then \
	  EXE="$$WIN_DIR/$(NAME).exe"; \
	else \
	  EXE=$$(ls -1 "$$WIN_DIR"/*.exe 2>/dev/null | head -n1); \
	fi; \
	if [ -z "$$EXE" ]; then \
	  echo "ERROR: No .exe found in $$WIN_DIR"; \
	  echo "Contents:"; ls -la "$$WIN_DIR"; exit 1; \
	fi; \
	echo "Zipping Windows binary ($$EXE) -> $(WIN_ZIP)"; \
	(cd "$$WIN_DIR" && zip -9 -j ../../../$(WIN_ZIP) "$$(basename "$$EXE")")

# --- Packaging ---
zip: dist zip-linux zip-macos zip-windows
	@echo "Created:"
	@echo "  $(LINUX_APPIMAGE)"
	@echo "  $(MAC_ZIP)"
	@echo "  $(WIN_ZIP)"

clean:
	cargo clean
	@rm -rf "$(DIST_DIR)" "$(APPDIR)" squashfs-root *.AppImage
