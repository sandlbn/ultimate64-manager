SHELL := /bin/bash

# --- Package info from Cargo.toml ---
HAVE_JQ := $(shell command -v jq >/dev/null 2>&1 && echo 1 || echo 0)
ifeq ($(HAVE_JQ),1)
VERSION := $(shell cargo metadata --no-deps --format-version=1 | jq -r '.packages[0].version')
else
VERSION := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1 | tr -d '\r')
endif

# --- Output names (matching GitHub releases) ---
APP_NAME     := Ultimate64Manager
DIST_DIR     := dist

LINUX_OUT    := $(DIST_DIR)/$(APP_NAME)-Linux.AppImage
MACOS_OUT    := $(DIST_DIR)/$(APP_NAME)-MacOS.zip
WINDOWS_OUT  := $(DIST_DIR)/$(APP_NAME)-Win.exe

# --- Targets ---
WINDOWS_TARGET := x86_64-pc-windows-msvc

.PHONY: all linux macos macos_dist windows dist clean version help

all: help

help:
	@echo "Ultimate64 Manager v$(VERSION) - Build targets:"
	@echo ""
	@echo "  make linux      - Build Linux AppImage"
	@echo "  make macos      - Build macOS app bundle (unsigned)"
	@echo "  make macos_dist - Build, codesign & notarize macOS app"
	@echo "  make windows    - Build Windows executable"
	@echo "  make clean      - Remove build artifacts"
	@echo "  make version    - Show version"
	@echo ""
	@echo "Output files:"
	@echo "  $(LINUX_OUT)"
	@echo "  $(MACOS_OUT)"
	@echo "  $(WINDOWS_OUT)"
	@echo ""
	@echo "macOS signing (macos_dist) requires env vars:"
	@echo "  APPLE_DEVELOPER_ID  - Developer ID Application: Name (TEAMID)"
	@echo "  APPLE_ID_EMAIL      - Apple ID email"
	@echo "  APPLE_TEAM_ID       - Team ID"
	@echo "  APPLE_APP_PASSWORD  - App-specific password"

# --- Linux AppImage ---
linux: dist
	@echo "Building Linux AppImage..."
	cargo build --release
	@# Check for linuxdeploy or appimagetool
	@if command -v linuxdeploy >/dev/null 2>&1; then \
		echo "Creating AppImage with linuxdeploy..."; \
		linuxdeploy --appdir=AppDir \
			--executable=target/release/ultimate64-manager \
			--desktop-file=ultimate64-manager.desktop \
			--icon-file=assets/icon.png \
			--output=appimage; \
		mv Ultimate64_Manager*.AppImage "$(LINUX_OUT)"; \
	else \
		echo "WARNING: linuxdeploy not found, copying binary instead"; \
		cp target/release/ultimate64-manager "$(LINUX_OUT)"; \
	fi
	@echo "Created: $(LINUX_OUT)"

# --- macOS App Bundle (unsigned) ---
macos: dist
	@if ! command -v cargo-bundle >/dev/null 2>&1; then \
		echo "ERROR: cargo-bundle not found. Install: cargo install cargo-bundle"; \
		exit 1; \
	fi
	@echo "Building macOS app bundle..."
	cargo bundle --release
	@BUNDLE=$$(find target/release/bundle/osx -name "*.app" -type d | head -n1); \
	if [ -z "$$BUNDLE" ]; then \
		echo "ERROR: No .app bundle found"; exit 1; \
	fi; \
	echo "Zipping $$BUNDLE -> $(MACOS_OUT)"; \
	ditto -c -k --sequesterRsrc --keepParent "$$BUNDLE" "$(MACOS_OUT)"
	@echo "Created: $(MACOS_OUT)"

# --- macOS App Bundle (signed & notarized) ---
# Required environment variables:
#   APPLE_DEVELOPER_ID  - "Developer ID Application: Your Name (TEAMID)"
#   APPLE_ID_EMAIL      - your Apple ID email
#   APPLE_TEAM_ID       - your Team ID
#   APPLE_APP_PASSWORD  - app-specific password from appleid.apple.com
macos_dist: dist
	@if [ -z "$(APPLE_DEVELOPER_ID)" ]; then \
		echo "ERROR: APPLE_DEVELOPER_ID not set"; \
		echo "  Export: export APPLE_DEVELOPER_ID='Developer ID Application: Your Name (TEAMID)'"; \
		exit 1; \
	fi
	@if [ -z "$(APPLE_ID_EMAIL)" ]; then \
		echo "ERROR: APPLE_ID_EMAIL not set"; exit 1; \
	fi
	@if [ -z "$(APPLE_TEAM_ID)" ]; then \
		echo "ERROR: APPLE_TEAM_ID not set"; exit 1; \
	fi
	@if [ -z "$(APPLE_APP_PASSWORD)" ]; then \
		echo "ERROR: APPLE_APP_PASSWORD not set"; \
		echo "  Create app-specific password at appleid.apple.com"; \
		exit 1; \
	fi
	@if ! command -v cargo-bundle >/dev/null 2>&1; then \
		echo "ERROR: cargo-bundle not found. Install: cargo install cargo-bundle"; \
		exit 1; \
	fi
	@echo "=== Building macOS app bundle ==="
	cargo bundle --release
	@BUNDLE=$$(find target/release/bundle/osx -name "*.app" -type d | head -n1); \
	if [ -z "$$BUNDLE" ]; then \
		echo "ERROR: No .app bundle found"; exit 1; \
	fi; \
	echo "=== Codesigning $$BUNDLE ==="; \
	codesign --force --deep --options runtime \
		--sign "$(APPLE_DEVELOPER_ID)" \
		"$$BUNDLE"; \
	echo "=== Creating zip for notarization ==="; \
	ditto -c -k --keepParent "$$BUNDLE" "$(MACOS_OUT)"; \
	echo "=== Submitting to Apple for notarization ==="; \
	xcrun notarytool submit "$(MACOS_OUT)" \
		--apple-id "$(APPLE_ID_EMAIL)" \
		--team-id "$(APPLE_TEAM_ID)" \
		--password "$(APPLE_APP_PASSWORD)" \
		--wait; \
	echo "=== Stapling notarization ticket ==="; \
	xcrun stapler staple "$$BUNDLE"; \
	echo "=== Re-creating zip with stapled app ==="; \
	rm -f "$(MACOS_OUT)"; \
	ditto -c -k --keepParent "$$BUNDLE" "$(MACOS_OUT)"
	@echo ""
	@echo "=== Done! ==="
	@echo "Created: $(MACOS_OUT) (signed & notarized)"

# --- Windows Executable ---
windows: dist
	@if ! command -v cargo-xwin >/dev/null 2>&1; then \
		echo "ERROR: cargo-xwin not found. Install: cargo install cargo-xwin"; \
		exit 1; \
	fi
	@echo "Building Windows executable..."
	rustup target add $(WINDOWS_TARGET) >/dev/null 2>&1 || true
	cargo xwin build --release --target $(WINDOWS_TARGET)
	@EXE=$$(find target/$(WINDOWS_TARGET)/release -maxdepth 1 -name "*.exe" | head -n1); \
	if [ -z "$$EXE" ]; then \
		echo "ERROR: No .exe found"; exit 1; \
	fi; \
	cp "$$EXE" "$(WINDOWS_OUT)"
	@echo "Created: $(WINDOWS_OUT)"

# --- Utilities ---
dist:
	@mkdir -p $(DIST_DIR)

version:
	@echo "$(APP_NAME) v$(VERSION)"

clean:
	cargo clean
	rm -rf $(DIST_DIR)