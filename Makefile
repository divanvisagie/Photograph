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
else ifeq ($(UNAME_S),Darwin)
PLATFORM := macos
else
PLATFORM := unsupported
endif

DEB_DIR := target/deb
PKG_ROOT := $(DEB_DIR)/$(APP_NAME)_$(DEB_VERSION)_$(ARCH)
DEB_PATH := $(DEB_DIR)/$(APP_NAME)_$(DEB_VERSION)_$(ARCH).deb
# Unversioned copy attached to every release, so
# https://github.com/divanvisagie/Photograph/releases/latest/download/photograph_amd64.deb
# always serves the newest .deb.
LATEST_DEB_PATH := $(DEB_DIR)/$(APP_NAME)_$(ARCH).deb
LINUX_DESKTOP_SRC := packaging/linux/$(APP_NAME).desktop
LINUX_ICON_SRC := packaging/linux/$(APP_NAME).svg
LINUX_DESKTOP_DST := $(PKG_ROOT)/usr/share/applications/$(APP_NAME).desktop
LINUX_ICON_DST := $(PKG_ROOT)/usr/share/icons/hicolor/scalable/apps/$(APP_NAME).svg

ICON_TMP_DIR := target/icons

RELEASE_BRANCH := master
# The version `make release` cuts: V=x.y.z if given (it may be the current,
# never-tagged version), else the current one bumped by BUMP (patch, minor
# or major; patch by default).
BUMP ?= patch
NEXT_VERSION := $(or $(V),$(shell echo "$(VERSION)" | awk -F. -v b="$(BUMP)" '{ if (b == "major") print $$1+1 ".0.0"; else if (b == "minor") print $$1 "." $$2+1 ".0"; else if (b == "patch") print $$1 "." $$2 "." $$3+1 }'))
TAG := v$(NEXT_VERSION)
# Optional release notes (Markdown): make release NOTES=path/to/notes.md.
# Written notes come first; GitHub's generated changelog link follows.
NOTES ?=
NOTES_FLAG := $(if $(NOTES),--notes-file "$(NOTES)")

.DEFAULT_GOAL := help

.PHONY: help dev test screenshot build build-linux build-deb build-unsupported install install-linux install-unsupported clean clean-deb clean-icons icons icon-runtime release release-check docs dmg build-macos install-macos

help: ## Show this help
	@echo "Usage: make <target>"
	@echo
	@awk 'BEGIN { FS = ":.*## " } /^[a-z-]+:.*## / { printf "  \033[1m%-14s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

dev: ## Run with live reload (requires cargo-watch)
	@command -v cargo-watch >/dev/null 2>&1 || { echo "cargo-watch is required: cargo install cargo-watch"; exit 1; }
	RUST_LOG=photograph=debug cargo watch -x "run --bin photograph"

test: ## Run the tests
	cargo test

# Throwaway config and data, so screenshots never touch your own settings.
SCREENSHOT_ENV := XDG_CONFIG_HOME=$(CURDIR)/target/screenshot-config XDG_DATA_HOME=$(CURDIR)/target/screenshot-data

screenshot: ## Save target/screenshot-*.png in Dark Orange (the default), Default and Calendar
	@rm -rf target/screenshot-config target/screenshot-data
	@mkdir -p target/screenshot-config target/screenshot-data
	@# Your XDG folder names, so Pictures is still a location.
	-@cp $${XDG_CONFIG_HOME:-$$HOME/.config}/user-dirs.dirs target/screenshot-config/ 2>/dev/null
	$(SCREENSHOT_ENV) PHOTOGRAPH_SCREENSHOT=target/screenshot-dark-orange.png cargo run --bin photograph
	$(SCREENSHOT_ENV) PHOTOGRAPH_SCREENSHOT=target/screenshot-default.png PHOTOGRAPH_SKIN=Default cargo run --bin photograph
	$(SCREENSHOT_ENV) PHOTOGRAPH_SCREENSHOT=target/screenshot-calendar.png PHOTOGRAPH_SKIN=Calendar cargo run --bin photograph

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

clean: ## Remove all build output (fixes cargo run running a stale build)
	cargo clean

clean-deb: ## Remove built .deb artifacts
	rm -rf "$(DEB_DIR)"

# A release is driven from here and finished by .github/workflows/release.yml
# (see ADR-0024). This target bumps the version, tags and pushes, opens a
# draft GitHub release and publishes the crate; the workflow then builds the
# .deb and .dmg on GitHub, attaches them, publishes the release and points
# the website's download links at them.
#
#   make release                 # next patch version
#   make release BUMP=minor      # or major
#   make release V=0.5.0         # a given version (the current one if untagged)
#   make release NOTES=notes.md  # written notes above the generated changelog
#
# The crate is checked with a dry run before anything is pushed, and
# published last: crates.io can't be undone, and if the upload fails the tag
# and draft are already out, so rerun just `cargo publish`.
release: ## Bump, tag and push a release, publish the crate; CI builds the .deb/.dmg (BUMP=minor|major, V=x.y.z, NOTES=file.md)
	git pull --ff-only --quiet origin "$(RELEASE_BRANCH)"
	@$(MAKE) --no-print-directory release-check
	@if [ "$(NEXT_VERSION)" != "$(VERSION)" ]; then \
		sed -i '0,/^version = ".*"/s//version = "$(NEXT_VERSION)"/' Cargo.toml && \
		cargo update --workspace --quiet && \
		git commit --quiet -m "Release $(TAG)" Cargo.toml Cargo.lock && \
		echo "Bumped $(VERSION) -> $(NEXT_VERSION)"; \
	fi
	cargo publish --dry-run --quiet
	git tag -a "$(TAG)" -m "$(TAG)"
	git push --quiet origin "$(RELEASE_BRANCH)" "$(TAG)"
	gh release create "$(TAG)" --draft --title "$(TAG)" $(NOTES_FLAG) --generate-notes --verify-tag
	cargo publish
	@# publish's verify build shares target/debug and leaves the dev build's
	@# fingerprint pointing at target/package's sources, so `cargo run` stops
	@# seeing edits to src/ until this crate's build is cleaned.
	cargo clean -p $(APP_NAME)
	@echo "Released $(APP_NAME) $(NEXT_VERSION) to crates.io and pushed $(TAG)."
	@echo "GitHub is building the .deb and .dmg; the release goes public, and the site's"
	@echo "downloads move to $(TAG), when they're done. Follow it with: gh run watch"

release-check: ## Verify a release can be cut (on master, clean, pushed, version valid and not yet tagged)
	@echo "$(NEXT_VERSION)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$' || { echo "can't release '$(NEXT_VERSION)': use V=x.y.z or BUMP=patch|minor|major"; exit 1; }
	@test -z "$(NOTES)" || test -f "$(NOTES)" || { echo "NOTES file not found: $(NOTES)"; exit 1; }
	@command -v gh >/dev/null 2>&1 || { echo "gh CLI is required: https://cli.github.com"; exit 1; }
	@branch="$$(git rev-parse --abbrev-ref HEAD)"; \
		test "$$branch" = "$(RELEASE_BRANCH)" || { echo "releases are cut from $(RELEASE_BRANCH), but you are on $$branch"; exit 1; }
	@test -z "$$(git status --porcelain)" || { echo "working tree has uncommitted changes"; exit 1; }
	@git fetch --quiet --tags origin "$(RELEASE_BRANCH)"
	@test "$$(git rev-parse HEAD)" = "$$(git rev-parse "origin/$(RELEASE_BRANCH)")" || { echo "HEAD differs from origin/$(RELEASE_BRANCH) — push or pull first"; exit 1; }
	@! git rev-parse -q --verify "refs/tags/$(TAG)" >/dev/null || { echo "$(TAG) is already tagged — release the next version (the default) or pick another with V=x.y.z"; exit 1; }
	@echo "Ready to release $(TAG) from $(RELEASE_BRANCH) at $$(git rev-parse --short HEAD)"

build-macos: dmg

install-macos: dmg
	@echo "Open target/dmg/Photograph-$(VERSION).dmg and drag Photograph to Applications."

dmg: ## Build a universal Photograph.app in a .dmg into target/dmg/ (macOS only)
	@test "$(UNAME_S)" = Darwin || { echo "make dmg runs on macOS (the release workflow builds it on GitHub)"; exit 1; }
	@command -v rsvg-convert >/dev/null 2>&1 || { echo "rsvg-convert is required: brew install librsvg"; exit 1; }
	packaging/macos/build-dmg.sh

build-unsupported:
	@echo "Unsupported platform: $(UNAME_S). Photograph builds on Linux and macOS (see docs/adr/0024-macos-again-released-by-ci.md)."
	@exit 1

install-unsupported:
	@echo "Unsupported platform: $(UNAME_S). Photograph builds on Linux and macOS (see docs/adr/0024-macos-again-released-by-ci.md)."
	@exit 1

clean-icons: ## Remove temporary icon build files
	rm -rf "$(ICON_TMP_DIR)"

docs: ## Serve the docs site at http://localhost:8000
	@command -v python3 >/dev/null 2>&1 || { echo "python3 is required"; exit 1; }
	@echo "Serving docs at http://localhost:8000"
	@cd docs && python3 -m http.server 8000
