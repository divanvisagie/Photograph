APP_NAME := photograph
VERSION := $(shell awk -F\" '/^version = / { print $$2; exit }' Cargo.toml)
DEB_REVISION ?= 1
DEB_VERSION := $(VERSION)-$(DEB_REVISION)
UNAME_S := $(shell uname -s)
ARCH := $(shell dpkg --print-architecture 2>/dev/null || echo amd64)
MACOS_APP_NAME := Photograph
MACOS_BUNDLE_ID ?= com.divanv.photograph
ICON_SOURCE_SVG := packaging/linux/$(APP_NAME).svg
RUNTIME_ICON_PNG := assets/$(APP_NAME)-icon-128.png
MACOS_RUNTIME_ICON_PNG := assets/$(APP_NAME)-icon-macos-128.png

ifeq ($(UNAME_S),Linux)
PLATFORM := linux
else ifeq ($(UNAME_S),Darwin)
PLATFORM := macos
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

MACOS_STAGING_DIR := target/macos
MACOS_APP_DIR := $(MACOS_STAGING_DIR)/$(MACOS_APP_NAME).app
MACOS_CONTENTS_DIR := $(MACOS_APP_DIR)/Contents
MACOS_BIN_DIR := $(MACOS_CONTENTS_DIR)/MacOS
MACOS_RESOURCES_DIR := $(MACOS_CONTENTS_DIR)/Resources
MACOS_INFO_PLIST_TEMPLATE := packaging/macos/Info.plist.in
MACOS_INFO_PLIST := $(MACOS_CONTENTS_DIR)/Info.plist
MACOS_ICON_SRC := packaging/macos/$(APP_NAME).icns
MACOS_ICON_NAME := $(APP_NAME).icns
MACOS_ICON_DST := $(MACOS_RESOURCES_DIR)/$(MACOS_ICON_NAME)
MACOS_DMG_PATH := $(MACOS_STAGING_DIR)/$(MACOS_APP_NAME)-$(VERSION).dmg
MACOS_INSTALL_DIR ?= /Applications
ICON_TMP_DIR := target/icons
MACOS_ICONSET_DIR := $(ICON_TMP_DIR)/$(APP_NAME).iconset
MACOS_ICON_BG_TEMPLATE := packaging/macos/icon-background.svg.in
MACOS_ICON_COMPOSITE := $(ICON_TMP_DIR)/$(APP_NAME)-macos.svg

.PHONY: dev build build-linux build-macos package-macos build-unsupported install install-linux install-macos install-unsupported clean-deb clean-macos clean-icons icons icon-runtime icon-macos

dev:
	@command -v cargo-watch >/dev/null 2>&1 || { echo "cargo-watch is required: cargo install cargo-watch"; exit 1; }
	RUST_LOG=photograph=debug cargo watch -x "run --bin photograph"

build: build-$(PLATFORM)

install: install-$(PLATFORM)

icons: icon-runtime
ifeq ($(UNAME_S),Darwin)
icons: icon-macos
else
	@echo "macOS icon generation skipped on $(UNAME_S) (run 'make icon-macos' on macOS)."
endif

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

icon-macos:
	@test "$(UNAME_S)" = "Darwin" || { echo "icon-macos must run on macOS (Darwin)."; exit 1; }
	@command -v iconutil >/dev/null 2>&1 || { echo "iconutil is required on macOS."; exit 1; }
	@test -f "$(ICON_SOURCE_SVG)" || { echo "missing icon source: $(ICON_SOURCE_SVG)"; exit 1; }
	@test -f "$(MACOS_ICON_BG_TEMPLATE)" || { echo "missing template: $(MACOS_ICON_BG_TEMPLATE)"; exit 1; }
	@rm -rf "$(MACOS_ICONSET_DIR)"
	@mkdir -p "$(MACOS_ICONSET_DIR)" "$(dir $(MACOS_ICON_SRC))"
	@set -e; \
	sed '/@SOURCE_SVG@/,$$d' "$(MACOS_ICON_BG_TEMPLATE)" > "$(MACOS_ICON_COMPOSITE)"; \
	sed -e '1{/<?xml/d;}' -e '/<svg /d' -e '/<\/svg>/d' "$(ICON_SOURCE_SVG)" >> "$(MACOS_ICON_COMPOSITE)"; \
	sed '1,/@SOURCE_SVG@/d' "$(MACOS_ICON_BG_TEMPLATE)" >> "$(MACOS_ICON_COMPOSITE)"; \
	echo "Generated composite SVG: $(MACOS_ICON_COMPOSITE)"; \
	render_png() { \
		size="$$1"; dest="$$2"; \
		if command -v rsvg-convert >/dev/null 2>&1; then \
			rsvg-convert -w "$$size" -h "$$size" "$(MACOS_ICON_COMPOSITE)" -o "$$dest"; \
		elif command -v inkscape >/dev/null 2>&1; then \
			inkscape "$(MACOS_ICON_COMPOSITE)" -w "$$size" -h "$$size" --export-filename="$$dest" >/dev/null; \
		elif command -v magick >/dev/null 2>&1; then \
			magick -background none "$(MACOS_ICON_COMPOSITE)" -resize "$${size}x$${size}" "$$dest"; \
		else \
			echo "need rsvg-convert, inkscape, or magick to rasterize SVG"; \
			exit 1; \
		fi; \
	}; \
	render_png 128 "$(MACOS_RUNTIME_ICON_PNG)"; \
	render_png 16 "$(MACOS_ICONSET_DIR)/icon_16x16.png"; \
	render_png 32 "$(MACOS_ICONSET_DIR)/icon_16x16@2x.png"; \
	render_png 32 "$(MACOS_ICONSET_DIR)/icon_32x32.png"; \
	render_png 64 "$(MACOS_ICONSET_DIR)/icon_32x32@2x.png"; \
	render_png 128 "$(MACOS_ICONSET_DIR)/icon_128x128.png"; \
	render_png 256 "$(MACOS_ICONSET_DIR)/icon_128x128@2x.png"; \
	render_png 256 "$(MACOS_ICONSET_DIR)/icon_256x256.png"; \
	render_png 512 "$(MACOS_ICONSET_DIR)/icon_256x256@2x.png"; \
	render_png 512 "$(MACOS_ICONSET_DIR)/icon_512x512.png"; \
	render_png 1024 "$(MACOS_ICONSET_DIR)/icon_512x512@2x.png"; \
	iconutil -c icns "$(MACOS_ICONSET_DIR)" -o "$(MACOS_ICON_SRC)"
	@echo "Generated macOS icon: $(MACOS_ICON_SRC)"

build-linux: icon-runtime
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
		"Depends: libc6, libgcc-s1" \
		"Description: Photograph native photo editor" \
		" Native Rust/egui photo editor with preview and export workflows." \
		> "$(PKG_ROOT)/DEBIAN/control"
	dpkg-deb --build --root-owner-group "$(PKG_ROOT)" "$(DEB_PATH)"
	@echo "Built package: $(DEB_PATH)"
	@echo "Install with: sudo apt install ./$(DEB_PATH)"

install-linux: build-linux
	sudo apt install --reinstall -y "./$(DEB_PATH)"

clean-deb:
	rm -rf "$(DEB_DIR)"

build-macos: icon-runtime icon-macos
	@test "$(UNAME_S)" = "Darwin" || { echo "build-macos must run on macOS (Darwin)."; exit 1; }
	@test -f "$(MACOS_INFO_PLIST_TEMPLATE)" || { echo "missing plist template: $(MACOS_INFO_PLIST_TEMPLATE)"; exit 1; }
	cargo build --release --bin $(APP_NAME)
	rm -rf "$(MACOS_APP_DIR)"
	mkdir -p "$(MACOS_BIN_DIR)" "$(MACOS_RESOURCES_DIR)"
	install -m 755 "target/release/$(APP_NAME)" "$(MACOS_BIN_DIR)/$(APP_NAME)"
	sed \
		-e 's|@APP_NAME@|$(MACOS_APP_NAME)|g' \
		-e 's|@EXECUTABLE@|$(APP_NAME)|g' \
		-e 's|@BUNDLE_ID@|$(MACOS_BUNDLE_ID)|g' \
		-e 's|@VERSION@|$(VERSION)|g' \
		-e 's|@ICON_FILE@|$(MACOS_ICON_NAME)|g' \
		"$(MACOS_INFO_PLIST_TEMPLATE)" > "$(MACOS_INFO_PLIST)"
	@if [ -f "$(MACOS_ICON_SRC)" ]; then \
		install -m 644 "$(MACOS_ICON_SRC)" "$(MACOS_ICON_DST)"; \
	else \
		echo "warning: missing optional macOS icon $(MACOS_ICON_SRC) (bundle will use default app icon)"; \
	fi
	@echo "Built macOS app bundle: $(MACOS_APP_DIR)"
	@echo "Optional DMG target: make package-macos"

package-macos: build-macos
	@command -v hdiutil >/dev/null 2>&1 || { echo "hdiutil is required on macOS for DMG packaging."; exit 1; }
	rm -f "$(MACOS_DMG_PATH)"
	hdiutil create -volname "$(MACOS_APP_NAME)" -srcfolder "$(MACOS_APP_DIR)" -ov -format UDZO "$(MACOS_DMG_PATH)"
	@echo "Built DMG: $(MACOS_DMG_PATH)"

install-macos: build-macos
	rm -rf "$(MACOS_INSTALL_DIR)/$(MACOS_APP_NAME).app"
	cp -R "$(MACOS_APP_DIR)" "$(MACOS_INSTALL_DIR)/$(MACOS_APP_NAME).app"
	@echo "Installed to: $(MACOS_INSTALL_DIR)/$(MACOS_APP_NAME).app"

build-unsupported:
	@echo "Unsupported platform: $(UNAME_S)"
	@exit 1

install-unsupported:
	@echo "Unsupported platform: $(UNAME_S)"
	@exit 1

clean-macos:
	rm -rf "$(MACOS_STAGING_DIR)"

clean-icons:
	rm -rf "$(ICON_TMP_DIR)"
