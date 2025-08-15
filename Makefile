SHELL := /bin/bash

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

# Targets
WINDOWS_TARGET := x86_64-pc-windows-msvc

# Artifacts
DIST_DIR := dist
MAC_ZIP  := $(DIST_DIR)/$(NAME)-v$(VERSION)-macos-app.zip
WIN_ZIP  := $(DIST_DIR)/$(NAME)-v$(VERSION)-windows-x86_64.zip

# Tool checks
CARGO_BUNDLE := $(shell command -v cargo-bundle 2>/dev/null)
CARGO_XWIN   := $(shell command -v cargo-xwin 2>/dev/null)

.PHONY: all macos windows zip zip-macos zip-windows dist clean version

all: zip

# --- Build steps ---
macos:
	@if [ -z "$(CARGO_BUNDLE)" ]; then \
	  echo "ERROR: cargo-bundle not found. Install: cargo install cargo-bundle"; \
	  exit 1; \
	fi
	cargo bundle --release

windows:
	@if [ -z "$(CARGO_XWIN)" ]; then \
	  echo "ERROR: cargo-xwin not found. Install: cargo install cargo-xwin"; \
	  exit 1; \
	fi
	rustup target add $(WINDOWS_TARGET) >/dev/null 2>&1 || true
	cargo xwin build --release --target $(WINDOWS_TARGET)

# --- Packaging ---
zip: dist zip-macos zip-windows
	@echo "Created:"
	@echo "  $(MAC_ZIP)"
	@echo "  $(WIN_ZIP)"

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


dist:
	@mkdir -p $(DIST_DIR)

# --- Utilities ---
version:
	@echo "$(NAME) v$(VERSION)"

clean:
	cargo clean
	@rm -rf "$(DIST_DIR)"
