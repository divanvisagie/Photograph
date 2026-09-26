APP_NAME := photograph
VERSION := $(shell awk -F\" '/^version = / { print $$2; exit }' Cargo.toml)
DEB_REVISION ?= 1
DEB_VERSION := $(VERSION)-$(DEB_REVISION)
UNAME_S := $(shell uname -s)
ARCH := $(shell dpkg --print-architecture 2>/dev/null || echo amd64)
ICON_SOURCE_SVG := packaging/linux/$(APP_NAME).svg
RUNTIME_ICON_PNG := assets/$(APP_NAME)-icon-128.png

ifeq ($(UNAME_S),Linux)
PLATFORM := linux
else
PLATFORM := unsupported
endif

DEB_DIR := target/deb
PKG_ROOT := $(DEB_DIR)/$(APP_NAME)_$(DEB_VERSION)_$(ARCH)
DEB_PATH := $(DEB_DIR)/$(APP_NAME)_$(DEB_VERSION)_$(ARCH).deb
LINUX_DESKTOP_SRC := packaging/linux/$(APP_NAME).desktop
LINUX_ICON_SRC := packaging/linux/$(APP_NAME).svg
LINUX_DESKTOP_DST := $(PKG_ROOT)/usr/share/applications/$(APP_NAME).desktop
LINUX_ICON_DST := $(PKG_ROOT)/usr/share/icons/hicolor/scalable/apps/$(APP_NAME).svg

ICON_TMP_DIR := target/icons

.DEFAULT_GOAL := help

.PHONY: help dev build build-linux build-deb build-unsupported install install-linux install-unsupported clean-deb clean-icons icons icon-runtime release docs

help: ## Show this help
	@echo "Usage: make <target>"
	@echo
	@awk 'BEGIN { FS = ":.*## " } /^[a-z-]+:.*## / { printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

dev: ## Run with live reload (requires cargo-watch)
	@command -v cargo-watch >/dev/null 2>&1 || { echo "cargo-watch is required: cargo install cargo-watch"; exit 1; }
	RUST_LOG=photograph=debug cargo watch -x "run --bin photograph"

build: build-$(PLATFORM) ## Build the .deb

install: install-$(PLATFORM) ## Build and install the .deb via apt

icons: icon-runtime ## Regenerate the runtime icon PNG from the SVG

icon-runtime:
	@test -f "$(ICON_SOURCE_SVG)" || { echo "missing icon source: $(ICON_SOURCE_SVG)"; exit 1; }
	@mkdir -p "$(dir $(RUNTIME_ICON_PNG))"
	@set -e; \
	render_png() { \
		size="$$1"; dest="$$2"; \
		if command -v rsvg-convert >/dev/null 2>&1; then \
			rsvg-convert -w "$$size" -h "$$size" "$(ICON_SOURCE_SVG)" -o "$$dest"; \
		elif command -v inkscape >/dev/null 2>&1; then \
			inkscape "$(ICON_SOURCE_SVG)" -w "$$size" -h "$$size" --export-filename="$$dest" >/dev/null; \
		elif command -v magick >/dev/null 2>&1; then \
			magick -background none "$(ICON_SOURCE_SVG)" -resize "$${size}x$${size}" "$$dest"; \
		else \
			echo "need rsvg-convert, inkscape, or magick to rasterize $(ICON_SOURCE_SVG)"; \
			exit 1; \
		fi; \
	}; \
	render_png 128 "$(RUNTIME_ICON_PNG)"
	@echo "Generated runtime icon: $(RUNTIME_ICON_PNG)"

build-linux: build-deb

build-deb: ## Build the .deb into target/deb/
	@command -v dpkg-deb >/dev/null 2>&1 || { echo "dpkg-deb is required (install dpkg-dev)."; exit 1; }
	@test -f "$(LINUX_DESKTOP_SRC)" || { echo "missing launcher file: $(LINUX_DESKTOP_SRC)"; exit 1; }
	@test -f "$(LINUX_ICON_SRC)" || { echo "missing icon file: $(LINUX_ICON_SRC)"; exit 1; }
	cargo build --release --bin $(APP_NAME)
	rm -rf "$(PKG_ROOT)"
	mkdir -p \
		"$(PKG_ROOT)/DEBIAN" \
		"$(PKG_ROOT)/usr/bin" \
		"$(PKG_ROOT)/usr/share/applications" \
		"$(PKG_ROOT)/usr/share/icons/hicolor/scalable/apps" \
		"$(DEB_DIR)"
	install -m 755 "target/release/$(APP_NAME)" "$(PKG_ROOT)/usr/bin/$(APP_NAME)"
	install -m 644 "$(LINUX_DESKTOP_SRC)" "$(LINUX_DESKTOP_DST)"
	install -m 644 "$(LINUX_ICON_SRC)" "$(LINUX_ICON_DST)"
	printf '%s\n' \
		"Package: $(APP_NAME)" \
		"Version: $(DEB_VERSION)" \
		"Section: graphics" \
		"Priority: optional" \
		"Architecture: $(ARCH)" \
		"Maintainer: Divan Visagie <me@divanv.com>" \
		"Depends: libc6, libgcc-s1, libvulkan1" \
		"Description: Photograph native photo editor" \
		" Native Rust/egui photo editor with preview and export workflows." \
		> "$(PKG_ROOT)/DEBIAN/control"
	dpkg-deb --build --root-owner-group "$(PKG_ROOT)" "$(DEB_PATH)"
	@echo "Built package: $(DEB_PATH)"
	@echo "Install with: sudo apt install ./$(DEB_PATH)"

install-linux: build-deb
	sudo apt install --reinstall -y "./$(DEB_PATH)"

clean-deb: ## Remove built .deb artifacts
	rm -rf "$(DEB_DIR)"

release: build-deb ## Build the .deb and publish a GitHub release (requires gh)
	@command -v gh >/dev/null 2>&1 || { echo "gh CLI is required: https://cli.github.com"; exit 1; }
	gh release create "v$(VERSION)" "$(DEB_PATH)" --title "v$(VERSION)" --generate-notes
	@echo "Created GitHub release v$(VERSION) with $(DEB_PATH)"

build-unsupported:
	@echo "Unsupported platform: $(UNAME_S). Photograph is Linux-only (see docs/adr/0012-drop-macos-support-linux-only.md)."
	@exit 1

install-unsupported:
	@echo "Unsupported platform: $(UNAME_S). Photograph is Linux-only (see docs/adr/0012-drop-macos-support-linux-only.md)."
	@exit 1

clean-icons: ## Remove temporary icon build files
	rm -rf "$(ICON_TMP_DIR)"

docs: ## Serve the docs site at http://localhost:8000
	@command -v python3 >/dev/null 2>&1 || { echo "python3 is required"; exit 1; }
	@echo "Serving docs at http://localhost:8000"
	@cd docs && python3 -m http.server 8000
